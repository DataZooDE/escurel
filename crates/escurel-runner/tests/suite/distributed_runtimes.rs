//! Distributed runtimes (#818): two real runners share one tenant.
//!
//! Real gateway, two real `escurel-runner` processes on the echo harness,
//! each with its own ledger, labels and harness set, and its own admin
//! identity (`runner:hq`, `runner:muc`). Each registers a `runtime` page;
//! work that declares `requires:` (on its skill page or its provenance) is
//! taken only by the runner that satisfies it, through a `claim_event`
//! lease; a killed runner's lease lapses, its event stays in the inbox
//! unclaimed (the other runner cannot take it), and its runtime reads stale.

use std::time::{Duration, Instant};

use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

use crate::support::{
    RunnerProcess, RunningRunner, body_of, call, control, wait_for_result, wait_for_run_started,
};

const TENANT: &str = "acme";
/// Anyone may run it.
const NOTE: &str =
    "---\nkind: skill\nid: note\nautonomy: auto\n---\n# note\n\nFold the event in.\n";
/// Needs the SAP GUI host.
const FIELD: &str = "---\nkind: skill\nid: field\nautonomy: auto\n\
    requires: {labels: [\"has:sap-gui\"]}\n---\n# field\n\nFold the event in.\n";
/// Needs the SAP GUI host too; the muc runner idles on it so a kill lands mid-run.
const SLOW: &str = "---\nkind: skill\nid: slow\nautonomy: auto\n\
    requires: {labels: [\"has:sap-gui\"]}\n---\n# slow\n\nFold the event in.\n";
/// Runs ON codex, so it needs a runtime whose codex CLI is really there.
const CODED: &str = "---\nkind: skill\nid: coded\nautonomy: auto\nharness: codex\n\
    requires: {harness: codex}\n---\n# coded\n\nFold the event in.\n";

fn instance(skill: &str) -> String {
    format!("---\nkind: instance\nid: c1\nskill: {skill}\n---\n# C1\n\nBASELINE.\n")
}

fn page(skill: &str) -> String {
    format!("markdown/instances/{skill}/c1.md")
}

async fn gateway() -> EscurelProcess {
    gateway_with(ConfigOverrides::default()).await
}

async fn gateway_with(config_overrides: ConfigOverrides) -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        config_overrides,
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("note", NOTE)
                .skill("field", FIELD)
                .skill("slow", SLOW)
                .skill("coded", CODED)
                .instance("note", "c1", instance("note"))
                .instance("field", "c1", instance("field"))
                .instance("slow", "c1", instance("slow"))
                .instance("coded", "c1", instance("coded"))
                .done(),
        ),
    })
    .await
}

fn admin(gw: &EscurelProcess) -> String {
    gw.mint_token(TENANT, Role::Admin)
}

/// `hq`: echo, plus a codex whose CLI is not installed (its probe fails, so
/// it is offered for nothing), `site:hq`.
fn hq(gw: &EscurelProcess) -> RunnerProcess<'_> {
    RunnerProcess::echo(gw, TENANT)
        .static_bearer(gw.mint_token_with_sub(TENANT, Role::Admin, "runner:hq"))
        .env("ESCUREL_RUNNER_ID", "hq")
        .env("ESCUREL_RUNNER_LABELS", "site:hq")
        .env("ESCUREL_RUNNER_HARNESS_ALLOW", "echo,codex")
        .env("ESCUREL_RUNNER_CODEX_BIN", "/nonexistent/codex")
        .env("ESCUREL_RUNNER_STATUS_INTERVAL", "1s")
}

/// `muc`: echo + codex (`codex_bin`), `site:muc,has:sap-gui`, short leases,
/// idles on `slow`.
fn muc<'a>(gw: &'a EscurelProcess, codex_bin: &str) -> RunnerProcess<'a> {
    RunnerProcess::echo(gw, TENANT)
        .static_bearer(gw.mint_token_with_sub(TENANT, Role::Admin, "runner:muc"))
        .env("ESCUREL_RUNNER_ID", "muc")
        .env("ESCUREL_RUNNER_LABELS", "site:muc,has:sap-gui")
        .env("ESCUREL_RUNNER_HARNESS_ALLOW", "echo,codex")
        .env("ESCUREL_RUNNER_CODEX_BIN", codex_bin)
        .env("ESCUREL_RUNNER_STATUS_INTERVAL", "1s")
        .env("ESCUREL_RUNNER_CLAIM_LEASE", "2s")
        .env("ESCUREL_ECHO_SLEEP_MS", "30000")
        .env("ESCUREL_ECHO_SLEEP_SKILL", "slow")
}

/// A stand-in `codex` CLI that answers its version probe and, run as a
/// harness, drains the prompt, leaves `ran` behind as proof, and finishes.
fn codex_stub(dir: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("codex");
    let ran = dir.join("ran");
    std::fs::write(
        &bin,
        format!(
            "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'codex-stub 0.1'; exit 0; fi\n\
             cat > /dev/null\necho run >> '{}'\n\
             echo '{{\"type\":\"turn.completed\",\"usage\":{{\"input_tokens\":1,\"output_tokens\":1}}}}'\n",
            ran.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    (bin, ran)
}

/// `run-started` records at the gateway for runs of `event_id`, from any
/// runner: the fleet-wide count a "runs once" assertion needs.
async fn run_starts(gw: &EscurelProcess, event_id: &str) -> usize {
    let r = call(
        gw,
        &admin(gw),
        "list_events",
        json!({ "label_skill": "escurel:run", "limit": 500 }),
    )
    .await;
    r["events"].as_array().map_or(0, |events| {
        events
            .iter()
            .filter(|e| {
                e["title"] == "run-started" && e["provenance"]["runner"]["event_id"] == event_id
            })
            .count()
    })
}

/// What `runner` decided about inbox event `event_id` (`/debug/admission`).
async fn admission(runner: &RunningRunner, event_id: &str) -> Option<String> {
    let resp = reqwest::get(format!(
        "{}/debug/admission?event_id={event_id}",
        runner.base_url()
    ))
    .await
    .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body: Value = resp.json().await.ok()?;
    body["decision"].as_str().map(str::to_owned)
}

async fn capture(gw: &EscurelProcess, skill: &str, provenance: Value) -> String {
    capture_body(gw, skill, provenance, "work").await
}

async fn capture_body(gw: &EscurelProcess, skill: &str, provenance: Value, body: &str) -> String {
    let r = call(
        gw,
        &admin(gw),
        "capture_event",
        json!({ "source": "test", "mime": "text/plain", "label_skill": skill,
                "instance_page_id": page(skill), "title": "t", "body": body,
                "provenance": provenance }),
    )
    .await;
    r["event_id"].as_str().unwrap().to_owned()
}

async fn runtimes(gw: &EscurelProcess) -> Vec<Value> {
    call(gw, &admin(gw), "list_runtimes", json!({})).await["runtimes"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

async fn runtime(gw: &EscurelProcess, id: &str) -> Option<Value> {
    runtimes(gw)
        .await
        .into_iter()
        .find(|r| r["runner_id"] == id)
}

/// Poll `probe` until it returns `Some`, or fail with `what`.
async fn eventually<T, F, Fut>(within: Duration, what: &str, mut probe: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let deadline = Instant::now() + within;
    loop {
        if let Some(v) = probe().await {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

async fn lookup(gw: &EscurelProcess, event_id: &str) -> Value {
    call(
        gw,
        &admin(gw),
        "list_events",
        json!({ "event_id": event_id }),
    )
    .await
}

/// What `runner` decided on the run-control request `req`, if anything yet.
async fn control_decision(runner: &RunningRunner, req: &str) -> Option<String> {
    let resp = reqwest::get(format!(
        "{}/debug/control?request_event_id={req}",
        runner.base_url()
    ))
    .await
    .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body: Value = resp.json().await.ok()?;
    body["decision"].as_str().map(str::to_owned)
}

async fn healthy(mut r: RunningRunner) -> RunningRunner {
    r.wait_healthy().await;
    r
}

#[tokio::test]
async fn work_goes_only_to_the_runtime_that_can_take_it_and_a_dead_one_goes_stale() {
    let gw = gateway().await;
    let version_file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(version_file.path(), "1.0\n").unwrap();
    let a = healthy(
        hq(&gw)
            .env(
                "ESCUREL_ECHO_VERSION_FILE",
                version_file.path().display().to_string(),
            )
            .env("ESCUREL_RUNNER_PROBE_INTERVAL", "1s")
            .spawn(),
    )
    .await;
    let stub_dir = tempfile::tempdir().unwrap();
    let (codex, codex_ran) = codex_stub(stub_dir.path());
    let mut b = healthy(muc(&gw, &codex.display().to_string()).spawn()).await;

    // Both register, with their own harness sets and labels.
    let rt_a = eventually(Duration::from_secs(20), "hq's runtime", || async {
        runtime(&gw, "hq").await.filter(|r| r["status"] == "online")
    })
    .await;
    let rt_b = eventually(Duration::from_secs(20), "muc's runtime", || async {
        runtime(&gw, "muc")
            .await
            .filter(|r| r["status"] == "online")
    })
    .await;
    assert_eq!(rt_a["labels"], json!(["site:hq"]), "{rt_a}");
    assert_eq!(rt_b["labels"], json!(["site:muc", "has:sap-gui"]), "{rt_b}");
    let names = |rt: &Value| -> Vec<String> {
        rt["harnesses"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["name"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(names(&rt_a), vec!["echo", "codex"], "{rt_a}");
    assert_eq!(names(&rt_b), vec!["echo", "codex"], "{rt_b}");
    assert_eq!(
        rt_a["harnesses"][0]["version"], "escurel-echo-harness 1.0",
        "the probe read the CLI's version: {rt_a}"
    );
    assert_eq!(rt_a["harnesses"][0]["available"], true, "{rt_a}");
    assert_eq!(
        (
            &rt_a["harnesses"][1]["version"],
            &rt_a["harnesses"][1]["available"]
        ),
        (&Value::Null, &json!(false)),
        "a CLI that is not there is unavailable: {rt_a}"
    );
    assert!(
        rt_a["harnesses"][1]["probe_error"]
            .as_str()
            .is_some_and(|e| e.contains("/nonexistent/codex")),
        "the admin view says why: {rt_a}"
    );
    assert_eq!(rt_b["harnesses"][1]["version"], "codex-stub 0.1", "{rt_b}");
    assert_eq!(rt_b["harnesses"][1]["available"], true, "{rt_b}");
    let holder_b = rt_b["claim_holder"].as_str().unwrap().to_owned();
    assert!(holder_b.starts_with("runner:muc#"), "{rt_b}");

    // A CLI upgrade is re-registered without a restart.
    std::fs::write(version_file.path(), "2.0\n").unwrap();
    eventually(Duration::from_secs(20), "hq's re-probe", || async {
        runtime(&gw, "hq")
            .await
            .filter(|r| r["harnesses"][0]["version"] == "escurel-echo-harness 2.0")
    })
    .await;

    // Skill-level `requires:` (a label, a harness) and event-level
    // `requires:` (a harness): muc takes all three, hq — whose codex CLI is
    // missing — never even records them. The codex skill really runs on
    // muc's codex.
    let by_skill = capture(&gw, "field", json!({})).await;
    let by_event = capture(&gw, "note", json!({ "requires": { "harness": "codex" } })).await;
    let on_codex = capture(&gw, "coded", json!({})).await;
    let anyone = capture(&gw, "note", json!({})).await;
    for e in [&by_skill, &by_event] {
        b.wait_for_status(e, &["processed"], Duration::from_secs(30))
            .await;
    }
    // The stub does not write the instance, so the run never reconciles; that
    // it was started on muc's codex CLI is the point.
    eventually(
        Duration::from_secs(30),
        "the codex skill to run on codex",
        || async {
            std::fs::read_to_string(&codex_ran)
                .is_ok_and(|r| !r.is_empty())
                .then_some(())
        },
    )
    .await;
    assert!(b.debug_run(&on_codex).await.is_some(), "it ran on muc");
    eventually(
        Duration::from_secs(30),
        "the unconstrained event to run",
        || async {
            let mut done = false;
            for r in [&a, &b] {
                done |= r
                    .debug_run(&anyone)
                    .await
                    .is_some_and(|run| run["status"] == "processed");
            }
            done.then_some(())
        },
    )
    .await;
    for e in [&by_skill, &by_event, &on_codex] {
        assert_eq!(
            admission(&a, e).await.as_deref(),
            Some("unroutable"),
            "hq saw {e} and left it"
        );
        assert!(a.debug_run(e).await.is_none(), "hq must not claim {e}");
    }
    assert_eq!(
        run_starts(&gw, &anyone).await,
        1,
        "an unconstrained event runs on ONE runtime, not both"
    );

    // muc takes a slow one, then dies mid-run.
    let stranded = capture(&gw, "slow", json!({})).await;
    b.wait_for_status(&stranded, &["pending"], Duration::from_secs(30))
        .await;
    let held = lookup(&gw, &stranded).await;
    assert_eq!(
        held["claim"]["holder"],
        json!(holder_b),
        "muc holds it: {held}"
    );
    b.kill();

    // The lease lapses; the event is still in the inbox and nobody holds it.
    eventually(Duration::from_secs(15), "muc's lease to lapse", || async {
        let looked = lookup(&gw, &stranded).await;
        (looked.get("claim").is_none() && looked["events"][0]["status"] == "inbox")
            .then_some(looked)
    })
    .await;
    // hq has had its chance at the stranded event once it has run work
    // captured after the lapse: its poller walked the inbox since.
    let later = capture_body(&gw, "note", json!({}), "later work").await;
    a.wait_for_status(&later, &["processed"], Duration::from_secs(30))
        .await;
    let looked = lookup(&gw, &stranded).await;
    assert!(
        looked.get("claim").is_none(),
        "hq never claims it: {looked}"
    );
    assert_eq!(looked["events"][0]["status"], "inbox", "{looked}");
    let inbox = call(&gw, &admin(&gw), "list_inbox", json!({})).await;
    assert!(
        inbox["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["event_id"] == stranded),
        "still visible in the inbox: {inbox}"
    );
    assert!(
        a.debug_run(&stranded).await.is_none(),
        "hq has no run for it"
    );

    // muc's runtime reads stale; hq's stays online.
    eventually(Duration::from_secs(15), "muc to read stale", || async {
        runtime(&gw, "muc").await.filter(|r| r["status"] == "stale")
    })
    .await;
    assert_eq!(runtime(&gw, "hq").await.unwrap()["status"], "online");
}

/// The runner that holds a run answers its controls: an inject to a run on
/// muc comes back `injected`, written by muc, while hq stays silent. The
/// result id is first-writer-wins, so one stored result cannot show that hq
/// kept quiet: hq's own decision (`/debug/control`) is asserted, once hq has
/// made its final one (crew F12).
#[tokio::test]
async fn an_inject_to_a_run_on_one_runtime_is_answered_by_that_runtime() {
    let gw = gateway().await;
    let token = admin(&gw);
    let a = healthy(hq(&gw).spawn()).await;
    let b = healthy(muc(&gw, "/nonexistent/codex").spawn()).await;

    let event = capture(&gw, "slow", json!({})).await;
    let run = b
        .wait_for_status(&event, &["pending"], Duration::from_secs(30))
        .await;
    let run_id = run["run_id"].as_str().unwrap().to_owned();
    wait_for_run_started(&gw, &token, &run_id).await;

    let req = control(
        &gw,
        &token,
        json!({ "action": "inject", "run_id": run_id, "text": "use the 2027 terms" }),
    )
    .await;
    let result = wait_for_result(&gw, &token, &req).await;
    assert_eq!(body_of(&result)["outcome"], "injected", "{result}");
    assert_eq!(
        result["provenance"]["captured_by"], "runner:muc",
        "the holder answered: {result}"
    );
    // hq did not file a second (or an earlier, contradicting) answer: it
    // deferred to the holder and, once its grace ran out, found the answer.
    let decision = eventually(Duration::from_secs(20), "hq's final decision", || async {
        let d = control_decision(&a, &req).await?;
        (d == "already_answered" || d.starts_with("answered")).then_some(d)
    })
    .await;
    assert_eq!(decision, "already_answered", "hq must not answer muc's run");
    let answers = call(
        &gw,
        &token,
        "list_events",
        json!({ "event_id": format!("run-control-result:{req}") }),
    )
    .await;
    assert_eq!(answers["events"].as_array().unwrap().len(), 1, "{answers}");
}

/// N concurrent claims on one event, as a fleet of runners would race: one wins.
#[tokio::test]
async fn concurrent_claims_on_one_event_have_one_winner() {
    let gw = gateway().await;
    let event = capture(&gw, "note", json!({})).await;
    let token = gw.mint_token_with_sub(TENANT, Role::Admin, "runner:fleet");
    let mut set = tokio::task::JoinSet::new();
    for n in 0..8 {
        let (url, token, event) = (gw.mcp_url(), token.clone(), event.clone());
        set.spawn(async move {
            let body: Value = reqwest::Client::new()
                .post(url)
                .header("authorization", format!("Bearer {token}"))
                .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                    "params": { "name": "claim_event", "arguments":
                        { "event_id": event, "holder_id": format!("r{n}"), "lease_secs": 30 } } }))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            body["result"]["structuredContent"].clone()
        });
    }
    let results = set.join_all().await;
    let winners = results.iter().filter(|r| r["claimed"] == true).count();
    assert_eq!(winners, 1, "{results:?}");
}

/// A runner that cannot read the skill catalogue (crew F7) does not know what
/// a skill `requires:`, so it must not route the skill's work as if it
/// required nothing. Here a role gate refuses this runner's `list_skills`
/// from its first poll: the `field` event (it requires `has:sap-gui`, which
/// this runner lacks) stays in the inbox, unclaimed and unrun.
#[tokio::test]
async fn a_runner_that_cannot_read_the_catalogue_takes_no_work() {
    let gw = gateway_with(ConfigOverrides {
        role_tools: Some("runner-blind=list_inbox".to_owned()),
        ..Default::default()
    })
    .await;
    let event = capture(&gw, "field", json!({})).await;
    let blind = healthy(
        RunnerProcess::echo(&gw, TENANT)
            .static_bearer(gw.mint_token_with_groups(
                TENANT,
                "runner:blind",
                &["runner-blind"],
                true,
            ))
            .env("ESCUREL_RUNNER_ID", "blind")
            .env("ESCUREL_RUNNER_POLL_INTERVAL", "200ms")
            .spawn(),
    )
    .await;
    // It had its chance: it looked at the event and could not decide.
    let decided = eventually(Duration::from_secs(20), "blind's decision", || async {
        admission(&blind, &event).await
    })
    .await;
    assert_eq!(decided, "retry");
    assert!(
        blind.debug_run(&event).await.is_none(),
        "the runner took work whose requirement it could not read"
    );
    let looked = lookup(&gw, &event).await;
    assert!(looked.get("claim").is_none(), "{looked}");
    assert_eq!(looked["events"][0]["status"], "inbox", "{looked}");
}

/// An exclusive runner (crew F8, `ESCUREL_RUNNER_EXCLUSIVE`) takes only work
/// whose SKILL names one of its labels or its non-default harness, and an
/// event's `provenance.requires` only narrows its skill's: it can never pull
/// an exclusive runner onto a skill that does not ask for it, nor loosen what
/// the skill asks for.
#[tokio::test]
async fn an_exclusive_runner_takes_only_its_skills_and_an_event_can_only_narrow() {
    let gw = gateway().await;
    let a = healthy(hq(&gw).spawn()).await;
    let b = healthy(
        muc(&gw, "/nonexistent/codex")
            .env("ESCUREL_RUNNER_EXCLUSIVE", "true")
            .spawn(),
    )
    .await;

    let plain = capture(&gw, "note", json!({})).await;
    let its_own = capture(&gw, "field", json!({})).await;
    // A capturer names muc's label on a skill that does not ask for it.
    let steered = capture(
        &gw,
        "note",
        json!({ "requires": { "labels": ["has:sap-gui"] } }),
    )
    .await;
    // An event adds a label to its skill's: now nobody carries both.
    let narrowed = capture(
        &gw,
        "field",
        json!({ "requires": { "labels": ["site:hq"] } }),
    )
    .await;

    b.wait_for_status(&its_own, &["processed"], Duration::from_secs(30))
        .await;
    a.wait_for_status(&plain, &["processed"], Duration::from_secs(30))
        .await;
    // Each runner has looked at each event nobody may take, and left it
    // (`plain` and `its_own` are already processed by the other runner).
    for runner in [&a, &b] {
        for e in [&steered, &narrowed] {
            eventually(Duration::from_secs(20), "a routing decision", || async {
                let d = admission(runner, e).await?;
                (d == "unroutable" || d == "done").then_some(())
            })
            .await;
        }
    }
    for e in [&plain, &steered, &narrowed] {
        assert!(b.debug_run(e).await.is_none(), "exclusive muc took {e}");
    }
    for e in [&its_own, &steered, &narrowed] {
        assert!(a.debug_run(e).await.is_none(), "hq took {e}");
    }
    for e in [&steered, &narrowed] {
        let looked = lookup(&gw, e).await;
        assert_eq!(looked["events"][0]["status"], "inbox", "{looked}");
        assert!(looked.get("claim").is_none(), "{looked}");
    }
}

/// The gateway grants a claim lease of at most an hour. A runner configured
/// past it would have every claim refused — and must never read a refusal as
/// "no other runner" (crew C-F1) — so it refuses to start, naming the key.
#[tokio::test]
async fn a_claim_lease_the_gateway_would_refuse_stops_the_runner_at_boot() {
    let gw = gateway().await;
    let out = std::process::Command::new(escurel_test_support::runner_bin())
        .env_remove("ESCUREL_RUNNER_TOKEN")
        .env("ESCUREL_RUNNER_GATEWAY_URL", gw.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_TOKEN", admin(&gw))
        .env(
            "ESCUREL_RUNNER_LISTEN",
            format!("127.0.0.1:{}", escurel_test_support::free_port()),
        )
        .env("ESCUREL_RUNNER_HARNESS", "echo")
        .env("ESCUREL_RUNNER_CLAIM_LEASE", "7200")
        .output()
        .expect("run escurel-runner");
    assert!(!out.status.success(), "it must not start");
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(
        said.contains("ESCUREL_RUNNER_CLAIM_LEASE"),
        "the error names the key: {said}"
    );
}

/// A fleet member's webhook (crew U-F1): muc holds the lease on a slow event;
/// a labelled runner that cannot read the catalogue gets the same event by
/// POST /trigger. It cannot route or claim it now, and it is not alone, so it
/// leaves the event to the poller instead of holding it on its ledger: the
/// event runs once, on muc.
#[tokio::test]
async fn a_fleet_runners_undecidable_webhook_is_left_to_the_poller() {
    let gw = gateway_with(ConfigOverrides {
        role_tools: Some("runner-blind=list_inbox".to_owned()),
        ..Default::default()
    })
    .await;
    let holder = healthy(muc(&gw, "/nonexistent/codex").spawn()).await;
    let event = capture(&gw, "slow", json!({})).await;
    let run = holder
        .wait_for_status(&event, &["pending"], Duration::from_secs(30))
        .await;
    wait_for_run_started(&gw, &admin(&gw), run["run_id"].as_str().unwrap()).await;

    let blind = healthy(
        RunnerProcess::echo(&gw, TENANT)
            .static_bearer(gw.mint_token_with_groups(
                TENANT,
                "runner:blind",
                &["runner-blind"],
                true,
            ))
            .env("ESCUREL_RUNNER_ID", "blind")
            .env("ESCUREL_RUNNER_LABELS", "site:hq")
            .spawn(),
    )
    .await;
    let stored = lookup(&gw, &event).await["events"][0].clone();
    let resp = reqwest::Client::new()
        .post(format!("{}/trigger", blind.base_url()))
        .json(&stored)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 202);
    let decided = eventually(Duration::from_secs(20), "the webhook's fate", || async {
        admission(&blind, &event)
            .await
            .filter(|d| d.starts_with("webhook-"))
    })
    .await;
    assert_eq!(decided, "webhook-skipped");
    assert!(
        blind.debug_run(&event).await.is_none(),
        "the fleet member ran nothing"
    );
    assert_eq!(run_starts(&gw, &event).await, 1, "one run, on the holder");
}

/// A runner whose credential may not claim (not an admin) takes nothing —
/// lone or in a fleet (owner decision 2026-11-13; crew U-F3): running the event
/// anyway could race a runner that holds it. It keeps serving `/healthz`, so an
/// operator sees the error it logs rather than a crash loop.
#[tokio::test]
async fn a_runner_whose_claims_are_refused_takes_nothing() {
    let gw = gateway().await;
    let event = capture(&gw, "note", json!({})).await;
    let refused = healthy(
        RunnerProcess::echo(&gw, TENANT)
            .static_bearer(gw.mint_token_with_groups(TENANT, "runner:member", &[], false))
            .env("ESCUREL_RUNNER_ID", "member")
            .spawn(),
    )
    .await;
    let decided = eventually(Duration::from_secs(20), "the member's decision", || async {
        admission(&refused, &event).await
    })
    .await;
    assert_eq!(decided, "refused");
    assert!(refused.debug_run(&event).await.is_none(), "it ran nothing");
    let looked = lookup(&gw, &event).await;
    assert_eq!(looked["events"][0]["status"], "inbox", "{looked}");
}
