//! End-to-end tests for the admin-gated MCP tools
//! (`admin_quota`, `admin_audit`, `admin_index_query`,
//! `admin_delete_chat_history`).
//!
//! These mirror the documented MCP admin surface and delegate to the
//! same logic the gRPC `EscurelAdmin` service uses. Real gateway,
//! real DuckDB, real OIDC (TestIssuer JWKS), real reqwest over
//! `POST /mcp`. The role gate is exercised with a genuine agent-role
//! token (must be rejected) and an admin-role token (must succeed).

use std::sync::Arc;

use escurel_quota::{QuotaConfig, QuotaManager};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "acme";

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(FixtureBuilder::new().tenant(TENANT).done()),
        config_overrides: ConfigOverrides {
            quota: Some(Arc::new(QuotaManager::new(QuotaConfig::defaults()))),
            ..Default::default()
        },
    })
    .await
}

/// POST a tools/call and return the full JSON-RPC envelope (so a test
/// can assert on either `result` or `error`).
async fn call(p: &EscurelProcess, role: Role, name: &str, args: Value) -> Value {
    let token = p.mint_token(TENANT, role);
    reqwest::Client::new()
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
        .expect("post")
        .json()
        .await
        .expect("json")
}

async fn append_chat(p: &EscurelProcess, group: &str, content: &str, ts: &str) {
    let body = call(
        p,
        Role::Agent,
        "append_message",
        json!({"chat_group_id": group, "role": "user", "content": content, "ts": ts}),
    )
    .await;
    assert!(body.get("error").is_none(), "append failed: {body}");
}

#[tokio::test]
async fn admin_quota_returns_snapshot() {
    let p = start().await;
    let body = call(&p, Role::Admin, "admin_quota", json!({})).await;
    assert!(body.get("error").is_none(), "admin_quota error: {body}");
    let r = &body["result"]["structuredContent"];
    assert!(r["queries_remaining"].is_number());
    assert!(r["writes_remaining"].is_number());
    assert!(r["embeds_remaining"].is_number());
    assert!(r["concurrent_sessions_in_use"].is_number());
    p.shutdown().await;
}

#[tokio::test]
async fn admin_audit_returns_drift_lists() {
    let p = start().await;
    let body = call(&p, Role::Admin, "admin_audit", json!({})).await;
    assert!(body.get("error").is_none(), "admin_audit error: {body}");
    let r = &body["result"]["structuredContent"];
    assert!(r["markdown_not_in_duckdb"].is_array());
    assert!(r["indexed_but_no_markdown"].is_array());
    p.shutdown().await;
}

#[tokio::test]
async fn admin_index_query_reads_chat_messages() {
    let p = start().await;
    append_chat(&p, "room-1", "hello ops", "2026-05-26T10:00:00Z").await;

    let body = call(
        &p,
        Role::Admin,
        "admin_index_query",
        json!({"table": "chat_messages", "limit": 50}),
    )
    .await;
    assert!(
        body.get("error").is_none(),
        "admin_index_query error: {body}"
    );
    let rows = body["result"]["structuredContent"]["rows"]
        .as_array()
        .expect("rows array");
    assert!(
        rows.iter().any(|row| row["content"] == "hello ops"),
        "expected the appended message in the table read: {rows:?}",
    );
    // dense_vec must be excluded from the projection (heavy column).
    assert!(
        rows.iter().all(|row| row.get("dense_vec").is_none()),
        "dense_vec must be excluded from inspect_table output",
    );
    p.shutdown().await;
}

#[tokio::test]
async fn admin_index_query_rejects_unknown_table() {
    let p = start().await;
    let body = call(
        &p,
        Role::Admin,
        "admin_index_query",
        json!({"table": "secrets"}),
    )
    .await;
    assert!(
        body.get("error").is_some(),
        "unknown table must error: {body}"
    );
    p.shutdown().await;
}

#[tokio::test]
async fn admin_delete_chat_history_purges_then_reads_empty() {
    let p = start().await;
    append_chat(&p, "room-1", "to be deleted", "2026-05-26T10:00:00Z").await;

    let del = call(
        &p,
        Role::Admin,
        "admin_delete_chat_history",
        json!({"chat_group_id": "room-1"}),
    )
    .await;
    assert!(del.get("error").is_none(), "delete error: {del}");
    assert_eq!(del["result"]["structuredContent"]["deleted"], 1);

    // Re-read via the agent list_messages tool: empty now.
    let list = call(
        &p,
        Role::Agent,
        "list_messages",
        json!({"chat_group_id": "room-1", "direction": "asc"}),
    )
    .await;
    assert_eq!(
        list["result"]["structuredContent"]["messages"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    p.shutdown().await;
}

#[tokio::test]
async fn agent_role_is_rejected_from_every_admin_tool() {
    let p = start().await;
    for (name, args) in [
        ("admin_quota", json!({})),
        ("admin_audit", json!({})),
        ("admin_index_query", json!({"table": "pages"})),
        (
            "admin_delete_chat_history",
            json!({"chat_group_id": "room-1"}),
        ),
        (
            "admin_purge_events",
            json!({"label_skill": "escurel:runner-status", "keep_latest": 0, "apply": true}),
        ),
    ] {
        let body = call(&p, Role::Agent, name, args).await;
        let err = body
            .get("error")
            .unwrap_or_else(|| panic!("{name} must reject agent role: {body}"));
        assert!(
            err["message"]
                .as_str()
                .unwrap_or_default()
                .contains("admin"),
            "{name} error should mention admin role: {err}",
        );
    }
    p.shutdown().await;
}

#[tokio::test]
async fn admin_lane_tools_over_mcp() {
    let p = start().await;

    // list_lanes: one markdown/fs lane.
    let env = call(&p, Role::Admin, "admin_list_lanes", json!({})).await;
    let lanes = env["result"]["structuredContent"]["lanes"]
        .as_array()
        .expect("lanes");
    assert_eq!(lanes.len(), 1);
    assert_eq!(lanes[0]["name"], "markdown");
    assert_eq!(lanes[0]["backend"], "fs");

    // lane_blob: the meta-skill markdown (base64) with its content type.
    let blob = call(
        &p,
        Role::Admin,
        "admin_lane_blob",
        json!({ "key": "markdown/skills/escurel.md" }),
    )
    .await;
    assert_eq!(
        blob["result"]["structuredContent"]["content_type"],
        "text/markdown"
    );
    assert!(
        blob["result"]["structuredContent"]["bytes_base64"]
            .as_str()
            .is_some_and(|s| !s.is_empty()),
        "bytes_base64 present: {blob}"
    );

    // Agent role is rejected.
    let denied = call(&p, Role::Agent, "admin_list_lanes", json!({})).await;
    assert!(denied.get("error").is_some(), "agent denied: {denied}");

    p.shutdown().await;
}

async fn capture_status(p: &EscurelProcess, n: u32) {
    let body = call(
        p,
        Role::Admin,
        "capture_event",
        json!({ "kind": "system", "source": "escurel-runner", "mime": "application/json",
                "label_skill": "escurel:runner-status", "title": "heartbeat",
                "at": format!("2026-09-{n:02}T10:00:00Z"), "body": json!({ "n": n }).to_string() }),
    )
    .await;
    assert!(body.get("error").is_none(), "capture failed: {body}");
}

async fn status_rows(p: &EscurelProcess) -> usize {
    let body = call(
        p,
        Role::Admin,
        "list_events",
        json!({ "label_skill": "escurel:runner-status", "limit": 1000 }),
    )
    .await;
    body["result"]["structuredContent"]["events"]
        .as_array()
        .unwrap()
        .len()
}

#[tokio::test]
async fn admin_purge_events_counts_by_default_then_deletes_what_it_counted() {
    let p = start().await;
    for n in 1..=10 {
        capture_status(&p, n).await;
    }

    // The default is a dry run: counted, nothing deleted.
    let dry = call(
        &p,
        Role::Admin,
        "admin_purge_events",
        json!({"label_skill": "escurel:runner-status", "before_ts": "2026-09-06T00:00:00Z", "keep_latest": 2}),
    )
    .await;
    assert!(dry.get("error").is_none(), "dry run: {dry}");
    let d = &dry["result"]["structuredContent"];
    assert_eq!(
        (
            d["total"].as_u64(),
            d["matched"].as_u64(),
            d["deleted"].as_u64()
        ),
        (Some(10), Some(5), Some(0))
    );
    assert_eq!(d["dry_run"], true);
    assert_eq!(status_rows(&p).await, 10);

    let done = call(
        &p,
        Role::Admin,
        "admin_purge_events",
        json!({"label_skill": "escurel:runner-status", "before_ts": "2026-09-06T00:00:00Z",
               "keep_latest": 2, "apply": true}),
    )
    .await;
    let r = &done["result"]["structuredContent"];
    assert_eq!(
        (
            r["matched"].as_u64(),
            r["deleted"].as_u64(),
            r["dry_run"].as_bool()
        ),
        (Some(5), Some(5), Some(false))
    );
    let audit = r["audit_event_id"].as_str().expect("audit event id");
    assert_eq!(status_rows(&p).await, 5);

    // The trail is a system event under its own label.
    let trail = call(
        &p,
        Role::Admin,
        "list_events",
        json!({ "label_skill": "escurel:event-purge", "include_system": true }),
    )
    .await;
    let ev = &trail["result"]["structuredContent"]["events"][0];
    assert_eq!(ev["event_id"], audit);
    p.shutdown().await;
}

#[tokio::test]
async fn admin_purge_events_refuses_an_unbounded_or_audit_or_foreign_request() {
    let p = start().await;
    capture_status(&p, 1).await;
    let refused = |body: &Value, needle: &str| {
        let msg = body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        assert!(msg.contains(needle), "expected `{needle}` in: {body}");
    };
    // No bound: never "every row of the label" by omission.
    let r = call(
        &p,
        Role::Admin,
        "admin_purge_events",
        json!({"label_skill": "escurel:runner-status", "apply": true}),
    )
    .await;
    refused(&r, "before_ts and/or keep_latest");
    // An audit label needs allow_protected.
    let r = call(
        &p,
        Role::Admin,
        "admin_purge_events",
        json!({"label_skill": "escurel:write-back", "keep_latest": 0, "apply": true}),
    )
    .await;
    refused(&r, "audit trail");
    // A malformed cutoff.
    let r = call(
        &p,
        Role::Admin,
        "admin_purge_events",
        json!({"label_skill": "x", "before_ts": "yesterday"}),
    )
    .await;
    refused(&r, "RFC 3339");
    // A foreign tenant.
    let r = call(
        &p,
        Role::Admin,
        "admin_purge_events",
        json!({"tenant_id": "someone-else", "label_skill": "x", "keep_latest": 0}),
    )
    .await;
    refused(&r, "does not match");
    assert_eq!(
        status_rows(&p).await,
        1,
        "nothing was deleted by any refused request"
    );
    p.shutdown().await;
}
