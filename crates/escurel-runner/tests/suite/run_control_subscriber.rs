//! The runner acts on `escurel:run-control` events (knowledge-workbench
//! backend P2-3b — BRD FR-C). A human's request, authorised and stamped by
//! the gateway (P2-2), is tailed by the runner, acted on — cancel, retry,
//! pause, resume, requeue — and answered under `escurel:run-control-result`,
//! one result per request, in the run's own record.
//!
//! Real gateway, real runner binary, real echo harness idling long enough
//! for a cancel to land mid-run.

use std::process::{Child, Command};
use std::time::{Duration, Instant};

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role, free_port};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const SKILL_BODY: &str = "---\ntype: skill\nid: renewal\nautonomy: auto\n---\n# renewal\n\nFold the event into the instance.\n";
const INSTANCE_BODY: &str = "---\ntype: instance\nid: c1\nskill: renewal\n---\n# C1\n\nBASELINE.\n";
const PAGE: &str = "markdown/instances/renewal/c1.md";
const ECHO_SLEEP_MS: &str = "4000";

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn call(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
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

async fn capture(p: &EscurelProcess, token: &str, title: &str) -> String {
    let r = call(
        p,
        token,
        "capture_event",
        json!({ "source": "manual", "mime": "text/plain", "label_skill": "renewal",
                "instance_page_id": PAGE, "title": title, "body": "please renew" }),
    )
    .await;
    r["event_id"].as_str().unwrap().to_owned()
}

/// Wait until the run's `run-started` exists: the gateway authorises a
/// cancel / retry through it, and the runner writes it best-effort right
/// after claiming the run, so a request sent the instant `/debug/run`
/// flips to pending can precede it.
async fn wait_for_run_started(p: &EscurelProcess, token: &str, run_id: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let own = call(p, token, "list_events", json!({ "run_id": run_id })).await;
        if own["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["title"] == "run-started")
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "no run-started for {run_id}: {own}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// A control request, as the gateway authorises and stamps it (P2-2).
async fn control(p: &EscurelProcess, token: &str, body: Value) -> String {
    let r = call(
        p,
        token,
        "capture_event",
        json!({ "source": "workbench", "mime": "application/json",
                "label_skill": "escurel:run-control", "title": body["action"],
                "body": body.to_string() }),
    )
    .await;
    r["event_id"].as_str().unwrap().to_owned()
}

async fn debug_run(listen: &str, event_id: &str) -> Option<Value> {
    let resp = reqwest::get(format!(
        "http://{listen}/debug/run?tenant={TENANT}&event_id={event_id}"
    ))
    .await
    .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.json().await.ok()
}

async fn wait_for_status(listen: &str, event_id: &str, want: &[&str], within: Duration) -> Value {
    let deadline = Instant::now() + within;
    loop {
        if let Some(run) = debug_run(listen, event_id).await
            && want.iter().any(|w| run["status"] == *w)
        {
            return run;
        }
        assert!(Instant::now() < deadline, "run never reached {want:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The result the runner wrote for `request_id`, once it exists.
async fn wait_for_result(p: &EscurelProcess, token: &str, request_id: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let r = call(
            p,
            token,
            "list_events",
            json!({ "label_skill": "escurel:run-control-result" }),
        )
        .await;
        if let Some(e) = r["events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["provenance"]["control"]["request_event_id"] == request_id)
        {
            return e.clone();
        }
        assert!(Instant::now() < deadline, "no result for {request_id}: {r}");
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

fn body_of(e: &Value) -> Value {
    serde_json::from_str(e["body"].as_str().unwrap()).unwrap()
}

/// A `retry` that lands while the tenant is paused is throttled at
/// admission; the row must go back to a retriable terminal for the poller
/// to re-drive after `resume`, not sit `pending` with nothing queued
/// (codex second-opinion review of P2, P1).
#[tokio::test]
async fn a_throttled_retry_is_re_driven_by_the_poller_not_wedged() {
    let gw = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("renewal", SKILL_BODY)
                .instance("renewal", "c1", INSTANCE_BODY)
                .done(),
        ),
        ..Default::default()
    })
    .await;
    let admin = gw.mint_token_with_sub(TENANT, Role::Admin, "ops:jo");
    let listen = format!("127.0.0.1:{}", free_port());
    let ledger_dir = tempfile::tempdir().expect("tempdir");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gw.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_TOKEN", &admin)
        .env("ESCUREL_RUNNER_HARNESS", "echo")
        .env(
            "ESCUREL_RUNNER_LEDGER_PATH",
            ledger_dir.keep().join("ledger.sqlite"),
        )
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms")
        .env("ESCUREL_RUNNER_CANCEL_GRACE", "1s")
        .env("ESCUREL_ECHO_SLEEP_MS", ECHO_SLEEP_MS);
    let _runner = ChildGuard(cmd.spawn().expect("spawn runner"));

    // Run 1, cancelled → a retriable terminal.
    let e1 = capture(&gw, &admin, "renew one").await;
    let run1 = wait_for_status(&listen, &e1, &["pending"], Duration::from_secs(30)).await["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    wait_for_run_started(&gw, &admin, &run1).await;
    control(&gw, &admin, json!({ "action": "cancel", "run_id": run1 })).await;
    wait_for_status(&listen, &e1, &["cancelled"], Duration::from_secs(10)).await;

    // Paused, the retry is admitted by nobody: answered `requeued`, and the
    // row must not be left `pending` with nothing queued.
    let req = control(&gw, &admin, json!({ "action": "pause" })).await;
    wait_for_result(&gw, &admin, &req).await;
    let req = control(&gw, &admin, json!({ "action": "retry", "run_id": run1 })).await;
    let result = wait_for_result(&gw, &admin, &req).await;
    assert_eq!(body_of(&result)["outcome"], "requeued", "{result}");
    tokio::time::sleep(Duration::from_millis(800)).await;
    let run = wait_for_status(
        &listen,
        &e1,
        &["failed", "dead_letter", "processed"],
        Duration::from_secs(5),
    )
    .await;
    assert_ne!(run["status"], "pending", "{run}");

    // Resumed, the poller re-drives it to completion.
    let req = control(&gw, &admin, json!({ "action": "resume" })).await;
    wait_for_result(&gw, &admin, &req).await;
    let run = wait_for_status(&listen, &e1, &["processed"], Duration::from_secs(40)).await;
    assert_eq!(run["status"], "processed", "{run}");
}

#[tokio::test]
async fn the_runner_acts_on_control_events_and_answers_each_one() {
    let gw = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("renewal", SKILL_BODY)
                .instance("renewal", "c1", INSTANCE_BODY)
                .done(),
        ),
        ..Default::default()
    })
    .await;
    let admin = gw.mint_token_with_sub(TENANT, Role::Admin, "ops:jo");

    let listen = format!("127.0.0.1:{}", free_port());
    let ledger_dir = tempfile::tempdir().expect("tempdir");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gw.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_TOKEN", &admin)
        .env("ESCUREL_RUNNER_HARNESS", "echo")
        .env(
            "ESCUREL_RUNNER_LEDGER_PATH",
            ledger_dir.keep().join("ledger.sqlite"),
        )
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms")
        .env("ESCUREL_RUNNER_CANCEL_GRACE", "1s")
        .env("ESCUREL_ECHO_SLEEP_MS", ECHO_SLEEP_MS);
    let _runner = ChildGuard(cmd.spawn().expect("spawn runner"));

    // --- cancel: a live run is stopped, and the answer names it.
    let e1 = capture(&gw, &admin, "renew one").await;
    let run = wait_for_status(&listen, &e1, &["pending"], Duration::from_secs(30)).await;
    let run1 = run["run_id"].as_str().unwrap().to_owned();
    wait_for_run_started(&gw, &admin, &run1).await;
    let req = control(
        &gw,
        &admin,
        json!({ "action": "cancel", "run_id": run1, "reason": "wrong document" }),
    )
    .await;
    let run = wait_for_status(&listen, &e1, &["cancelled"], Duration::from_secs(10)).await;
    assert_eq!(run["run_id"], run1);
    let result = wait_for_result(&gw, &admin, &req).await;
    let body = body_of(&result);
    assert_eq!(body["action"], "cancel", "{result}");
    assert_eq!(body["outcome"], "cancelled", "{result}");
    assert_eq!(body["run_id"], run1, "{result}");
    assert_eq!(result["kind"], "system");
    assert_eq!(
        result["instance_page_id"], PAGE,
        "answered where the request was filed"
    );
    assert_eq!(result["run_id"], run1, "in the run's own record");
    assert_eq!(
        result["provenance"]["control"]["requested_by"], "ops:jo",
        "{result}"
    );
    // The cancel reason reached the run's terminal.
    let own = call(&gw, &admin, "list_events", json!({ "run_id": run1 })).await;
    let finished = own["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["title"] == "run-finished")
        .expect("run-finished");
    assert_eq!(body_of(finished)["reason"], "wrong document");

    // --- cancel of a run that is not live: answered, not acted on.
    wait_for_run_started(&gw, &admin, &run1).await;
    let req = control(&gw, &admin, json!({ "action": "cancel", "run_id": run1 })).await;
    let result = wait_for_result(&gw, &admin, &req).await;
    assert_eq!(body_of(&result)["outcome"], "not_live", "{result}");

    // --- retry: the cancelled run is re-driven as a fresh run, which lands.
    let req = control(&gw, &admin, json!({ "action": "retry", "run_id": run1 })).await;
    let result = wait_for_result(&gw, &admin, &req).await;
    let body = body_of(&result);
    assert_eq!(body["outcome"], "requeued", "{result}");
    let run2 = body["new_run_id"].as_str().expect("new run id").to_owned();
    assert_ne!(run2, run1);
    let run = wait_for_status(&listen, &e1, &["processed"], Duration::from_secs(30)).await;
    assert_eq!(run["run_id"], run2, "{run}");

    // --- pause: a new event is not admitted while the tenant is paused…
    let req = control(&gw, &admin, json!({ "action": "pause" })).await;
    let result = wait_for_result(&gw, &admin, &req).await;
    assert_eq!(body_of(&result)["outcome"], "paused", "{result}");
    let e2 = capture(&gw, &admin, "renew two").await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(
        debug_run(&listen, &e2).await.is_none(),
        "paused: no run claimed"
    );
    // …and runs once resumed.
    let req = control(&gw, &admin, json!({ "action": "resume" })).await;
    let result = wait_for_result(&gw, &admin, &req).await;
    assert_eq!(body_of(&result)["outcome"], "resumed", "{result}");
    wait_for_status(
        &listen,
        &e2,
        &["pending", "processed"],
        Duration::from_secs(30),
    )
    .await;

    // --- requeue of an event that is not dead-lettered is refused, with why.
    let req = control(&gw, &admin, json!({ "action": "requeue", "event_id": e1 })).await;
    let result = wait_for_result(&gw, &admin, &req).await;
    let body = body_of(&result);
    assert_eq!(body["outcome"], "refused", "{result}");
    assert!(
        body["detail"].as_str().unwrap_or("").contains("dead"),
        "{result}"
    );

    // Every request got exactly one answer.
    let results = call(
        &gw,
        &admin,
        "list_events",
        json!({ "label_skill": "escurel:run-control-result" }),
    )
    .await;
    assert_eq!(results["events"].as_array().unwrap().len(), 6, "{results}");
    let inbox = call(&gw, &admin, "list_inbox", json!({})).await;
    assert!(
        inbox["events"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| !e["label_skill"].as_str().unwrap().starts_with("escurel:")),
        "no control traffic in the inbox: {inbox}"
    );
}
