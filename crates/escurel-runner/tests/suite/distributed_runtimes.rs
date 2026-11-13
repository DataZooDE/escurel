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
                .instance("note", "c1", instance("note"))
                .instance("field", "c1", instance("field"))
                .instance("slow", "c1", instance("slow"))
                .done(),
        ),
    })
    .await
}

fn admin(gw: &EscurelProcess) -> String {
    gw.mint_token(TENANT, Role::Admin)
}

/// `hq`: echo only, `site:hq`.
fn hq(gw: &EscurelProcess) -> RunnerProcess<'_> {
    RunnerProcess::echo(gw, TENANT)
        .static_bearer(gw.mint_token_with_sub(TENANT, Role::Admin, "runner:hq"))
        .env("ESCUREL_RUNNER_ID", "hq")
        .env("ESCUREL_RUNNER_LABELS", "site:hq")
        .env("ESCUREL_RUNNER_STATUS_INTERVAL", "1s")
}

/// `muc`: echo + codex, `site:muc,has:sap-gui`, short leases, idles on `slow`.
fn muc(gw: &EscurelProcess) -> RunnerProcess<'_> {
    RunnerProcess::echo(gw, TENANT)
        .static_bearer(gw.mint_token_with_sub(TENANT, Role::Admin, "runner:muc"))
        .env("ESCUREL_RUNNER_ID", "muc")
        .env("ESCUREL_RUNNER_LABELS", "site:muc,has:sap-gui")
        .env("ESCUREL_RUNNER_HARNESS_ALLOW", "echo,codex")
        .env("ESCUREL_RUNNER_CODEX_BIN", "/nonexistent/codex")
        .env("ESCUREL_RUNNER_STATUS_INTERVAL", "1s")
        .env("ESCUREL_RUNNER_CLAIM_LEASE", "2s")
        .env("ESCUREL_ECHO_SLEEP_MS", "30000")
        .env("ESCUREL_ECHO_SLEEP_SKILL", "slow")
}

async fn capture(gw: &EscurelProcess, skill: &str, provenance: Value) -> String {
    let r = call(
        gw,
        &admin(gw),
        "capture_event",
        json!({ "source": "test", "mime": "text/plain", "label_skill": skill,
                "instance_page_id": page(skill), "title": "t", "body": "work",
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
    let mut b = healthy(muc(&gw).spawn()).await;

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
    assert_eq!(names(&rt_a), vec!["echo"], "{rt_a}");
    assert_eq!(names(&rt_b), vec!["echo", "codex"], "{rt_b}");
    assert_eq!(
        rt_a["harnesses"][0]["version"], "escurel-echo-harness 1.0",
        "the probe read the CLI's version: {rt_a}"
    );
    assert_eq!(
        rt_b["harnesses"][1]["version"],
        Value::Null,
        "a CLI that is not there: {rt_b}"
    );
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

    // Skill-level `requires:` (a label) and event-level `requires:` (a
    // harness): muc takes both, hq never even records them.
    let by_skill = capture(&gw, "field", json!({})).await;
    let by_event = capture(&gw, "note", json!({ "requires": { "harness": "codex" } })).await;
    let anyone = capture(&gw, "note", json!({})).await;
    for e in [&by_skill, &by_event] {
        b.wait_for_status(e, &["processed"], Duration::from_secs(30))
            .await;
    }
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
    for e in [&by_skill, &by_event] {
        assert!(a.debug_run(e).await.is_none(), "hq must not claim {e}");
    }
    assert!(
        a.debug_run(&anyone).await.is_none() || b.debug_run(&anyone).await.is_none(),
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
        looked.get("claim").is_none().then_some(looked)
    })
    .await;
    tokio::time::sleep(Duration::from_secs(2)).await;
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
/// muc comes back `injected`, written by muc, while hq stays silent.
#[tokio::test]
async fn an_inject_to_a_run_on_one_runtime_is_answered_by_that_runtime() {
    let gw = gateway().await;
    let token = admin(&gw);
    let _a = healthy(hq(&gw).spawn()).await;
    let b = healthy(muc(&gw).spawn()).await;

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
    // hq did not file a second (or an earlier, contradicting) answer.
    tokio::time::sleep(Duration::from_secs(3)).await;
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
    // Many polls' worth of chances.
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert!(
        blind.debug_run(&event).await.is_none(),
        "the runner took work whose requirement it could not read"
    );
    let looked = lookup(&gw, &event).await;
    assert!(looked.get("claim").is_none(), "{looked}");
    assert_eq!(looked["events"][0]["status"], "inbox", "{looked}");
}
