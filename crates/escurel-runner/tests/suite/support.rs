//! Shared harness for the runner suite: one way to start a real
//! `escurel-runner` against a real gateway, and the MCP / run-control
//! helpers most files need.
//!
//! Before this existed every file carried its own `ChildGuard` and a
//! hand-rolled `Command` with the same dozen env vars, so a new runner knob
//! meant touching dozens of copies. New tests use [`RunnerProcess`], which
//! now lives in `escurel-test-support` so other crates' tests start a runner
//! the same way.

use std::time::{Duration, Instant};

use escurel_test_support::EscurelProcess;
pub use escurel_test_support::{RunnerProcess, RunningRunner};
use serde_json::{Value, json};

/// `tools/call` `name` as `token`; the structured result. Fails the test on
/// a JSON-RPC error.
pub async fn call(gw: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    let body: Value = reqwest::Client::new()
        .post(gw.mcp_url())
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

/// A system event's JSON body.
pub fn body_of(e: &Value) -> Value {
    serde_json::from_str(e["body"].as_str().unwrap()).unwrap()
}

/// Wait until the run's `run-started` exists: the gateway authorises a
/// run-control request through it, and the runner writes it best-effort
/// right after claiming the run, so a request sent the instant `/debug/run`
/// flips to pending can precede it.
pub async fn wait_for_run_started(gw: &EscurelProcess, token: &str, run_id: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let own = call(gw, token, "list_events", json!({ "run_id": run_id })).await;
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

/// File an `escurel:run-control` request as `token`; its event id.
pub async fn control(gw: &EscurelProcess, token: &str, body: Value) -> String {
    let r = call(
        gw,
        token,
        "capture_event",
        json!({ "source": "workbench", "mime": "application/json",
                "label_skill": "escurel:run-control", "title": body["action"],
                "body": body.to_string() }),
    )
    .await;
    r["event_id"].as_str().unwrap().to_owned()
}

/// The `escurel:run-control-result` the runner wrote for `request_id`.
pub async fn wait_for_result(gw: &EscurelProcess, token: &str, request_id: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let r = call(
            gw,
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
