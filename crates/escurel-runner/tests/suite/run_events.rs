//! The runner writes a run's lifecycle as `escurel:run` system events
//! (knowledge-workbench backend, P1 PR7 — BRD FR-R-1/2/4/5).
//!
//! Real gateway, real runner binary, real echo harness. The ledger stays the
//! source of truth; the events are its projection — best-effort, so a
//! gateway that refuses them (a non-admin runner bearer) never fails the run.
//! Every run lands on the root event's page as `run-started`, one
//! `run-attempt` per attempt, and `run-finished` with the ledger's terminal.

use std::process::{Child, Command};
use std::time::{Duration, Instant};

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role, free_port};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const SKILL: &str = "renewal";
const SKILL_BODY: &str = "---\ntype: skill\nid: renewal\nautonomy: auto\n---\n# renewal\n\nFold the event into the instance.\n";
const INSTANCE_BODY: &str = "---\ntype: instance\nid: c1\nskill: renewal\n---\n# C1\n\nBASELINE.\n";
const PAGE: &str = "markdown/instances/renewal/c1.md";

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn call(p: &EscurelProcess, role: Role, name: &str, args: Value) -> Value {
    let token = p.mint_token(TENANT, role);
    let body: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": { "name": name, "arguments": args } }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json");
    assert!(body.get("error").is_none(), "{name}: {body}");
    body["result"]["structuredContent"].clone()
}

async fn gateway() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill(SKILL, SKILL_BODY)
                .instance(SKILL, "c1", INSTANCE_BODY)
                .done(),
        ),
        ..Default::default()
    })
    .await
}

fn spawn_runner(gw: &EscurelProcess, role: Role, extra: &[(&str, &str)]) -> (ChildGuard, String) {
    let listen = format!("127.0.0.1:{}", free_port());
    let ledger_dir = tempfile::tempdir().expect("tempdir");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gw.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_TOKEN", gw.mint_token(TENANT, role))
        .env("ESCUREL_RUNNER_HARNESS", "echo")
        .env(
            "ESCUREL_RUNNER_LEDGER_PATH",
            ledger_dir.keep().join("ledger.sqlite"),
        )
        .env("ESCUREL_RUNNER_RETRY_BACKOFF", "50ms")
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms");
    for (k, v) in extra {
        cmd.env(k, v);
    }
    (ChildGuard(cmd.spawn().expect("spawn runner")), listen)
}

async fn capture(gw: &EscurelProcess) -> String {
    let r = call(
        gw,
        Role::Agent,
        "capture_event",
        json!({ "source": "manual", "mime": "text/plain", "label_skill": SKILL,
                "instance_page_id": PAGE, "title": "renewal request",
                "body": "ECHO_FOLD_MARKER customer wants to renew" }),
    )
    .await;
    r["event_id"].as_str().unwrap().to_owned()
}

/// Wait for the ledger's terminal for `event_id`; returns `(run_id, status)`.
async fn await_terminal(listen: &str, event_id: &str) -> (String, String) {
    let url = format!("http://{listen}/debug/run?tenant={TENANT}&event_id={event_id}");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(resp) = reqwest::get(&url).await
            && let Ok(v) = resp.json::<Value>().await
            && let Some(status) = v["status"].as_str()
            && status != "pending"
        {
            return (v["run_id"].as_str().unwrap().to_owned(), status.to_owned());
        }
        assert!(Instant::now() < deadline, "run never reached a terminal");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn run_events(gw: &EscurelProcess, run_id: &str) -> Vec<Value> {
    // The projection is best-effort and written AFTER the ledger terminal;
    // give it a moment.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let r = call(gw, Role::Admin, "list_events", json!({ "run_id": run_id })).await;
        let events = r["events"].as_array().cloned().unwrap_or_default();
        if events.iter().any(|e| e["title"] == "run-finished") || Instant::now() >= deadline {
            return events;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn body(e: &Value) -> Value {
    serde_json::from_str(e["body"].as_str().unwrap_or("{}")).unwrap_or_default()
}

#[tokio::test]
async fn a_run_writes_started_attempt_and_finished_system_events_on_the_target_page() {
    let gw = gateway().await;
    let event_id = capture(&gw).await;
    // Admin: the runner's own bearer is admin in production, and the
    // `escurel:` namespace is admin-only to write.
    let (_runner, listen) = spawn_runner(&gw, Role::Admin, &[]);
    let (run_id, status) = await_terminal(&listen, &event_id).await;
    assert_eq!(status, "processed");

    let events = run_events(&gw, &run_id).await;
    let titles: Vec<&str> = events
        .iter()
        .map(|e| e["title"].as_str().unwrap())
        .collect();
    assert_eq!(
        titles,
        ["run-started", "run-attempt", "run-finished"],
        "{events:?}"
    );
    for e in &events {
        assert_eq!(e["kind"], "system");
        assert_eq!(e["label_skill"], "escurel:run");
        assert_eq!(e["status"], "processed", "attached to the target page: {e}");
        assert_eq!(e["instance_page_id"], PAGE);
        assert_eq!(e["run_id"], run_id);
        assert_eq!(e["root_event_id"], event_id, "the trigger is its own root");
        let runner = &e["provenance"]["runner"];
        assert_eq!(runner["run_id"], run_id, "{e}");
        assert_eq!(runner["root_event_id"], event_id);
        assert_eq!(runner["event_id"], event_id);
        assert_eq!(runner["depth"], 0);
        assert_eq!(runner["harness"], "echo");
        assert_eq!(runner["max_attempts"], 3);
        assert_eq!(runner["target_page_id"], PAGE);
        assert!(
            runner["trace_id"].as_str().is_some_and(|t| t.len() == 32),
            "{e}"
        );
    }
    let attempt = body(&events[1]);
    assert_eq!(attempt["attempt"], 1);
    assert_eq!(attempt["outcome"], "ok", "{attempt}");
    let finished = body(&events[2]);
    assert_eq!(finished["status"], "processed", "{finished}");
    assert_eq!(finished["attempts"], 1);
    assert_eq!(finished["produced_instance"], PAGE);
    assert!(
        finished["produced_version"]
            .as_str()
            .is_some_and(|v| !v.is_empty())
    );
    assert!(
        finished["summary"]
            .as_str()
            .is_some_and(|s| s.contains("folded")),
        "{finished}"
    );
    assert_eq!(
        finished["tool_calls"], 4,
        "the echo makes four calls: {finished}"
    );
    assert_eq!(events[2]["provenance"]["runner"]["autonomy"], "auto");
    assert_eq!(events[2]["provenance"]["runner"]["attempt"], 1);

    // The lineage read shows the root and its run together.
    let tree = call(
        &gw,
        Role::Admin,
        "list_events",
        json!({ "root_event_id": event_id, "include_system": true }),
    )
    .await;
    let ids: Vec<&str> = tree["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["event_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.len(), 4, "{tree}");
    // Membership, not order: the root was captured undated (`at_ts` NULL)
    // and the listing sorts NULLS LAST, so a client folds by ids
    // (`list_lineage` does), never by position.
    assert!(ids.contains(&event_id.as_str()), "{ids:?}");
    assert!(
        ids.iter().filter(|i| i.starts_with("run:")).count() == 3,
        "{ids:?}"
    );
}

#[tokio::test]
async fn a_permanent_failure_dead_letters_with_the_attempts_error_and_is_not_re_driven() {
    let gw = gateway().await;
    let event_id = capture(&gw).await;
    // The echo's injected failure is a non-zero exit: a PERMANENT failure,
    // which fails fast (one attempt). It dead-letters (owner decision after
    // the live smoke of P3, 2026-09-23): a retriable `failed` row was
    // re-claimed by the poller every interval as a NEW run until the
    // per-root budget was spent. A dead letter is idempotency-terminal — the
    // event waits in the DLQ for a human `requeue` / `retry`.
    let (_runner, listen) = spawn_runner(&gw, Role::Admin, &[("ESCUREL_ECHO_FAIL_SKILL", SKILL)]);
    let (run_id, status) = await_terminal(&listen, &event_id).await;
    assert_eq!(status, "dead_letter");

    let events = run_events(&gw, &run_id).await;
    let titles: Vec<&str> = events
        .iter()
        .map(|e| e["title"].as_str().unwrap())
        .collect();
    assert_eq!(
        titles,
        ["run-started", "run-attempt", "run-finished"],
        "{events:?}"
    );
    let attempt = body(&events[1]);
    assert_eq!(attempt["attempt"], 1);
    assert_eq!(attempt["outcome"], "failed", "{attempt}");
    assert!(
        attempt["error"]
            .as_str()
            .is_some_and(|s| s.contains("injected")),
        "{attempt}"
    );
    let finished = body(&events[2]);
    assert_eq!(finished["status"], "dead_letter", "{finished}");
    assert_eq!(finished["reason"], "permanent");
    assert_eq!(finished["attempts"], 1);
    assert!(
        finished["error"]
            .as_str()
            .is_some_and(|s| s.contains("injected")),
        "the attempt's own message rides on the dead letter too: {finished}"
    );
    assert!(finished["produced_instance"].is_null());

    // Not re-driven: several poll intervals later the ledger still holds
    // exactly this one run, and the DLQ names it with its reason.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let ledger: Value = reqwest::get(format!("http://{listen}/debug/ledger"))
        .await
        .expect("ledger")
        .json()
        .await
        .expect("json");
    assert_eq!(ledger["total"], 1, "re-driven: {ledger}");
    assert_eq!(ledger["dead_letter"], 1, "{ledger}");
    let dlq: Value = reqwest::get(format!("http://{listen}/dlq"))
        .await
        .expect("dlq")
        .json()
        .await
        .expect("json");
    let entry = dlq["dead_letters"]
        .as_array()
        .and_then(|a| a.iter().find(|d| d["run_id"] == run_id))
        .cloned()
        .unwrap_or_else(|| panic!("{dlq}"));
    assert_eq!(entry["reason"], "permanent", "{entry}");
}

#[tokio::test]
async fn run_events_are_not_written_when_emit_events_is_off() {
    let gw = gateway().await;
    let event_id = capture(&gw).await;
    let (_runner, listen) =
        spawn_runner(&gw, Role::Admin, &[("ESCUREL_RUNNER_EMIT_EVENTS", "false")]);
    let (run_id, status) = await_terminal(&listen, &event_id).await;
    assert_eq!(status, "processed");
    tokio::time::sleep(Duration::from_millis(500)).await;
    let r = call(&gw, Role::Admin, "list_events", json!({ "run_id": run_id })).await;
    assert!(r["events"].as_array().unwrap().is_empty(), "{r}");
}

#[tokio::test]
async fn a_gateway_that_refuses_the_run_event_does_not_fail_the_run() {
    let gw = gateway().await;
    let event_id = capture(&gw).await;
    // A NON-admin runner bearer: the gateway refuses the `escurel:` write.
    // The projection is best-effort; the run itself must still land.
    let (_runner, listen) = spawn_runner(&gw, Role::Agent, &[]);
    let (run_id, status) = await_terminal(&listen, &event_id).await;
    assert_eq!(
        status, "processed",
        "the run lands whatever the projection does"
    );
    let expanded = call(&gw, Role::Agent, "expand", json!({ "page_id": PAGE })).await;
    assert!(
        expanded["body"].as_str().unwrap().contains("folded event"),
        "{expanded}"
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    let r = call(&gw, Role::Admin, "list_events", json!({ "run_id": run_id })).await;
    assert!(
        r["events"].as_array().unwrap().is_empty(),
        "refused, so absent: {r}"
    );
}

/// Crash recovery (P1 PR7b — BRD FR-R-5): a run that landed but died before
/// its terminal was recorded is reconciled on the next boot — and its
/// `run-finished` is written then, so the projection never misses a
/// terminal the ledger reached. Idempotent by event id: a run that had
/// already written its own is a no-op.
#[tokio::test]
async fn recovery_writes_the_missing_run_finished_for_an_orphaned_pending_row() {
    use escurel_runner_core::{Ledger, LedgerDecision, Lineage, Trigger};

    let gw = gateway().await;
    let event_id = capture(&gw).await;
    // The effect landed: the event is folded into the page (assigned).
    call(
        &gw,
        Role::Admin,
        "assign_event",
        json!({ "event_id": event_id, "instance_page_id": PAGE }),
    )
    .await;
    // …but the runner died before recording the terminal: an orphaned
    // `pending` row in its ledger, seeded through the real ledger API.
    let ledger_dir = tempfile::tempdir().expect("tempdir");
    let ledger_path = ledger_dir.path().join("ledger.sqlite");
    let run_id = {
        let ledger = Ledger::open(&ledger_path).expect("open ledger");
        match ledger
            .begin_run(&Trigger {
                manual: None,
                is_system: false,
                tenant: TENANT.to_owned(),
                event_id: event_id.clone(),
                label_skill: SKILL.to_owned(),
                instance_page_id: Some(PAGE.to_owned()),
                lineage: Lineage::root(event_id.clone()),
                workflow: None,
                content_hash: None,
            })
            .expect("begin_run")
        {
            LedgerDecision::Created(id) => id.0,
            other => panic!("expected a fresh pending row, got {other:?}"),
        }
    };

    let listen = format!("127.0.0.1:{}", free_port());
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gw.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_TOKEN", gw.mint_token(TENANT, Role::Admin))
        .env("ESCUREL_RUNNER_HARNESS", "echo")
        .env("ESCUREL_RUNNER_LEDGER_PATH", &ledger_path)
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms");
    let _runner = ChildGuard(cmd.spawn().expect("spawn runner"));

    // Recovery runs on boot: the row is confirmed and marked processed.
    let (recovered_run, status) = await_terminal(&listen, &event_id).await;
    assert_eq!(recovered_run, run_id);
    assert_eq!(status, "processed");

    let events = run_events(&gw, &run_id).await;
    let titles: Vec<&str> = events
        .iter()
        .map(|e| e["title"].as_str().unwrap())
        .collect();
    assert_eq!(
        titles,
        ["run-finished"],
        "only the terminal is reconstructible: {events:?}"
    );
    let e = &events[0];
    assert_eq!(e["event_id"], format!("run:{run_id}:finished"));
    assert_eq!(e["instance_page_id"], PAGE);
    assert_eq!(e["provenance"]["runner"]["harness"], "recovery", "{e}");
    assert_eq!(e["provenance"]["runner"]["run_id"], run_id);
    let finished = body(e);
    assert_eq!(finished["status"], "processed", "{finished}");
    assert_eq!(finished["produced_instance"], PAGE);
    assert!(
        finished["summary"]
            .as_str()
            .is_some_and(|s| s.contains("reconciled")),
        "{finished}"
    );
}
