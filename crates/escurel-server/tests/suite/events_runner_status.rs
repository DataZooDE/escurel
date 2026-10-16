//! `report_runner_status`: a runner's health as ONE overwritten record
//! (`runner-status:<runner_id>`, label `escurel:runner-status`) instead of a
//! heartbeat event per tick. Real gateway, real DuckDB, raw JSON-RPC.
//!
//! The record is bookkeeping the runner and the gateway own: the same
//! bookkeeper gate as a reserved-label `capture_event`, and the reserved id
//! prefix is the tool's alone. It announces nothing (no webhook, no WS fan-out):
//! nothing acts on a heartbeat.

use std::time::Duration;

use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

const TENANT: &str = "carl";
const LABEL: &str = "escurel:runner-status";

async fn start_with(overrides: ConfigOverrides) -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: overrides,
        fixtures: Some(FixtureBuilder::new().tenant(TENANT).done()),
    })
    .await
}

async fn start() -> EscurelProcess {
    start_with(ConfigOverrides::default()).await
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

async fn call(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    let body = rpc(p, token, name, args).await;
    assert!(body.get("error").is_none(), "{name}: {body}");
    body["result"]["structuredContent"].clone()
}

fn report(runner: &str, state: &str, n: u32) -> Value {
    json!({
        "runner_id": runner,
        "state": state,
        "status_interval_s": 300,
        "started_at": "2026-10-16T08:00:00Z",
        "last_change_at": "2026-10-16T08:00:00Z",
        "body": json!({ "runner_id": runner, "version": "t", "live_runs": [], "n": n }).to_string(),
    })
}

async fn label_rows(p: &EscurelProcess, token: &str) -> Vec<Value> {
    let all = call(
        p,
        token,
        "list_events",
        json!({ "label_skill": LABEL, "newest_first": true, "limit": 1000, "include_system": true }),
    )
    .await;
    all["events"].as_array().unwrap().clone()
}

#[tokio::test]
async fn report_runner_status_overwrites_and_lists_first() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);

    // The old stream: heartbeats of this runner and of another one.
    for i in 0..6 {
        call(
            &p,
            &admin,
            "capture_event",
            json!({ "kind": "system", "source": "escurel-runner", "mime": "application/json",
                    "label_skill": LABEL, "title": "heartbeat",
                    "at": format!("2026-09-22T11:00:{i:02}Z"),
                    "provenance": { "runner_status": { "runner_id": if i < 5 { "r1" } else { "r2" } } },
                    "body": json!({ "old": i }).to_string() }),
        )
        .await;
    }

    let first = call(
        &p,
        &admin,
        "report_runner_status",
        report("r1", "started", 0),
    )
    .await;
    assert_eq!(first["event_id"], "runner-status:r1", "{first}");
    assert_eq!(first["created"], true);
    let rows = label_rows(&p, &admin).await;
    assert_eq!(
        rows.len(),
        2,
        "r1's five legacy rows are gone, r2's stays: {rows:?}"
    );

    for n in 1..=30 {
        let r = call(
            &p,
            &admin,
            "report_runner_status",
            report("r1", "heartbeat", n),
        )
        .await;
        assert_eq!(r["created"], false);
    }
    let rows = label_rows(&p, &admin).await;
    assert_eq!(rows.len(), 2, "30 more reports add no row: {rows:?}");
    let record = &rows[0];
    assert_eq!(
        record["event_id"], "runner-status:r1",
        "newest first: {rows:?}"
    );
    assert_eq!(record["title"], "heartbeat", "the title is the state");
    assert_eq!(record["kind"], "system");
    assert_eq!(record["provenance"]["runner_status"]["runner_id"], "r1");
    let body: Value = serde_json::from_str(record["body"].as_str().unwrap()).unwrap();
    assert_eq!(body["n"], 30, "{body}");
    assert_eq!(body["state"], "heartbeat");
    assert_eq!(body["status_interval_s"], 300);
    assert_eq!(body["started_at"], "2026-10-16T08:00:00Z");
    assert_eq!(body["last_change_at"], "2026-10-16T08:00:00Z");

    // The same read the workbench makes: the label's newest row, one of them.
    let latest = call(
        &p,
        &admin,
        "list_events",
        json!({ "label_skill": LABEL, "newest_first": true, "limit": 1, "include_system": true }),
    )
    .await;
    assert_eq!(latest["events"][0]["event_id"], "runner-status:r1");

    // It never entered the inbox.
    let inbox = call(&p, &admin, "list_inbox", json!({})).await;
    assert!(inbox["events"].as_array().unwrap().is_empty(), "{inbox}");
}

#[tokio::test]
async fn only_the_bookkeeper_reports_and_only_this_tool_mints_the_reserved_id() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);
    let agent = p.mint_token(TENANT, Role::Agent);

    let denied = rpc(
        &p,
        &agent,
        "report_runner_status",
        report("r1", "started", 0),
    )
    .await;
    assert!(
        denied.get("error").is_some(),
        "an agent token may not report: {denied}"
    );
    assert!(label_rows(&p, &admin).await.is_empty());

    // The reserved id cannot be minted through capture_event, even by the admin.
    for event_id in ["runner-status:r1", "runner-status:"] {
        let r = rpc(
            &p,
            &admin,
            "capture_event",
            json!({ "event_id": event_id, "kind": "system", "label_skill": LABEL,
                    "source": "x", "title": "forged", "body": "{}" }),
        )
        .await;
        assert!(
            r["error"]["message"]
                .as_str()
                .is_some_and(|m| m.contains("runner-status:") && m.contains("reserved")),
            "{event_id}: {r}"
        );
    }
    assert!(label_rows(&p, &admin).await.is_empty());

    // Malformed reports are refused, nothing is stored.
    for bad in [
        json!({ "runner_id": "", "state": "heartbeat", "status_interval_s": 300, "body": "{}" }),
        json!({ "runner_id": "a b", "state": "heartbeat", "status_interval_s": 300, "body": "{}" }),
        json!({ "runner_id": "r1", "state": "sleeping", "status_interval_s": 300, "body": "{}" }),
        json!({ "runner_id": "r1", "state": "heartbeat", "status_interval_s": 0, "body": "{}" }),
        json!({ "runner_id": "r1", "state": "heartbeat", "status_interval_s": 300, "body": "[1]" }),
        json!({ "runner_id": "r1", "state": "heartbeat", "status_interval_s": 300, "body": "nope" }),
    ] {
        let r = rpc(&p, &admin, "report_runner_status", bad.clone()).await;
        assert_eq!(r["error"]["code"], -32602, "{bad}: {r}");
    }
    assert!(label_rows(&p, &admin).await.is_empty());
}

#[tokio::test]
async fn a_status_report_announces_nothing() {
    let sink = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&sink)
        .await;
    let p = start_with(ConfigOverrides {
        webhook_url: Some(format!("{}/hook", sink.uri())),
        ..Default::default()
    })
    .await;
    let admin = p.mint_token(TENANT, Role::Admin);

    for n in 0..3 {
        call(
            &p,
            &admin,
            "report_runner_status",
            report("r1", "heartbeat", n),
        )
        .await;
    }
    // Control: an ordinary capture does reach the hook, so a silent sink is meaningful.
    call(
        &p,
        &admin,
        "capture_event",
        json!({ "source": "manual", "mime": "text/plain", "label_skill": "email",
                "title": "hello", "body": "b" }),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let received = sink.received_requests().await.unwrap();
    assert_eq!(
        received.len(),
        1,
        "only the ordinary capture was announced, not the three status reports: {received:?}"
    );
}

#[tokio::test]
async fn the_tool_is_advertised() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);
    let list: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {admin}"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let names: Vec<_> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert!(names.contains(&"report_runner_status"), "{names:?}");
}
