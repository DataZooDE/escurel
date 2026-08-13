//! E2E for the SQL-view binding-health surfaces (PR-2e): the admin
//! `validate_bindings` tool and `expand`'s fail-closed-on-drift branch.
//! Real gateway + DuckDB + OIDC, offline json_dir.

use std::path::PathBuf;
use std::sync::Arc;

use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::backend::{SqlConnector, SqlViewBackend, SqlViewBinding};
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, LaneStore};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, Opts, Role};
use serde_json::{Value, json};
use tempfile::TempDir;

const TENANT: &str = "acme";

async fn call(p: &EscurelProcess, role: Role, name: &str, args: Value) -> Value {
    let token = p.mint_token(TENANT, role);
    reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": args },
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

#[tokio::test]
async fn validate_bindings_reports_drift_and_expand_fails_closed() {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let data_dir = TempDir::new().unwrap();
    let data_path: PathBuf = data_dir.path().to_path_buf();
    std::fs::write(
        data_path.join("a.json"),
        br#"{"name":"Acme","tier":"gold"}"#,
    )
    .unwrap();

    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Arc::new(Indexer::new(store, embedder, conn, TENANT).unwrap());

    let m = SqlViewBackend::new(Arc::clone(&indexer))
        .create_instance(
            "customers",
            &SqlViewBinding {
                connector: SqlConnector::JsonDir,
                attach: None,
                relation: data_path.to_str().unwrap().to_owned(),
                filter: None,
                project: Default::default(),
                search_text: Vec::new(),
            },
            "eu",
            "# EU",
        )
        .await
        .unwrap();

    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            indexer: Some(Arc::clone(&indexer)),
            ..Default::default()
        },
        ..Default::default()
    })
    .await;

    // Healthy initially.
    let healthy = call(&p, Role::Admin, "validate_bindings", json!({})).await;
    let r = &healthy["result"]["structuredContent"];
    assert_eq!(r["ok"], true, "fresh bindings healthy: {healthy}");
    assert_eq!(r["degraded"], 0);

    // A non-admin cannot run it.
    let denied = call(&p, Role::Agent, "validate_bindings", json!({})).await;
    assert!(denied.get("error").is_some(), "agent must be rejected");

    // Drift the source schema (drop `tier`).
    std::fs::write(data_path.join("a.json"), br#"{"name":"Acme"}"#).unwrap();

    // validate_bindings now reports the drift.
    let drifted = call(&p, Role::Admin, "validate_bindings", json!({})).await;
    let r = &drifted["result"]["structuredContent"];
    assert_eq!(r["ok"], false, "drift should be reported: {drifted}");
    assert_eq!(r["bindings"][0]["status"], "binding_degraded");

    // expand fails closed: the projection carries a binding_degraded Issue,
    // no rows (never wrong rows).
    let expanded = call(&p, Role::Admin, "expand", json!({ "page_id": m.page_id })).await;
    let proj = &expanded["result"]["structuredContent"]["backend_projection"];
    assert_eq!(proj["issue"]["code"], "binding_degraded", "got {expanded}");
    assert_eq!(proj["rows"].as_array().unwrap().len(), 0);

    p.shutdown().await;
    drop(data_dir);
}
