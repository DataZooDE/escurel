//! Shared scaffolding for the remote-connector tests: a real gateway over a real DuckDB + `FsStore`,
//! `tools/call` over HTTP, and real upstream servers on loopback sockets.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_server::egress::EgressPolicy;
use escurel_storage::{FsStore, LaneStore};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, Opts, Role};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::net::TcpListener;

pub const TENANT: &str = "acme";

/// A gateway whose indexer already holds `skills` (id, markdown), with `egress` as its policy.
pub async fn spawn_gateway(
    skills: &[(&str, &str)],
    egress: EgressPolicy,
) -> (EscurelProcess, Vec<TempDir>) {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Arc::new(Indexer::new(store, embedder, conn, TENANT).unwrap());
    for (id, md) in skills {
        indexer
            .update_page(&format!("markdown/skills/{id}.md"), md)
            .await
            .unwrap();
    }
    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            indexer: Some(indexer),
            egress: Some(egress),
            ..Default::default()
        },
        ..Default::default()
    })
    .await;
    (process, vec![store_dir, db_dir])
}

pub fn loopback_ok() -> EgressPolicy {
    EgressPolicy {
        allow_loopback: true,
        ..EgressPolicy::default()
    }
}

/// `tools/call` as `role`; returns the JSON-RPC envelope.
pub async fn call_as(p: &EscurelProcess, role: Role, name: &str, args: Value) -> Value {
    let token = p.mint_token(TENANT, role);
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

/// `tools/call` as admin; returns `result.structuredContent` (panics on a JSON-RPC error).
pub async fn admin(p: &EscurelProcess, name: &str, args: Value) -> Value {
    let v = call_as(p, Role::Admin, name, args).await;
    assert!(v.get("error").is_none(), "{name}: {v}");
    v["result"]["structuredContent"].clone()
}

pub async fn serve(app: Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), handle)
}
