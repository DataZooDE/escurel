//! End-to-end tests for the SQL-view read path + read-only write guard
//! (PR-2c). Real gateway, real DuckDB, real `FsStore`, real reqwest over
//! `POST /mcp`. A SQL-view instance is materialised through the same
//! `SqlViewBackend` the create path uses (over an offline `json_dir`), then
//! `expand` / `update_page` are driven over the wire.

use std::sync::Arc;

use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::backend::{SqlConnector, SqlViewBackend, SqlViewBinding};
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, LaneStore};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, Opts};
use serde_json::{Value, json};
use tempfile::TempDir;

const TENANT: &str = "acme";

struct Setup {
    process: EscurelProcess,
    page_id: String,
    _dirs: Vec<TempDir>,
}

fn skill_md(data_dir: &str) -> String {
    format!(
        "---\n\
         type: skill\n\
         id: customers\n\
         description: EU customers, mirrored read-only.\n\
         backend:\n\
        \x20 kind: sql_view\n\
        \x20 source:\n\
        \x20   connector: json_dir\n\
        \x20   relation: {data_dir}\n\
        \x20 project:\n\
        \x20   name: name\n\
        \x20 search_text: [name]\n\
         ---\n\
         # customers\n"
    )
}

async fn setup() -> Setup {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let data_dir = TempDir::new().unwrap();
    std::fs::write(
        data_dir.path().join("a.json"),
        br#"{"name":"Acme","tier":"gold"}"#,
    )
    .unwrap();
    std::fs::write(
        data_dir.path().join("b.json"),
        br#"{"name":"Globex","tier":"silver"}"#,
    )
    .unwrap();

    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Arc::new(Indexer::new(store, embedder, conn, TENANT).unwrap());

    // Seed the sql_view skill, then materialise one instance over json_dir.
    indexer
        .update_page(
            "markdown/skills/customers.md",
            &skill_md(data_dir.path().to_str().unwrap()),
        )
        .await
        .unwrap();
    let binding = SqlViewBinding {
        connector: SqlConnector::JsonDir,
        attach: None,
        relation: data_dir.path().to_str().unwrap().to_owned(),
        filter: None,
        project: [("name".to_owned(), "name".to_owned())]
            .into_iter()
            .collect(),
        search_text: vec!["name".to_owned()],
    };
    let m = SqlViewBackend::new(Arc::clone(&indexer))
        .create_instance(
            "customers",
            &binding,
            "eu",
            "# EU customers\nMirror of the CRM.",
        )
        .await
        .unwrap();

    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::Disabled,
        config_overrides: ConfigOverrides {
            indexer: Some(indexer),
            ..Default::default()
        },
        ..Default::default()
    })
    .await;

    Setup {
        process,
        page_id: m.page_id,
        _dirs: vec![store_dir, db_dir, data_dir],
    }
}

async fn call(p: &EscurelProcess, name: &str, args: Value) -> Value {
    reqwest::Client::new()
        .post(p.mcp_url())
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

#[tokio::test]
async fn expand_sql_instance_returns_overlay_then_bounded_projection() {
    let s = setup().await;
    let body = call(&s.process, "expand", json!({ "page_id": s.page_id })).await;
    let page = &body["result"]["structuredContent"];

    // Overlay (shown first): body + the backend_ref binding.
    assert!(page["body"].as_str().unwrap().contains("EU customers"));
    assert_eq!(page["frontmatter"]["backend_ref"]["kind"], "sql_view");

    // Bounded projection of the view's rows beneath the overlay (REQ-SQL-06).
    let proj = &page["backend_projection"];
    let rows = proj["rows"].as_array().expect("projection rows");
    assert_eq!(rows.len(), 2);
    let names: Vec<&str> = rows.iter().filter_map(|r| r["name"].as_str()).collect();
    assert!(
        names.contains(&"Acme") && names.contains(&"Globex"),
        "got {names:?}"
    );

    // Projected source column exposed under `source.<field>` (REQ-OV-02).
    assert!(proj["source"]["name"].is_string());

    s.process.shutdown().await;
}

#[tokio::test]
async fn update_page_creating_sql_instance_is_rejected_backend_read_only() {
    let s = setup().await;
    // Attempt to fabricate a NEW sql_view instance via update_page (instead
    // of the materialise path) → rejected with backend_read_only.
    let content = "---\ntype: instance\nskill: customers\nid: us\n---\n# US (forged)\n";
    let body = call(
        &s.process,
        "update_page",
        json!({ "page_id": "markdown/instances/customers/us.md", "content": content }),
    )
    .await;
    let r = &body["result"]["structuredContent"];
    assert_eq!(r["ok"], false, "must be rejected: {body}");
    assert_eq!(r["issues"][0]["code"], "backend_read_only");

    s.process.shutdown().await;
}

#[tokio::test]
async fn update_page_editing_existing_overlay_is_allowed() {
    let s = setup().await;
    // Editing the existing overlay (keeping its backend_ref) is permitted
    // co-authoring — the overlay body/notes are markdown.
    let content = "---\n\
         type: instance\n\
         skill: customers\n\
         id: eu\n\
         backend_ref:\n\
        \x20 kind: sql_view\n\
        \x20 view: vw_customers__eu\n\
         ---\n\
         # EU customers\n\nAdded an operator note.\n";
    let body = call(
        &s.process,
        "update_page",
        json!({ "page_id": s.page_id, "content": content }),
    )
    .await;
    let r = &body["result"]["structuredContent"];
    assert_eq!(r["ok"], true, "overlay edit should be allowed: {body}");

    s.process.shutdown().await;
}
