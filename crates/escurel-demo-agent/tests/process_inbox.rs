//! No-mock E2E for the demo agent's fold (M7-PR5).
//!
//! Real running gateway + real Indexer (DuckDB + FsStore +
//! ZeroEmbedder). The agent captures events into the inbox over the
//! real `/mcp` surface, runs `process_inbox_once` against that same
//! gateway, and asserts routable events are folded into their instances
//! (moved out of the inbox and into the instance's `list_events`
//! history) while unroutable ones are left in the inbox.

use std::collections::HashMap;

use escurel_demo_agent::{McpClient, process_inbox_once};
use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "carl";
const SPINE: &str = "markdown/instances/engagement__spine.md";

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(FixtureBuilder::new().tenant(TENANT).done()),
        ..Default::default()
    })
    .await
}

/// Raw `/mcp` tool call (the agent's `McpClient` only wraps inbox/assign;
/// the test drives capture + list_events directly).
async fn call(p: &EscurelProcess, name: &str, args: Value) -> Value {
    let token = p.mint_token(TENANT, Role::Agent);
    let resp = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": name, "arguments": args },
        }))
        .send()
        .await
        .expect("post");
    let body: Value = resp.json().await.unwrap();
    assert!(body.get("error").is_none(), "tool {name} error: {body}");
    body["result"].clone()
}

async fn capture(p: &EscurelProcess, args: Value) {
    call(p, "capture_event", args).await;
}

async fn inbox_len(p: &EscurelProcess) -> usize {
    let result = call(p, "list_inbox", json!({})).await;
    result["events"].as_array().map_or(0, Vec::len)
}

async fn events_len(p: &EscurelProcess, instance: &str) -> usize {
    let result = call(p, "list_events", json!({ "instance_page_id": instance })).await;
    result["events"].as_array().map_or(0, Vec::len)
}

fn agent(p: &EscurelProcess) -> McpClient {
    McpClient::new(p.mcp_url(), p.mint_token(TENANT, Role::Agent))
}

#[tokio::test]
async fn folds_preflagged_events_into_their_instance() {
    let p = start().await;

    // Two events pre-flagged for the spine (gmail-label style), one for
    // a different instance.
    capture(
        &p,
        json!({ "source": "gmail", "label_skill": "gmail", "instance_page_id": SPINE,
                "title": "contact form" }),
    )
    .await;
    capture(
        &p,
        json!({ "source": "meet", "label_skill": "meet", "instance_page_id": SPINE,
                "title": "discovery call" }),
    )
    .await;

    assert_eq!(inbox_len(&p).await, 2, "both events start in the inbox");
    assert_eq!(events_len(&p, SPINE).await, 0, "none folded yet");

    let report = process_inbox_once(&agent(&p), &HashMap::new())
        .await
        .expect("process inbox");
    assert_eq!(report.assigned, 2);
    assert_eq!(report.skipped, 0);

    assert_eq!(inbox_len(&p).await, 0, "inbox drained after the fold");
    assert_eq!(events_len(&p, SPINE).await, 2, "both folded into the spine");

    p.shutdown().await;
}

#[tokio::test]
async fn routes_by_label_skill_and_skips_unroutable() {
    let p = start().await;

    // One event routable only by its label_skill (no pre-flag), one with
    // neither a pre-flag nor a matching route.
    capture(
        &p,
        json!({ "source": "gmail", "label_skill": "gmail", "title": "routed by skill" }),
    )
    .await;
    capture(
        &p,
        json!({ "source": "unknown", "label_skill": "mystery", "title": "no target" }),
    )
    .await;

    let routes = HashMap::from([("gmail".to_owned(), SPINE.to_owned())]);
    let report = process_inbox_once(&agent(&p), &routes)
        .await
        .expect("process inbox");
    assert_eq!(report.assigned, 1, "the gmail event folded via the route");
    assert_eq!(report.skipped, 1, "the mystery event left in the inbox");

    assert_eq!(inbox_len(&p).await, 1, "only the unroutable event remains");
    assert_eq!(events_len(&p, SPINE).await, 1);

    p.shutdown().await;
}
