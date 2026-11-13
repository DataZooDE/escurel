//! Agent profiles (#823): least privilege on the per-run token.
//!
//! A skill names a default agent (`agent: "[[agent::scribe]]"`); the runner
//! resolves it, mints the run's token as `agent:scribe` with the `agent` claim
//! and `escurel:agent` — never `escurel:admin` — and the GATEWAY reads the
//! agent page itself to enforce its scopes. A run of scribe may write notes
//! only, so its fold into a customer is refused by the gateway, the refusal is
//! on the run's tool-call record, and the run does not land.
//!
//! Real gateway under `ESCUREL_WRITE_ACL=enforce`, real runner in MINTED mode
//! (no `ESCUREL_RUNNER_AGENT_NARROW`: the agent narrows on its own), real echo
//! harness.

use std::process::{Child, Command};
use std::time::{Duration, Instant};

use escurel_test_support::{
    AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role, WriteAclMode, free_port,
};
use serde_json::{Value, json};

const TENANT: &str = "acme";

/// Both skills grant `ops` and default to `scribe`, so the ONLY thing that can
/// refuse the customer fold is scribe's write scope.
fn skill(id: &str) -> String {
    format!(
        "---\nkind: skill\nid: {id}\nautonomy: auto\nagent: \"[[agent::scribe]]\"\n\
         acl:\n  create: [ops]\n  update: [ops]\n---\n# {id}\n\nFold the event into the instance.\n"
    )
}

/// Grants no write at all (admin-only under the tenant default), but sits in
/// scribe's write scope: only scribe NOT carrying admin can refuse it.
const MEMO: &str = "---\nkind: skill\nid: memo\nautonomy: auto\nagent: \"[[agent::scribe]]\"\n---\n\
    # memo\n\nFold the event into the instance.\n";
/// Defaults to an agent that does not exist: refused, never run wide.
const ORPHAN: &str = "---\nkind: skill\nid: orphan\nautonomy: auto\nagent: \"[[agent::ghost]]\"\n---\n\
    # orphan\n\nFold the event into the instance.\n";
const SCRIBE: &str = "---\nkind: instance\nskill: agent\nid: scribe\nharness: echo\n\
    skills: [\"[[skill::customer]]\", \"[[skill::note]]\", \"[[skill::memo]]\"]\n\
    scopes:\n  write: [\"markdown/instances/note/*\", \"markdown/instances/memo/*\"]\n\
    max_concurrent: 1\n---\n# scribe\n";
/// A reader: may expand, may not search.
const READER: &str = "---\nkind: instance\nskill: agent\nid: reader\n\
    skills: [\"[[skill::note]]\"]\nscopes:\n  tools: [expand, whoami, update_page]\n---\n# reader\n";

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn instance_body(skill: &str) -> String {
    format!("---\nkind: instance\nid: c1\nskill: {skill}\n---\n# C1\n\nBASELINE.\n")
}

fn page_id(skill: &str) -> String {
    format!("markdown/instances/{skill}/c1.md")
}

async fn rpc(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    reqwest::Client::new()
        .post(p.mcp_url())
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

async fn call(p: &EscurelProcess, name: &str, args: Value) -> Value {
    let body = rpc(p, &p.mint_token(TENANT, Role::Admin), name, args).await;
    assert!(body.get("error").is_none(), "{name}: {body}");
    body["result"]["structuredContent"].clone()
}

async fn gateway() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            write_acl: Some(WriteAclMode::Enforce),
            ..Default::default()
        },
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("customer", skill("customer").as_str())
                .skill("note", skill("note").as_str())
                .skill("memo", MEMO)
                .skill("orphan", ORPHAN)
                .instance("orphan", "c1", instance_body("orphan").as_str())
                .instance("memo", "c1", instance_body("memo").as_str())
                .instance("customer", "c1", instance_body("customer").as_str())
                .instance("note", "c1", instance_body("note").as_str())
                .instance("agent", "scribe", SCRIBE)
                .instance("agent", "reader", READER)
                .done(),
        ),
    })
    .await
}

/// A minted-mode runner driving the echo harness. Kept local on purpose:
/// another slice extracts a shared runner builder for the suite.
fn spawn_runner(gw: &EscurelProcess) -> (ChildGuard, String) {
    let (signing_key, kid) = gw.signing_material();
    let listen = format!("127.0.0.1:{}", free_port());
    let ledger_dir = tempfile::tempdir().expect("tempdir");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gw.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env_remove("ESCUREL_RUNNER_TOKEN")
        .env_remove("ESCUREL_RUNNER_AGENT_NARROW")
        .env("ESCUREL_RUNNER_AUTH_ISSUER", gw.issuer_url())
        .env("ESCUREL_RUNNER_AUTH_KID", kid)
        .env("ESCUREL_RUNNER_AUTH_SIGNING_KEY", &signing_key)
        .env("ESCUREL_RUNNER_HARNESS", "echo")
        .env(
            "ESCUREL_RUNNER_LEDGER_PATH",
            ledger_dir.keep().join("ledger.duckdb"),
        )
        .env("ESCUREL_RUNNER_MAX_ATTEMPTS", "1")
        .env("ESCUREL_RUNNER_RETRY_BACKOFF", "50ms")
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms");
    (ChildGuard(cmd.spawn().expect("spawn runner")), listen)
}

async fn capture(gw: &EscurelProcess, skill: &str) -> String {
    let r = call(
        gw,
        "capture_event",
        json!({ "source": "manual", "mime": "text/plain", "label_skill": skill,
                "instance_page_id": page_id(skill), "title": "work item",
                "body": format!("ECHO_FOLD_MARKER work for {skill}") }),
    )
    .await;
    r["event_id"].as_str().unwrap().to_owned()
}

/// `(status, run_id)` of the ledger terminal for `event_id`.
async fn await_terminal(runner: &mut ChildGuard, listen: &str, event_id: &str) -> (String, String) {
    let url = format!("http://{listen}/debug/run?tenant={TENANT}&event_id={event_id}");
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(status) = runner.0.try_wait().expect("try_wait") {
            panic!("runner at {listen} exited before the run reached a terminal: {status:?}");
        }
        let last = match reqwest::get(&url).await {
            Ok(resp) => match resp.json::<Value>().await {
                Ok(v) => {
                    if let Some(status) = v["status"].as_str()
                        && status != "pending"
                    {
                        return (status.to_owned(), v["run_id"].as_str().unwrap().to_owned());
                    }
                    v.to_string()
                }
                Err(e) => format!("not JSON: {e}"),
            },
            Err(e) => format!("request failed: {e}"),
        };
        assert!(
            Instant::now() < deadline,
            "run never reached a terminal; the last answer from {listen} was {last}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn writer_of(gw: &EscurelProcess, skill: &str) -> (bool, String) {
    let e = call(gw, "expand", json!({ "page_id": page_id(skill) })).await;
    let folded = e["body"]
        .as_str()
        .unwrap_or_default()
        .contains("folded event");
    let writer = e["page"]["last_written_by"].as_str().unwrap_or_default();
    (folded, writer.to_owned())
}

/// The run's `agent` group in `usage_report`, polled: `run-finished` is
/// written after the ledger terminal the test waited on.
async fn agent_runs(gw: &EscurelProcess, want: u64) -> Vec<(String, u64)> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let r = call(gw, "usage_report", json!({ "group_by": ["agent"] })).await;
        let rows: Vec<(String, u64)> = r["rows"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|row| {
                (
                    row["group"]["agent"].as_str().unwrap_or("").to_owned(),
                    row["runs"].as_u64().unwrap_or(0),
                )
            })
            .collect();
        if rows.iter().map(|(_, n)| n).sum::<u64>() >= want || Instant::now() > deadline {
            return rows;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test]
async fn an_agent_scoped_to_notes_is_refused_a_customer_write_by_the_gateway() {
    let gw = gateway().await;
    let (mut runner, listen) = spawn_runner(&gw);

    // (a) scribe runs `customer` (it lists it) but may write only notes.
    let e1 = capture(&gw, "customer").await;
    let (status, run) = await_terminal(&mut runner, &listen, &e1).await;
    assert_ne!(
        status, "processed",
        "a notes-only agent must not land a customer write"
    );
    let (folded, _) = writer_of(&gw, "customer").await;
    assert!(!folded, "the customer instance must be untouched");
    let calls = call(&gw, "get_run_tool_calls", json!({ "run_id": run })).await;
    let refused = calls["calls"]
        .as_array()
        .expect("calls")
        .iter()
        .find(|c| c["tool"] == "update_page")
        .unwrap_or_else(|| panic!("the refused write is on the run's record: {calls}"));
    assert_eq!(refused["status"], "rejected", "{calls}");
    assert_eq!(
        refused["error_code"], "agent_scope_denied",
        "refused by the agent's scope, not by the skill ACL: {calls}"
    );

    // (b) the same agent's note write lands, as the agent.
    let e2 = capture(&gw, "note").await;
    assert_eq!(
        await_terminal(&mut runner, &listen, &e2).await.0,
        "processed"
    );
    let (folded, writer) = writer_of(&gw, "note").await;
    assert!(folded, "the note instance was not written");
    assert_eq!(writer, "agent:scribe", "the run writes as its agent");

    // The token is `escurel:agent`, never the runner's admin: a skill that
    // grants no write stays closed to scribe even inside its write scope.
    let e3 = capture(&gw, "memo").await;
    let (status, run) = await_terminal(&mut runner, &listen, &e3).await;
    assert_ne!(status, "processed", "an agent token must not carry admin");
    let calls = call(&gw, "get_run_tool_calls", json!({ "run_id": run })).await;
    assert!(
        calls["calls"]
            .as_array()
            .expect("calls")
            .iter()
            .any(|c| c["tool"] == "update_page" && c["error_code"] == "forbidden"),
        "{calls}"
    );

    // A skill whose default agent is missing does not run with a broader token.
    let e4 = capture(&gw, "orphan").await;
    assert_ne!(
        await_terminal(&mut runner, &listen, &e4).await.0,
        "processed"
    );
    assert!(
        !writer_of(&gw, "orphan").await.0,
        "orphan's instance must be untouched"
    );

    // (f) every scribe run is scribe's in the usage report.
    let rows = agent_runs(&gw, 4).await;
    assert!(rows.contains(&("agent:scribe".to_owned(), 3)), "{rows:?}");
}

#[tokio::test]
async fn a_token_naming_an_unknown_agent_is_refused_every_tool() {
    let gw = gateway().await;
    let forged = gw.mint_token_for_agent(TENANT, "ghost", "note", &["ops"]);
    for (tool, args) in [
        ("whoami", json!({})),
        ("expand", json!({ "page_id": page_id("note") })),
        ("search", json!({ "q": "baseline" })),
    ] {
        let body = rpc(&gw, &forged, tool, args).await;
        assert_eq!(
            body["error"]["data"]["code"], "agent_unknown",
            "{tool} must be refused for a missing agent: {body}"
        );
    }
}

#[tokio::test]
async fn an_agents_tool_scope_refuses_what_it_does_not_list() {
    let gw = gateway().await;
    let token = gw.mint_token_for_agent(TENANT, "reader", "note", &["ops"]);
    let body = rpc(&gw, &token, "search", json!({ "q": "baseline" })).await;
    assert_eq!(
        body["error"]["data"]["code"], "agent_scope_denied",
        "{body}"
    );
    let body = rpc(&gw, &token, "expand", json!({ "page_id": page_id("note") })).await;
    assert!(body.get("error").is_none(), "expand is in scope: {body}");

    // Filing an event into a record outside the write scope is a write too.
    let scribe = gw.mint_token_for_agent(TENANT, "scribe", "customer", &["ops"]);
    for (tool, args) in [
        (
            "capture_event",
            json!({ "source": "manual", "mime": "text/plain", "label_skill": "customer",
                    "instance_page_id": page_id("customer"), "title": "t", "body": "b" }),
        ),
        (
            "assign_event",
            json!({ "event_id": "nope", "instance_page_id": page_id("customer") }),
        ),
    ] {
        let body = rpc(&gw, &scribe, tool, args).await;
        assert_eq!(
            body["error"]["data"]["code"], "agent_scope_denied",
            "{tool}: {body}"
        );
    }

    // (e) an agent cannot rewrite agent pages — its own included.
    let widened = READER.replace("tools: [expand, whoami, update_page]", "tools: [search]");
    let body = rpc(
        &gw,
        &token,
        "update_page",
        json!({ "page_id": "markdown/instances/agent/reader.md", "content": widened }),
    )
    .await;
    assert_eq!(
        body["result"]["structuredContent"]["ok"], false,
        "an agent must not widen its own scopes: {body}"
    );
}

async fn list_tools(gw: &EscurelProcess, token: &str) -> Value {
    reqwest::Client::new()
        .post(gw.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json")
}

fn tool_names(body: &Value) -> Vec<String> {
    body["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("a tool list: {body}"))
        .iter()
        .filter_map(|t| t["name"].as_str().map(str::to_owned))
        .collect()
}

/// The listing matches what the gateway will let the agent call, so a harness
/// is never offered a tool every call to which is refused.
#[tokio::test]
async fn tools_list_offers_an_agent_only_its_scoped_tools() {
    let gw = gateway().await;
    let reader = gw.mint_token_for_agent(TENANT, "reader", "note", &["ops"]);
    let mut names = tool_names(&list_tools(&gw, &reader).await);
    names.sort();
    assert_eq!(
        names,
        ["expand", "update_page", "whoami"],
        "reader's scopes.tools"
    );

    // An agent with no `scopes.tools` sees what its role sees.
    let scribe = gw.mint_token_for_agent(TENANT, "scribe", "note", &["ops"]);
    let plain = gw.mint_token(TENANT, Role::Agent);
    assert_eq!(
        tool_names(&list_tools(&gw, &scribe).await),
        tool_names(&list_tools(&gw, &plain).await)
    );

    // A token naming no agent page lists nothing: it may call nothing.
    let forged = gw.mint_token_for_agent(TENANT, "ghost", "note", &["ops"]);
    let body = list_tools(&gw, &forged).await;
    assert_eq!(body["error"]["data"]["code"], "agent_unknown", "{body}");
}

/// Crash recovery attributes a reconciled run to the agent a fresh run of the
/// same event would execute as — the event's `provenance.agent` request, else
/// the skill's default — through the same resolution, so `usage_report`
/// counts it against that agent rather than an anonymous recovery.
#[tokio::test]
async fn recovery_resolves_the_agent_a_fresh_run_would() {
    use escurel_runner_core::{Ledger, LedgerDecision, Trigger};

    let gw = gateway().await;
    // `note` defaults to scribe; the event asks for scribe explicitly too, so
    // either source names it — the point is that recovery reads them at all.
    let r = call(
        &gw,
        "capture_event",
        json!({ "source": "manual", "mime": "text/plain", "label_skill": "note",
                "instance_page_id": page_id("note"), "title": "t", "body": "b",
                "provenance": { "agent": "[[agent::scribe]]" } }),
    )
    .await;
    let event_id = r["event_id"].as_str().unwrap().to_owned();
    call(
        &gw,
        "assign_event",
        json!({ "event_id": event_id, "instance_page_id": page_id("note") }),
    )
    .await;
    let events = call(&gw, "list_events", json!({ "event_id": event_id })).await;
    let event: escurel_types::Event =
        serde_json::from_value(events["events"][0].clone()).expect("event");
    let ledger_dir = tempfile::tempdir().expect("tempdir");
    let ledger_path = ledger_dir.path().join("ledger.duckdb");
    let run_id = {
        let ledger = Ledger::open(&ledger_path).expect("open ledger");
        match ledger
            .begin_run(&Trigger::from_event(&event, TENANT.to_owned()))
            .expect("begin_run")
        {
            LedgerDecision::Created(id) => id.0,
            other => panic!("expected a fresh pending row, got {other:?}"),
        }
    };

    let runner = crate::support::RunnerProcess::echo(&gw, TENANT)
        .static_bearer(gw.mint_token(TENANT, Role::Admin))
        .ledger(&ledger_path)
        .spawn();
    let url = format!(
        "http://{}/debug/run?tenant={TENANT}&event_id={event_id}",
        runner.listen()
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(resp) = reqwest::get(&url).await
            && let Ok(v) = resp.json::<Value>().await
            && v["status"].as_str().is_some_and(|s| s != "pending")
        {
            assert_eq!(v["status"], "processed", "{v}");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "recovery never reconciled the row"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    let finished = loop {
        let r = call(&gw, "list_events", json!({ "run_id": run_id })).await;
        if let Some(e) = r["events"]
            .as_array()
            .and_then(|a| a.iter().find(|e| e["title"] == "run-finished"))
        {
            break e.clone();
        }
        assert!(Instant::now() < deadline, "no run-finished for {run_id}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(
        finished["provenance"]["runner"]["agent"], "scribe",
        "{finished}"
    );
    assert_eq!(
        finished["provenance"]["runner"]["skill"], "note",
        "{finished}"
    );
}
