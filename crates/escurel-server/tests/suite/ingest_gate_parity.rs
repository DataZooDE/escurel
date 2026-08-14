//! `/ingest` ↔ MCP dispatch-gate **parity** over real HTTP.
//!
//! The REST intake path used to re-implement the dispatch gate's
//! cross-cutting checks by hand — and diverged on each (2026-08-14 API
//! review, B1; escurel#382): no `event_id` passthrough, no
//! tenant-suspend check, no reader-mode check, and metrics counted
//! every outcome as a 200. These tests pin the parity contract: the two
//! intake doors give the same guarantees for the same act.

use std::sync::Arc;

use escurel_admin::{FsTenantStore, TenantStore};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};
use tempfile::TempDir;

const TENANT: &str = "stuttgart-ai";

async fn start(overrides: ConfigOverrides) -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: overrides,
        fixtures: Some(FixtureBuilder::new().tenant(TENANT).done()),
    })
    .await
}

async fn mcp(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    let resp = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": args },
        }))
        .send()
        .await
        .expect("post");
    assert_eq!(resp.status(), 200, "http status");
    resp.json().await.unwrap()
}

/// escurel#382: `/ingest/upload` must accept a caller-supplied
/// `event_id` and feed it to `capture_event`'s existing dedup, so a
/// redelivered byte-carrying share dedupes the *work*, not only the
/// page. Without it the server mints a fresh id per request and the
/// consumer transcribes/files the same recording twice.
#[tokio::test]
async fn redelivered_upload_with_event_id_is_one_event() {
    let p = start(ConfigOverrides::default()).await;
    let token = p.mint_token(TENANT, Role::Agent);
    let client = reqwest::Client::new();

    let body = json!({
        "content_type": "application/x-escurel-test",
        "bytes_b64": base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD, b"same recording bytes"),
        "title": "standup recording",
        "event_id": "client-idempotency-key-1",
    });

    // The delivery, then the offline queue's redelivery of identical bytes.
    for attempt in 1..=2u8 {
        let resp = client
            .post(format!("{}/ingest/upload", p.base_url()))
            .header("authorization", format!("Bearer {token}"))
            .json(&body)
            .send()
            .await
            .expect("post upload");
        let status = resp.status();
        let out: Value = resp.json().await.unwrap();
        assert!(
            status.is_success(),
            "attempt {attempt} must be accepted: {status} {out}"
        );
        assert_eq!(
            out["event_id"],
            json!("client-idempotency-key-1"),
            "attempt {attempt} must carry the caller's idempotency key: {out}"
        );
    }

    // The inbox holds ONE event for the pair of deliveries.
    let inbox = mcp(&p, &token, "list_inbox", json!({})).await;
    let events = inbox["result"]["structuredContent"]["events"]
        .as_array()
        .unwrap_or_else(|| panic!("inbox shape: {inbox}"));
    let matching: Vec<&Value> = events
        .iter()
        .filter(|e| e["event_id"] == json!("client-idempotency-key-1"))
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "one delivery + one redelivery must be ONE inbox event: {inbox}"
    );
    assert_eq!(
        events.len(),
        1,
        "no anonymous duplicate event may exist beside it: {inbox}"
    );
}

/// #247's suspend gate stops agent traffic at the MCP dispatch gate —
/// the REST intake door must not stay open when the front door is shut.
#[tokio::test]
async fn suspended_tenant_rejects_ingest() {
    // A real FsTenantStore so `tenant_update` has somewhere to persist
    // the suspend (mirrors mcp_admin_tools.rs).
    let tenants_dir = TempDir::new().unwrap();
    let tenant_store: Arc<dyn TenantStore> =
        Arc::new(FsTenantStore::new(tenants_dir.path().to_path_buf()));
    let p = start(ConfigOverrides {
        tenant_store: Some(tenant_store),
        ..Default::default()
    })
    .await;
    let admin = p.mint_token(TENANT, Role::Admin);
    let agent = p.mint_token(TENANT, Role::Agent);

    let created = mcp(
        &p,
        &admin,
        "tenant_create",
        json!({ "tenant_id": TENANT, "display_name": "Stuttgart AI" }),
    )
    .await;
    assert!(created.get("error").is_none(), "tenant_create: {created}");

    let susp = mcp(
        &p,
        &admin,
        "tenant_update",
        json!({ "tenant_id": TENANT, "status": "suspended" }),
    )
    .await;
    assert!(susp.get("error").is_none(), "suspend failed: {susp}");

    let resp = reqwest::Client::new()
        .post(format!("{}/ingest", p.base_url()))
        .header("authorization", format!("Bearer {agent}"))
        .json(&json!({ "blob_id": "inbox/nope", "content_type": "text/plain" }))
        .send()
        .await
        .expect("post ingest");
    assert_eq!(
        resp.status(),
        403,
        "a suspended tenant must reject agent ingest like it rejects agent tools: {}",
        resp.text().await.unwrap_or_default()
    );
}

/// A ducklake READER refuses mutating tools with a typed error so the
/// client retries against the writer — `/ingest` is a mutation and must
/// refuse the same way instead of accepting work it cannot own.
#[tokio::test]
async fn reader_replica_rejects_ingest() {
    let p = start(ConfigOverrides {
        reader_mode: true,
        ..Default::default()
    })
    .await;
    let token = p.mint_token(TENANT, Role::Agent);

    let resp = reqwest::Client::new()
        .post(format!("{}/ingest", p.base_url()))
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({ "blob_id": "inbox/nope", "content_type": "text/plain" }))
        .send()
        .await
        .expect("post ingest");
    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or_default();
    assert_eq!(
        status, 503,
        "a reader replica must refuse REST ingest, not half-accept it: {body}"
    );
    assert_eq!(
        body["error"],
        json!("read_only_replica"),
        "typed refusal so the client knows to retry against the writer: {body}"
    );
}

/// The `(route, status)` metrics axis must record what actually
/// happened: an unauthenticated `/ingest` is a 401, not a 200.
#[tokio::test]
async fn ingest_metrics_record_the_actual_status() {
    let p = start(ConfigOverrides::default()).await;

    // No bearer at all → the gate refuses.
    let resp = reqwest::Client::new()
        .post(format!("{}/ingest", p.base_url()))
        .json(&json!({ "blob_id": "inbox/nope", "content_type": "text/plain" }))
        .send()
        .await
        .expect("post ingest");
    assert_eq!(resp.status(), 401, "unauthenticated ingest must 401");

    let metrics = reqwest::get(p.metrics_url().expect("metrics listener"))
        .await
        .expect("scrape")
        .text()
        .await
        .expect("metrics body");
    let count = |status: &str| -> f64 {
        metrics
            .lines()
            .find(|l| {
                l.contains("route=\"/ingest\"") && l.contains(&format!("status=\"{status}\""))
            })
            .and_then(|l| l.split_whitespace().last())
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.0)
    };
    assert_eq!(
        count("200"),
        0.0,
        "a refused request must not be counted as a 200:\n{metrics}"
    );
    assert!(
        count("401") >= 1.0,
        "the 401 must be recorded on the /ingest route:\n{metrics}"
    );
}
