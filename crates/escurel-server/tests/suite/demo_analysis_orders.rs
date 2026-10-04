//! The VS Code demo's supplier-risk analysis has a chart (Peacock report `supplier-risk-report`), and
//! a chart needs ROWS: the report's `data:` alias is the authored query `analysis_orders`, which reads a
//! `sql_view` over the demo's order lines. This is the test that the rows are really there.
//!
//! Real gateway over `POST /mcp`, real DuckDB + `FsStore`, the REAL demo seed
//! (`editors/vscode/demo/seed` via `Indexer::seed_from_dir`), the same two steps the demo's start script
//! performs (point the skill's relation at the absolute sources dir, then `create_sql_instance`), and the
//! tool Peacock calls (`query_instance`). No mocks at any boundary.

use std::path::PathBuf;
use std::sync::Arc;

use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, LaneStore};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, Opts};
use serde_json::{Value, json};
use tempfile::TempDir;

const TENANT: &str = "acme";
/// What the seed's skill page carries until the start script resolves it (DuckDB resolves a relative
/// glob against the server's cwd, so the demo points it at an absolute directory).
const PLACEHOLDER: &str = "@ORDER_LINES_DIR@";

fn demo_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../editors/vscode/demo")
        .canonicalize()
        .expect("editors/vscode/demo exists")
}

async fn call(p: &EscurelProcess, name: &str, args: Value) -> Value {
    let body: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": { "name": name, "arguments": args } }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json");
    assert!(body.get("error").is_none(), "{name}: {body}");
    body["result"]["structuredContent"].clone()
}

async fn demo_gateway() -> (EscurelProcess, Vec<TempDir>) {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Arc::new(Indexer::new(store, embedder, conn, TENANT).unwrap());
    indexer
        .seed_from_dir(&demo_dir().join("seed"))
        .await
        .expect("seed the demo");
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::Disabled,
        config_overrides: ConfigOverrides {
            indexer: Some(indexer),
            // The operator exposes the demo's extracts (the start script does the same).
            egress: Some(escurel_server::egress::EgressPolicy {
                sql_file_dirs: vec![demo_dir().join("sources")],
                ..Default::default()
            }),
            ..Default::default()
        },
        ..Default::default()
    })
    .await;
    (p, vec![store_dir, db_dir])
}

/// The demo start script's step: resolve the placeholder, then materialise the view.
async fn materialise(p: &EscurelProcess) {
    let page = call(
        p,
        "expand",
        json!({ "page_id": "markdown/skills/order-lines.md", "raw": true }),
    )
    .await;
    let skill = page["content"]
        .as_str()
        .expect("the demo seeds the order-lines skill");
    assert!(
        skill.contains(PLACEHOLDER),
        "the seed carries the placeholder"
    );
    let dir = demo_dir().join("sources/order-lines");
    let resolved = skill.replace(PLACEHOLDER, dir.to_str().unwrap());
    let r = call(
        p,
        "update_page",
        json!({ "page_id": "markdown/skills/order-lines.md", "content": resolved }),
    )
    .await;
    assert_eq!(r["ok"], true, "{r}");
    call(
        p,
        "create_sql_instance",
        json!({ "skill": "order-lines", "id": "all", "overlay_body": "# Order lines\nRead-only mirror of the demo's order lines." }),
    )
    .await;
}

#[tokio::test]
async fn the_analysis_chart_has_rows_one_per_affected_order_with_their_share() {
    let (p, _dirs) = demo_gateway().await;
    materialise(&p).await;

    // The analysis id starts with the supplier it is about (the echo harness writes
    // `<supplier>-<suffix>`), and that is how the query finds its orders.
    let r = call(
        &p,
        "query_instance",
        json!({ "ref": "analysis_orders", "params": { "analysis": "meier-guss-2026-10-03" } }),
    )
    .await;
    let rows = r["rows"].as_array().expect("rows");
    let orders: Vec<&str> = rows.iter().map(|r| r["order"].as_str().unwrap()).collect();
    assert_eq!(
        orders,
        ["order-4500131", "order-4500123"],
        "largest first: {r}"
    );
    assert_eq!(rows[0]["customer"], "Kessler Werkzeugbau GmbH");
    assert_eq!(rows[0]["net_value"].as_f64(), Some(66200.0));
    assert_eq!(rows[1]["qty"].as_i64(), Some(240));
    // The shares are of the analysis's own total (128,600), so they match the table in its body.
    let share: f64 = rows.iter().map(|r| r["share_pct"].as_f64().unwrap()).sum();
    assert!(
        (share - 100.0).abs() < 0.2,
        "shares add up to 100%: {rows:?}"
    );
    assert_eq!(rows[0]["share_pct"].as_f64(), Some(51.5));
}

#[tokio::test]
async fn an_analysis_of_another_supplier_gets_only_its_own_orders() {
    let (p, _dirs) = demo_gateway().await;
    materialise(&p).await;
    let other = call(
        &p,
        "query_instance",
        json!({ "ref": "analysis_orders", "params": { "analysis": "stahl-ag-2026-10-03" } }),
    )
    .await;
    let orders: Vec<&str> = other["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["order"].as_str().unwrap())
        .collect();
    assert_eq!(orders, ["order-4500131"], "{other}");
    let none = call(
        &p,
        "query_instance",
        json!({ "ref": "analysis_orders", "params": { "analysis": "nobody-1" } }),
    )
    .await;
    assert_eq!(none["rows"].as_array().unwrap().len(), 0, "{none}");
}
