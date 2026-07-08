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
use hmac::{Hmac, Mac};
use serde_json::{Value, json};
use sha2::Sha256;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

type HmacSha256 = Hmac<Sha256>;

/// Tenant used for the auth token. NOTE: the test-support gateway's
/// `Indexer` is single-tenant and hardwired to `"acme"` regardless of the
/// token/fixture tenant, so the authoritative `tenant_id` the gateway
/// stamps into the webhook payload (`indexer.tenant()`) is always
/// [`GATEWAY_TENANT`], not this value.
const TENANT: &str = "acme"; // aligned to the served indexer; tenant boundary now enforced

/// The gateway's authoritative single-tenant identity (`indexer.tenant()`
/// in the test-support harness). This is what rides in the webhook
/// payload's `tenant_id`.
const GATEWAY_TENANT: &str = "acme";

/// Compute the expected `sha256=<hex>` signature for `body` under `secret`,
/// matching the gateway's `X-Escurel-Webhook-Signature` scheme.
fn expected_signature(secret: &str, body: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("hmac key of any size");
    mac.update(body);
    let bytes = mac.finalize().into_bytes();
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256={hex}")
}

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
    // tools/call results are MCP-shaped; the payload is under
    // `structuredContent`.
    body["result"]["structuredContent"].clone()
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
    // Even without a secret the payload always carries the authoritative
    // gateway tenant (`indexer.tenant()`).
    assert_eq!(body["tenant_id"].as_str(), Some(GATEWAY_TENANT));

    p.shutdown().await;
}

#[tokio::test]
async fn capture_event_webhook_is_hmac_signed_and_carries_tenant_id() {
    // A real HTTP sink that accepts the webhook POST and records it.
    let sink = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/hook"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&sink)
        .await;

    const SECRET: &str = "sup3r-s3cret-hmac-key";

    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(FixtureBuilder::new().tenant(TENANT).done()),
        config_overrides: ConfigOverrides {
            webhook_url: Some(format!("{}/hook", sink.uri())),
            webhook_secret: Some(SECRET.to_owned()),
            ..Default::default()
        },
    })
    .await;

    let captured = call_mcp(
        &p,
        Role::Agent,
        "capture_event",
        json!({
            "source": "manual",
            "mime": "text/plain",
            "label_skill": "note",
            "title": "signed webhook",
            "body": "signed webhook"
        }),
    )
    .await;
    let event_id = captured["event_id"].as_str().expect("event_id").to_owned();

    // Poll the real sink until the fire-and-forget POST lands.
    let mut received: Option<(Vec<u8>, String)> = None;
    for _ in 0..60 {
        let reqs = sink.received_requests().await.unwrap_or_default();
        if let Some(r) = reqs.iter().find(|r| r.url.path() == "/hook") {
            let sig = r
                .headers
                .get("X-Escurel-Webhook-Signature")
                .map(|v| v.to_str().expect("ascii signature").to_owned())
                .expect("signature header present");
            received = Some((r.body.clone(), sig));
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let (raw_body, signature) = received.expect("signed webhook POST was delivered");

    // The signature validates as HMAC-SHA256 of the *exact raw body bytes*.
    assert_eq!(
        signature,
        expected_signature(SECRET, &raw_body),
        "X-Escurel-Webhook-Signature must be HMAC-SHA256 of the raw body"
    );

    // The payload carries the gateway's tenant + the captured event.
    let body: Value = serde_json::from_slice(&raw_body).expect("json body");
    assert_eq!(
        body["tenant_id"].as_str(),
        Some(GATEWAY_TENANT),
        "payload must carry the gateway's authoritative tenant_id"
    );
    assert_eq!(body["event_id"].as_str(), Some(event_id.as_str()));
    assert_eq!(body["title"].as_str(), Some("signed webhook"));

    p.shutdown().await;
}

#[tokio::test]
async fn delivery_log_records_outbound_webhook_outcome() {
    // A real sink that 200s the POST.
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

    let captured = call_mcp(
        &p,
        Role::Agent,
        "capture_event",
        json!({ "source": "manual", "label_skill": "note", "title": "logged", "body": "logged" }),
    )
    .await;
    let event_id = captured["event_id"].as_str().expect("event_id").to_owned();

    // Poll the admin delivery log until the fire-and-forget POST is recorded.
    let mut found: Option<Value> = None;
    for _ in 0..60 {
        let out = call_mcp(&p, Role::Admin, "admin_webhook_deliveries", json!({})).await;
        assert_eq!(
            out["configured"].as_bool(),
            Some(true),
            "webhook configured"
        );
        if let Some(rec) = out["deliveries"]
            .as_array()
            .and_then(|a| a.iter().find(|d| d["event_id"].as_str() == Some(&event_id)))
        {
            found = Some(rec.clone());
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let rec = found.expect("delivery recorded in the admin log");
    assert_eq!(rec["ok"].as_bool(), Some(true), "200 sink → ok");
    assert_eq!(
        rec["http_status"].as_u64(),
        Some(200),
        "records the status code"
    );

    p.shutdown().await;
}

#[tokio::test]
async fn delivery_log_reports_unconfigured_when_no_webhook() {
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(FixtureBuilder::new().tenant(TENANT).done()),
        config_overrides: ConfigOverrides::default(),
    })
    .await;
    let out = call_mcp(&p, Role::Admin, "admin_webhook_deliveries", json!({})).await;
    assert_eq!(out["configured"].as_bool(), Some(false));
    assert_eq!(out["deliveries"].as_array().map(|a| a.len()), Some(0));
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
