//! The VS Code demo's orders and suppliers are `instances: rows` sql_views over SAP-shaped extracts
//! (VBAK, LFA1), each with a linked markdown companion for the notes, items and history. This is the
//! test that the REAL demo seed does what the walkthrough claims.
//!
//! Real gateway over `POST /mcp`, real DuckDB + `FsStore`, the real seed
//! (`editors/vscode/demo/seed` via `Indexer::seed_from_dir`) and the real extracts, with the same
//! placeholder resolution `demo/run.sh` performs.

use std::path::PathBuf;
use std::sync::Arc;

use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, LaneStore};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, Opts};
use serde_json::{Value, json};
use tempfile::TempDir;

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

/// What `demo/run.sh` does: point each skill's `@..._DIR@` placeholder at the real extract.
async fn demo_gateway() -> (EscurelProcess, Vec<TempDir>) {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Arc::new(Indexer::new(store, embedder, conn, "acme").unwrap());
    indexer
        .seed_from_dir(&demo_dir().join("seed"))
        .await
        .expect("seed the demo");
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::Disabled,
        config_overrides: ConfigOverrides {
            indexer: Some(indexer),
            ..Default::default()
        },
        ..Default::default()
    })
    .await;
    for (skill, placeholder, dir) in [
        ("customer-order", "@VBAK_DIR@", "sources/vbak"),
        ("supplier", "@LFA1_DIR@", "sources/lfa1"),
    ] {
        let page_id = format!("markdown/skills/{skill}.md");
        let page = call(&p, "expand", json!({ "page_id": page_id, "raw": true })).await;
        let text = page["content"].as_str().expect("seeded skill").to_owned();
        assert!(text.contains(placeholder), "{skill} carries {placeholder}");
        let resolved = text.replace(placeholder, demo_dir().join(dir).to_str().unwrap());
        let r = call(
            &p,
            "update_page",
            json!({ "page_id": page_id, "content": resolved }),
        )
        .await;
        assert_eq!(r["ok"], true, "{r}");
    }
    (p, vec![store_dir, db_dir])
}

#[tokio::test]
async fn the_demo_orders_are_the_rows_of_the_vbak_extract() {
    let (p, _d) = demo_gateway().await;
    let r = call(
        &p,
        "list_instances",
        json!({ "skill": "customer-order", "limit": 100 }),
    )
    .await;
    let ids: Vec<&str> = r["instances"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["page_id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        [
            "markdown/instances/customer-order/order-4500123.md",
            "markdown/instances/customer-order/order-4500124.md",
            "markdown/instances/customer-order/order-4500131.md",
            "markdown/instances/customer-order/order-4500140.md",
            "markdown/instances/customer-order/order-4500152.md",
        ],
        "{r}"
    );
    let first = &r["instances"][2]["frontmatter"];
    assert_eq!(
        first["sales_doc"], 4_500_131,
        "SAP columns under their field names: {first}"
    );
    assert_eq!(first["sold_to_name"], "Kessler Werkzeugbau GmbH");
    assert_eq!(first["confirmed_delivery"], "2026-10-19");
}

#[tokio::test]
async fn an_order_reads_as_one_instance_the_row_plus_its_linked_notes() {
    let (p, _d) = demo_gateway().await;
    let r = call(
        &p,
        "expand",
        json!({ "page_id": "markdown/instances/customer-order/order-4500131.md" }),
    )
    .await;
    // The row (read-only SAP columns) ...
    assert_eq!(r["frontmatter"]["order_type"], "ZOR", "{r}");
    assert_eq!(r["backend_projection"]["read_only"], true);
    // ... and the companion's own field and body (the items table, the history).
    assert_eq!(r["frontmatter"]["delivery_risk"], "low", "{r}");
    let body = r["body"].as_str().unwrap();
    assert!(
        body.contains("| Item | Material |"),
        "the items table: {body}"
    );
    assert!(body.contains("[[supplier::meier-guss"), "{body}");
    assert_eq!(r["backend_projection"]["linked"]["exists"], true);
    assert!(r["content_sha256"].is_string(), "the companion is CAS-able");
}

#[tokio::test]
async fn a_supplier_is_found_by_vendor_number_and_a_wikilink_to_it_resolves() {
    let (p, _d) = demo_gateway().await;
    // The echo harness's own lookup of the supplier a signal is about.
    let r = call(
        &p,
        "list_instances",
        json!({ "skill_id": "supplier", "frontmatter_key": "vendor", "frontmatter_value": "100234" }),
    )
    .await;
    let sup = &r["instances"][0];
    assert_eq!(
        sup["page_id"], "markdown/instances/supplier/meier-guss.md",
        "{r}"
    );
    assert_eq!(sup["frontmatter"]["name"], "Meier-Guss GmbH");
    assert_eq!(sup["frontmatter"]["vendor"], 100_234);
    let link = call(
        &p,
        "resolve",
        json!({ "wikilink": "[[supplier::meier-guss|Meier-Guss GmbH]]" }),
    )
    .await;
    assert_eq!(link["exists"], true, "{link}");
}
