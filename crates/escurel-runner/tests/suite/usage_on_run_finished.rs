//! `run-finished` carries the run's token usage and cost (knowledge-workbench
//! backend P3-4 — BRD FR-O-3).
//!
//! Real gateway, real runner binary, real echo harness. Every adapter parses
//! what its harness reports into `HarnessOutcome.usage`; the runner sums it
//! across attempts and writes it as `run-finished.body.usage`, and meters it
//! as `escurel_runner_tokens_total{tenant,kind}` /
//! `escurel_runner_cost_usd_total{tenant}`. The echo reports a synthetic usage
//! under `ESCUREL_ECHO_USAGE` (a test knob; unset ⇒ `null`, as for any harness
//! that reports nothing).

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

fn spawn_runner(gw: &EscurelProcess, extra: &[(&str, &str)]) -> (ChildGuard, String) {
    let listen = format!("127.0.0.1:{}", free_port());
    let ledger_dir = tempfile::tempdir().expect("tempdir");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gw.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        // Admin: the `escurel:` namespace is admin-only to write.
        .env("ESCUREL_RUNNER_TOKEN", gw.mint_token(TENANT, Role::Admin))
        .env("ESCUREL_RUNNER_HARNESS", "echo")
        .env(
            "ESCUREL_RUNNER_LEDGER_PATH",
            ledger_dir.keep().join("ledger.duckdb"),
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

/// The run's `run-finished` body — written best-effort AFTER the ledger
/// terminal, so poll for it.
async fn run_finished(gw: &EscurelProcess, run_id: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let r = call(gw, Role::Admin, "list_events", json!({ "run_id": run_id })).await;
        let events = r["events"].as_array().cloned().unwrap_or_default();
        if let Some(e) = events.iter().find(|e| e["title"] == "run-finished") {
            return serde_json::from_str(e["body"].as_str().unwrap_or("{}")).unwrap_or_default();
        }
        assert!(
            Instant::now() < deadline,
            "no run-finished for {run_id}: {events:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn run_finished_carries_the_harness_usage_and_the_runner_meters_it() {
    let gw = gateway().await;
    let event_id = capture(&gw).await;
    let (_runner, listen) = spawn_runner(&gw, &[("ESCUREL_ECHO_USAGE", "120,45,0.0031")]);
    let (run_id, status) = await_terminal(&listen, &event_id).await;
    assert_eq!(status, "processed");

    let finished = run_finished(&gw, &run_id).await;
    assert_eq!(
        finished["usage"],
        json!({ "input_tokens": 120, "output_tokens": 45, "cost_usd": 0.0031, "model": "echo" }),
        "{finished}"
    );

    let metrics = reqwest::get(format!("http://{listen}/metrics"))
        .await
        .expect("metrics")
        .text()
        .await
        .expect("text");
    for line in [
        "escurel_runner_tokens_total{kind=\"input\",tenant=\"acme\"} 120",
        "escurel_runner_tokens_total{kind=\"output\",tenant=\"acme\"} 45",
        "escurel_runner_cost_usd_total{tenant=\"acme\"} 0.0031",
    ] {
        assert!(metrics.contains(line), "missing `{line}` in:\n{metrics}");
    }
}

#[tokio::test]
async fn a_harness_that_reports_no_usage_leaves_usage_null() {
    let gw = gateway().await;
    let event_id = capture(&gw).await;
    let (_runner, listen) = spawn_runner(&gw, &[]);
    let (run_id, status) = await_terminal(&listen, &event_id).await;
    assert_eq!(status, "processed");

    let finished = run_finished(&gw, &run_id).await;
    assert!(
        finished.get("usage").is_some_and(Value::is_null),
        "usage is present and null when nothing was reported: {finished}"
    );
}
