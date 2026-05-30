//! End-to-end test for the outbound capture webhook (M7-PR4,
//! DataZooDE/escurel#63 sibling — event-sourcing surface).
//!
//! Real running gateway + real Indexer (DuckDB + FsStore +
//! ZeroEmbedder), with `webhook_url` pointed at a real wiremock HTTP
//! sink. Captures an event over MCP-over-HTTP and asserts the gateway
//! delivers a fire-and-forget POST carrying the captured event's JSON.
//! No mocks at the boundary under test — a real HTTP server receives
//! the real POST.

use std::time::Duration;

use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TENANT: &str = "carl";

async fn call_mcp(p: &EscurelProcess, role: Role, name: &str, args: Value) -> Value {
    let token = p.mint_token(TENANT, role);
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
    assert_eq!(resp.status(), 200, "http status");
    let body: Value = resp.json().await.unwrap();
    if body.get("error").is_some() {
        panic!("tool {name} returned error: {body}");
    }
    body["result"].clone()
}

#[tokio::test]
async fn capture_event_delivers_outbound_webhook() {
    // A real HTTP sink that accepts the webhook POST.
    let sink = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/hook"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&sink)
        .await;

    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(FixtureBuilder::new().tenant(TENANT).done()),
        config_overrides: ConfigOverrides {
            webhook_url: Some(format!("{}/hook", sink.uri())),
            ..Default::default()
        },
    })
    .await;

    // Capture an event into the inbox.
    let captured = call_mcp(
        &p,
        Role::Agent,
        "capture_event",
        json!({
            "source": "manual",
            "mime": "text/plain",
            "label_skill": "note",
            "title": "hello webhook",
            "body": "hello webhook"
        }),
    )
    .await;
    let event_id = captured["event_id"].as_str().expect("event_id").to_owned();

    // The POST is fire-and-forget; poll the sink until it lands.
    let mut delivered: Option<Value> = None;
    for _ in 0..60 {
        let reqs = sink.received_requests().await.unwrap_or_default();
        if let Some(r) = reqs.iter().find(|r| r.url.path() == "/hook") {
            delivered = Some(serde_json::from_slice(&r.body).expect("json body"));
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let body = delivered.expect("webhook POST was delivered to the sink");
    assert_eq!(body["event_id"].as_str(), Some(event_id.as_str()));
    assert_eq!(body["title"].as_str(), Some("hello webhook"));
    assert_eq!(body["status"].as_str(), Some("inbox"));

    p.shutdown().await;
}

#[tokio::test]
async fn capture_event_without_webhook_url_is_a_noop() {
    // No webhook_url configured → capture still succeeds, nothing fired.
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(FixtureBuilder::new().tenant(TENANT).done()),
        config_overrides: ConfigOverrides::default(),
    })
    .await;

    let captured = call_mcp(
        &p,
        Role::Agent,
        "capture_event",
        json!({ "source": "manual", "title": "no hook", "body": "no hook" }),
    )
    .await;
    assert_eq!(captured["status"].as_str(), Some("inbox"));

    p.shutdown().await;
}
