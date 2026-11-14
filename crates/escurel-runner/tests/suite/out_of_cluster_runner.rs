//! An out-of-cluster runner (#828): a static `escurel:runner` credential and
//! NO signing key. It claims inbox work, registers its runtime, reports its
//! status as `role: runner`, and gets each run's token from the gateway
//! (`mint_run_token`) instead of minting it — so an agent-profile skill runs
//! as its agent, confined by the agent's `scopes.write` exactly as in minted
//! mode, and the run token carries the same claims a minting runner signs.
//!
//! Real gateway (`ESCUREL_WRITE_ACL=enforce`, `ESCUREL_EVENT_ACL=enforce`,
//! signing on the test issuer's key), real `escurel-runner` on the echo
//! harness. No mocks.

use std::time::Duration;

use escurel_test_support::{
    AuthMode, ConfigOverrides, EscurelProcess, EventAclMode, FixtureBuilder, Opts, Role,
    WriteAclMode,
};
use serde_json::{Value, json};

use crate::support::{GatewayProxy, ProxyRule, RunnerProcess, call};

const TENANT: &str = "acme";

/// Both grant `ops` and default to `scribe`, so only scribe's write scope can
/// refuse the customer fold.
fn skill(id: &str) -> String {
    format!(
        "---\nkind: skill\nid: {id}\nautonomy: auto\nagent: \"[[agent::scribe]]\"\n\
         acl:\n  create: [ops]\n  update: [ops]\n---\n# {id}\n\nFold the event into the instance.\n"
    )
}

/// A skill no agent lists, granting a group no runner holds.
const PAYROLL: &str = "---\nkind: skill\nid: payroll\nautonomy: auto\n\
    acl:\n  create: [finance]\n  update: [finance]\n---\n# payroll\n\nPay people.\n";

const SCRIBE: &str = "---\nkind: instance\nskill: agent\nid: scribe\nharness: echo\n\
    skills: [\"[[skill::customer]]\", \"[[skill::note]]\"]\n\
    scopes:\n  write: [\"markdown/instances/note/*\"]\n---\n# scribe\n";

fn instance_body(skill: &str) -> String {
    format!("---\nkind: instance\nid: c1\nskill: {skill}\n---\n# C1\n\nBASELINE.\n")
}

fn page_id(skill: &str) -> String {
    format!("markdown/instances/{skill}/c1.md")
}

/// A skill with no agent: an ordinary run, on the skill's own write groups.
const PLAIN: &str = "---\nkind: skill\nid: plain\nautonomy: auto\n\
    acl:\n  create: [ops]\n  update: [ops]\n---\n# plain\n\nFold the event into the instance.\n";

async fn gateway() -> EscurelProcess {
    gateway_signing(true).await
}

/// `signing`: whether the gateway holds a signing key (and so can mint run tokens).
async fn gateway_signing(signing: bool) -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            write_acl: Some(WriteAclMode::Enforce),
            event_acl: Some(EventAclMode::Enforce),
            signing,
            ..Default::default()
        },
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("customer", skill("customer").as_str())
                .skill("note", skill("note").as_str())
                .skill("payroll", PAYROLL)
                .skill("plain", PLAIN)
                .instance("customer", "c1", instance_body("customer").as_str())
                .instance("note", "c1", instance_body("note").as_str())
                .instance("plain", "c1", instance_body("plain").as_str())
                .instance("agent", "scribe", SCRIBE)
                .done(),
        ),
    })
    .await
}

/// The body of `run:<run_id>:<suffix>`.
async fn run_event(gw: &EscurelProcess, run_id: &str, suffix: &str) -> Value {
    let found = call(
        gw,
        &admin(gw),
        "list_events",
        json!({ "event_id": format!("run:{run_id}:{suffix}") }),
    )
    .await;
    let event = &found["events"][0];
    assert!(event.is_object(), "no run:{run_id}:{suffix}: {found}");
    crate::support::body_of(event)
}

fn admin(gw: &EscurelProcess) -> String {
    gw.mint_token(TENANT, Role::Admin)
}

/// The narrow runner credential an out-of-cluster runner holds.
fn runner_token(gw: &EscurelProcess, sub: &str) -> String {
    gw.mint_token_with_groups(TENANT, sub, &["escurel:runner"], false)
}

async fn rpc(gw: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    reqwest::Client::new()
        .post(gw.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": { "name": name, "arguments": args } }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json")
}

async fn capture(gw: &EscurelProcess, skill: &str) -> String {
    let r = call(
        gw,
        &admin(gw),
        "capture_event",
        json!({ "source": "manual", "mime": "text/plain", "label_skill": skill,
                "instance_page_id": page_id(skill), "title": "work item",
                "body": format!("ECHO_FOLD_MARKER work for {skill}") }),
    )
    .await;
    r["event_id"].as_str().unwrap().to_owned()
}

/// File `run:<run_id>:started` for `skill` as the runner holding `token`
/// does before it asks for the run's token.
async fn start_run(gw: &EscurelProcess, token: &str, run_id: &str, skill: &str) {
    let r = rpc(
        gw,
        token,
        "capture_event",
        json!({ "event_id": format!("run:{run_id}:started"), "source": "escurel-runner",
                "mime": "application/json", "label_skill": "escurel:run", "kind": "system",
                "title": "run-started", "body": "{}",
                "provenance": { "runner": { "run_id": run_id, "skill": skill } } }),
    )
    .await;
    assert!(r.get("error").is_none(), "run-started filed: {r}");
}

fn claims_of(token: &str) -> Value {
    use base64::Engine as _;
    let payload = token.split('.').nth(1).expect("jwt");
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .expect("b64");
    serde_json::from_slice(&bytes).expect("json")
}

#[tokio::test]
async fn a_runner_with_only_the_runner_role_runs_an_agent_skill_confined_by_its_scopes() {
    let gw = gateway().await;
    let mut runner = RunnerProcess::echo(&gw, TENANT)
        .static_bearer(runner_token(&gw, "runner:ooc"))
        .env("ESCUREL_RUNNER_ID", "ooc")
        .env("ESCUREL_RUNNER_MAX_ATTEMPTS", "1")
        .env("ESCUREL_RUNNER_STATUS_INTERVAL", "1s")
        .spawn();
    runner.wait_healthy().await;

    // In scope: the note fold lands, written as the agent.
    let note = capture(&gw, "note").await;
    let done = runner
        .wait_for_status(&note, &["processed"], Duration::from_secs(60))
        .await;
    let run_id = done["run_id"].as_str().unwrap().to_owned();
    let page = call(
        &gw,
        &admin(&gw),
        "expand",
        json!({ "page_id": page_id("note") }),
    )
    .await;
    assert!(
        page["body"].as_str().unwrap().contains("folded event"),
        "{page}"
    );
    assert_eq!(page["page"]["last_written_by"], "agent:scribe", "{page}");
    let calls = call(
        &gw,
        &admin(&gw),
        "get_run_tool_calls",
        json!({ "run_id": run_id }),
    )
    .await;
    assert!(
        calls["calls"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["tool"] == "update_page" && c["status"] == "ok"),
        "the write was made on the run's own token: {calls}"
    );

    // Out of scope: the gateway refuses the customer fold, on the run's record.
    let customer = capture(&gw, "customer").await;
    let done = runner
        .wait_for_status(
            &customer,
            &["failed", "dead_letter"],
            Duration::from_secs(60),
        )
        .await;
    let run_id = done["run_id"].as_str().unwrap().to_owned();
    let calls = call(
        &gw,
        &admin(&gw),
        "get_run_tool_calls",
        json!({ "run_id": run_id }),
    )
    .await;
    let refused = calls["calls"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["tool"] == "update_page")
        .unwrap_or_else(|| panic!("the refused write is on the run's record: {calls}"));
    assert_eq!(refused["error_code"], "agent_scope_denied", "{calls}");

    // It registered its runtime and reports as a runner.
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        let rts = call(&gw, &admin(&gw), "list_runtimes", json!({})).await;
        let rt = rts["runtimes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["runner_id"] == "ooc")
            .cloned();
        if let Some(rt) = rt.filter(|r| r["status"] == "online" && r["role"] == "runner") {
            assert!(
                rt["claim_holder"]
                    .as_str()
                    .is_some_and(|h| h.starts_with("runner:ooc#")),
                "{rt}"
            );
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no online runtime with role runner: {rts}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// `mint_run_token` signs what a minting runner signs (`sub`, `act.sub`,
/// `run_id`, `root_event_id`, `skill`, `agent`, narrowed roles), never the
/// admin shape for a runner, and refuses a run another holder started, a
/// machine caller and a member.
#[tokio::test]
async fn mint_run_token_signs_the_minted_shape_and_refuses_foreign_runs_and_machines() {
    let gw = gateway().await;
    let ooc = runner_token(&gw, "runner:ooc");
    start_run(&gw, &ooc, "01RUNA", "note").await;
    start_run(&gw, &ooc, "01RUNB", "note").await;
    let minted = call(
        &gw,
        &ooc,
        "mint_run_token",
        json!({ "run_id": "01RUNA", "root_event_id": "01ROOT", "trace_id": "t-1",
                "skill": "note", "agent": "scribe", "ttl_secs": 600 }),
    )
    .await;
    let c = claims_of(minted["token"].as_str().unwrap());
    assert_eq!(c["sub"], "agent:scribe", "{c}");
    assert_eq!(c["act"]["sub"], "runner:ooc", "{c}");
    assert_eq!(c["run_id"], "01RUNA", "{c}");
    assert_eq!(c["root_event_id"], "01ROOT", "{c}");
    assert_eq!(c["trace_id"], "t-1", "{c}");
    assert_eq!(c["skill"], "note", "{c}");
    assert_eq!(c["agent"], "scribe", "{c}");
    assert_eq!(c["roles"], json!(["escurel:agent", "ops"]), "{c}");
    assert_eq!(minted["subject"], "agent:scribe", "{minted}");

    // A runner asking for the wide shape gets the narrowed one; an admin gets wide.
    let wide = call(
        &gw,
        &ooc,
        "mint_run_token",
        json!({ "run_id": "01RUNB", "root_event_id": "01RUNB", "skill": "note",
                "grant": "wide", "ttl_secs": 600 }),
    )
    .await;
    let c = claims_of(wide["token"].as_str().unwrap());
    assert_eq!(c["sub"], "agent:note", "{c}");
    assert_eq!(c["roles"], json!(["escurel:agent", "ops"]), "{c}");
    assert_eq!(c["skill"], "note", "{c}");
    let by_admin = call(
        &gw,
        &admin(&gw),
        "mint_run_token",
        json!({ "run_id": "01RUNC", "root_event_id": "01RUNC", "skill": "note",
                "grant": "wide", "ttl_secs": 600 }),
    )
    .await;
    let c = claims_of(by_admin["token"].as_str().unwrap());
    assert_eq!(c["roles"], json!(["escurel:admin"]), "{c}");
    assert!(c.get("skill").is_none(), "{c}");

    // A run another holder started is not this caller's to mint for.
    start_run(&gw, &ooc, "01RUND", "note").await;
    let other = runner_token(&gw, "runner:other");
    let body = rpc(
        &gw,
        &other,
        "mint_run_token",
        json!({ "run_id": "01RUND", "root_event_id": "01RUND", "skill": "note",
                "ttl_secs": 600 }),
    )
    .await;
    assert_eq!(body["error"]["data"]["code"], "run_not_yours", "{body}");
    let own = rpc(
        &gw,
        &ooc,
        "mint_run_token",
        json!({ "run_id": "01RUND", "root_event_id": "01RUND", "skill": "note",
                "ttl_secs": 600 }),
    )
    .await;
    assert!(own.get("error").is_none(), "its own run: {own}");

    // A machine caller (even an admin-role run token) and a member are refused; an unknown agent too.
    let machine = gw.mint_token_for_run(TENANT, Role::Admin, "agent:note", "01RUNE", "01RUNE");
    let body = rpc(
        &gw,
        &machine,
        "mint_run_token",
        json!({ "run_id": "01RUNF", "skill": "note", "ttl_secs": 600 }),
    )
    .await;
    assert_eq!(
        body["error"]["data"]["code"], "agent_scope_denied",
        "{body}"
    );
    let member = gw.mint_token_with_sub(TENANT, Role::Agent, "member:alice");
    let body = rpc(
        &gw,
        &member,
        "mint_run_token",
        json!({ "run_id": "01RUNG", "skill": "note", "ttl_secs": 600 }),
    )
    .await;
    assert_eq!(body["error"]["data"]["code"], "admin_required", "{body}");
    start_run(&gw, &ooc, "01RUNH", "note").await;
    let body = rpc(
        &gw,
        &ooc,
        "mint_run_token",
        json!({ "run_id": "01RUNH", "skill": "note", "agent": "ghost", "ttl_secs": 600 }),
    )
    .await;
    assert_eq!(body["error"]["data"]["code"], "agent_unknown", "{body}");
}

/// `mint_run_token` is the trust boundary of a keyless runner: the token it
/// signs carries the skill's write groups, so a runner credential gets one
/// only for a run it started itself, of that very skill, as an agent that
/// lists the skill. An admin may mint for any run it has not left to another.
#[tokio::test]
async fn mint_run_token_mints_only_for_the_callers_own_run_of_a_listed_skill() {
    let gw = gateway().await;
    let ooc = runner_token(&gw, "runner:ooc");
    let mint = |run: &str, skill: &str, agent: Option<&str>| {
        let mut args = json!({ "run_id": run, "skill": skill, "ttl_secs": 600 });
        if let Some(a) = agent {
            args["agent"] = json!(a);
        }
        args
    };
    let code = |body: &Value| body["error"]["data"]["code"].clone();

    // A run the caller never started: nothing to mint for.
    let body = rpc(&gw, &ooc, "mint_run_token", mint("01NONE", "payroll", None)).await;
    assert_eq!(code(&body), "run_unknown", "{body}");

    // Its own run of `note` does not reach `payroll`'s groups.
    start_run(&gw, &ooc, "01NOTE", "note").await;
    let body = rpc(&gw, &ooc, "mint_run_token", mint("01NOTE", "payroll", None)).await;
    assert_eq!(code(&body), "run_not_yours", "{body}");

    // A run another runner started, of the very skill asked for.
    let other = runner_token(&gw, "runner:other");
    start_run(&gw, &other, "01PAY", "payroll").await;
    let body = rpc(&gw, &ooc, "mint_run_token", mint("01PAY", "payroll", None)).await;
    assert_eq!(code(&body), "run_not_yours", "{body}");

    // An agent that does not list the skill, even on the caller's own run.
    start_run(&gw, &ooc, "01OWNPAY", "payroll").await;
    let body = rpc(
        &gw,
        &ooc,
        "mint_run_token",
        mint("01OWNPAY", "payroll", Some("scribe")),
    )
    .await;
    assert_eq!(code(&body), "agent_scope_denied", "{body}");

    // A skill no page declares: never a groupless token, not even for an admin.
    start_run(&gw, &ooc, "01GHOST", "ghost-skill").await;
    let body = rpc(
        &gw,
        &ooc,
        "mint_run_token",
        mint("01GHOST", "ghost-skill", None),
    )
    .await;
    assert_eq!(code(&body), "skill_unknown", "{body}");
    let body = rpc(
        &gw,
        &admin(&gw),
        "mint_run_token",
        mint("01ADMINGHOST", "ghost-skill", None),
    )
    .await;
    assert_eq!(code(&body), "skill_unknown", "{body}");

    // The happy path: its own run of payroll, no agent.
    let ok = call(
        &gw,
        &ooc,
        "mint_run_token",
        mint("01OWNPAY", "payroll", None),
    )
    .await;
    let c = claims_of(ok["token"].as_str().unwrap());
    assert_eq!(c["roles"], json!(["escurel:agent", "finance"]), "{c}");
    assert_eq!(c["skill"], "payroll", "{c}");
}

/// A revoke ends the agent's run tokens too, from the next call on — one
/// minted before it is `agent_token_revoked` — and a run token minted after
/// it, within the same second, stands (its `iat` is raised to the epoch).
#[tokio::test]
async fn a_revoke_ends_a_run_token_minted_before_it_and_a_later_one_stands() {
    let gw = gateway().await;
    let ooc = runner_token(&gw, "runner:ooc");
    start_run(&gw, &ooc, "01REV", "note").await;
    let args = json!({ "run_id": "01REV", "skill": "note", "agent": "scribe", "ttl_secs": 600 });
    let before = call(&gw, &ooc, "mint_run_token", args.clone()).await["token"]
        .as_str()
        .unwrap()
        .to_owned();
    let body = rpc(&gw, &before, "whoami", json!({})).await;
    assert!(body.get("error").is_none(), "{body}");

    call(
        &gw,
        &admin(&gw),
        "revoke_agent_tokens",
        json!({ "agent": "scribe" }),
    )
    .await;
    let body = rpc(&gw, &before, "whoami", json!({})).await;
    assert_eq!(
        body["error"]["data"]["code"], "agent_token_revoked",
        "{body}"
    );

    let after = call(&gw, &ooc, "mint_run_token", args).await["token"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(after, before);
    let body = rpc(&gw, &after, "whoami", json!({})).await;
    assert!(
        body.get("error").is_none(),
        "minted after the revoke: {body}"
    );
}

/// The runner asks for a fresh run token on every attempt: an agent revoked
/// while attempt 1 is in flight (which then times out) runs attempt 2 on a
/// new bearer that the revoke does not reach, instead of retrying the dead one.
#[tokio::test]
async fn a_revoke_between_attempts_runs_the_next_attempt_on_a_fresh_token() {
    let gw = gateway().await;
    let dir = tempfile::tempdir().expect("tempdir");
    let marker = dir.path().join("slept");
    let mut runner = RunnerProcess::echo(&gw, TENANT)
        .static_bearer(runner_token(&gw, "runner:ooc"))
        .env("ESCUREL_RUNNER_ID", "ooc")
        .env("ESCUREL_RUNNER_MAX_ATTEMPTS", "3")
        .env("ESCUREL_RUNNER_RUN_TIMEOUT", "3s")
        .env("ESCUREL_RUNNER_RETRY_BACKOFF", "100ms")
        .env("ESCUREL_ECHO_SLEEP_MS", "8000")
        .env("ESCUREL_ECHO_SLEEP_ONCE", marker.to_str().unwrap())
        .spawn();
    runner.wait_healthy().await;

    let note = capture(&gw, "note").await;
    // Attempt 1 is asleep in the harness: its token was minted before it started.
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while !marker.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "attempt 1 never reached the harness"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    call(
        &gw,
        &admin(&gw),
        "revoke_agent_tokens",
        json!({ "agent": "scribe" }),
    )
    .await;

    let done = runner
        .wait_for_status(&note, &["processed"], Duration::from_secs(60))
        .await;
    let run_id = done["run_id"].as_str().unwrap().to_owned();
    let first = run_event(&gw, &run_id, "attempt:1").await;
    assert_eq!(first["outcome"], "timeout", "attempt 1 timed out: {first}");
    let page = call(
        &gw,
        &admin(&gw),
        "expand",
        json!({ "page_id": page_id("note") }),
    )
    .await;
    assert!(
        page["body"].as_str().unwrap().contains("folded event"),
        "{page}"
    );
    assert_eq!(page["page"]["last_written_by"], "agent:scribe", "{page}");
}

/// A gateway that FAILS a mint (a 5xx, a timeout, a refusal) is not a gateway
/// that cannot mint: the attempt fails and is retried, and the harness never
/// gets the runner's own bearer — here an admin's, which would have folded the
/// event with the runner's full authority.
#[tokio::test]
async fn a_failed_mint_fails_the_attempt_and_never_runs_as_the_runner() {
    let gw = gateway().await;
    let proxy = GatewayProxy::start(
        gw.base_url(),
        ProxyRule {
            tool: "mint_run_token",
            delay: Duration::ZERO,
            fail_with: Some(503),
        },
    )
    .await;
    let mut runner = RunnerProcess::echo(&gw, TENANT)
        .static_bearer(gw.mint_token_with_sub(TENANT, Role::Admin, "runner:hq"))
        .env("ESCUREL_RUNNER_GATEWAY_URL", &proxy.url)
        .env("ESCUREL_RUNNER_MAX_ATTEMPTS", "2")
        .env("ESCUREL_RUNNER_RETRY_BACKOFF", "100ms")
        .spawn();
    runner.wait_healthy().await;

    let plain = capture(&gw, "plain").await;
    let done = runner
        .wait_for_status(&plain, &["failed", "dead_letter"], Duration::from_secs(60))
        .await;
    let run_id = done["run_id"].as_str().unwrap().to_owned();
    for attempt in ["attempt:1", "attempt:2"] {
        let a = run_event(&gw, &run_id, attempt).await;
        assert!(
            a["error"].as_str().is_some_and(|e| e.contains("mint")),
            "{attempt} failed on the mint, and was retried: {a}"
        );
    }
    let page = call(
        &gw,
        &admin(&gw),
        "expand",
        json!({ "page_id": page_id("plain") }),
    )
    .await;
    assert!(
        page["body"].as_str().unwrap().contains("BASELINE"),
        "{page}"
    );
    assert!(
        !page["body"].as_str().unwrap().contains("folded event"),
        "{page}"
    );
}

/// Against a gateway with NO signing key, a keyless runner falls back the
/// documented way: an ordinary run writes as the runner's own subject, and an
/// agent run is refused with a reason that names the fix — never run on the
/// runner's own credential.
#[tokio::test]
async fn without_a_gateway_signing_key_ordinary_runs_write_as_the_runner_and_agent_runs_refuse() {
    let gw = gateway_signing(false).await;
    let mut runner = RunnerProcess::echo(&gw, TENANT)
        .static_bearer(gw.mint_token_with_groups(
            TENANT,
            "runner:ooc",
            &["escurel:runner", "ops"],
            false,
        ))
        .env("ESCUREL_RUNNER_MAX_ATTEMPTS", "1")
        .spawn();
    runner.wait_healthy().await;

    let plain = capture(&gw, "plain").await;
    runner
        .wait_for_status(&plain, &["processed"], Duration::from_secs(60))
        .await;
    let page = call(
        &gw,
        &admin(&gw),
        "expand",
        json!({ "page_id": page_id("plain") }),
    )
    .await;
    assert!(
        page["body"].as_str().unwrap().contains("folded event"),
        "{page}"
    );
    assert_eq!(page["page"]["last_written_by"], "runner:ooc", "{page}");

    let note = capture(&gw, "note").await;
    let done = runner
        .wait_for_status(&note, &["failed", "dead_letter"], Duration::from_secs(60))
        .await;
    let run_id = done["run_id"].as_str().unwrap().to_owned();
    let finished = run_event(&gw, &run_id, "finished").await;
    assert!(
        finished["error"]
            .as_str()
            .is_some_and(|e| e.contains("scribe") && e.contains("ESCUREL_AUTH_SIGNING_KEY")),
        "{finished}"
    );
    let page = call(
        &gw,
        &admin(&gw),
        "expand",
        json!({ "page_id": page_id("note") }),
    )
    .await;
    assert!(
        page["body"].as_str().unwrap().contains("BASELINE"),
        "{page}"
    );
}
