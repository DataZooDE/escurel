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
            disable_grpc: true,
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
    let r = &body["result"];
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
    let r = &body["result"];
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
    let rows = body["result"]["rows"].as_array().expect("rows array");
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
    assert_eq!(del["result"]["deleted"], 1);

    // Re-read via the agent list_messages tool: empty now.
    let list = call(
        &p,
        Role::Agent,
        "list_messages",
        json!({"chat_group_id": "room-1", "direction": "asc"}),
    )
    .await;
    assert_eq!(list["result"]["messages"].as_array().unwrap().len(), 0);
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
    let lanes = env["result"]["lanes"].as_array().expect("lanes");
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
    assert_eq!(blob["result"]["content_type"], "text/markdown");
    assert!(
        blob["result"]["bytes_base64"]
            .as_str()
            .is_some_and(|s| !s.is_empty()),
        "bytes_base64 present: {blob}"
    );

    // Agent role is rejected.
    let denied = call(&p, Role::Agent, "admin_list_lanes", json!({})).await;
    assert!(denied.get("error").is_some(), "agent denied: {denied}");

    p.shutdown().await;
}
