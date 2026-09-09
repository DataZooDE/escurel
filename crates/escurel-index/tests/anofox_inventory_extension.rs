//! P1 no-mock end-to-end: a baked anofox DuckDB extension reached through a
//! curator-authored query page.
//!
//! Real DuckDB (v1.5.5), a REAL `anofox_inventory.duckdb_extension` loaded via
//! the production `ESCUREL_INDEX_EXTENSIONS` hook, a real `vw_` sql_view over
//! on-disk SKU rows, a real `[[query::*]]` page whose SQL calls the real
//! `inv_optimize_qr(...)` reorder-policy function, and the real per-instance
//! ACL — no mocks anywhere. This is the fleet thesis exercised: compute lives
//! in a DuckDB extension, invoked only through an ACL-checked query page.
//!
//! Standalone test binary (own process) so setting the two env vars the boot
//! path reads cannot race the parallel `suite` tests.

use std::path::PathBuf;
use std::sync::Arc;

use bytes::Bytes;
use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::backend::{SqlConnector, SqlViewBackend, SqlViewBinding};
use escurel_index::{AclCaller, Indexer, Migrator, QueryError};
use escurel_storage::{FsStore, Key, LaneStore};
use tempfile::TempDir;

const TENANT: &str = "acme";

const SKILL_QUERY: (&str, &str) = (
    "markdown/skills/query.md",
    "---\ntype: skill\nid: query\ndescription: Reusable parameterised reads.\n---\n# query\n",
);

/// Public sql_view skill (default tenant read policy ⇒ `public` may read).
const SKILL_INVENTORY: (&str, &str) = (
    "markdown/skills/inventory.md",
    "---\ntype: skill\nid: inventory\ndescription: SKU planning inputs, read-only.\n\
     backend:\n  kind: sql_view\n  source: { connector: json_dir, relation: /unused }\n\
     search_text: [sku]\n---\n# inventory\n",
);

/// Owner-private sql_view skill — a non-owner, non-admin caller is denied.
const SKILL_INVENTORY_SECRET: (&str, &str) = (
    "markdown/skills/secret_inventory.md",
    "---\ntype: skill\nid: secret_inventory\ndescription: Owner-private SKU inputs.\n\
     visibility: owner\nowner_field: credential\n\
     backend:\n  kind: sql_view\n  source: { connector: json_dir, relation: /unused }\n\
     search_text: [sku]\n---\n# secret_inventory\n",
);

/// The curator's reorder-policy page: calls the REAL `inv_optimize_qr` over the
/// managed `{{target}}` view. The model would only ever *select* this page.
const QUERY_REORDER: (&str, &str) = (
    "markdown/instances/query/reorder-proposal.md",
    "---\ntype: instance\nskill: query\nid: reorder-proposal\n\
     target: \"[[inventory::wh]]\"\n\
     params: []\n\
     sql: \"SELECT sku, t.policy.reorder_point AS reorder_point, t.policy.order_quantity AS order_quantity \
     FROM (SELECT sku, inv_optimize_qr(annual_demand, demand_std, lead_time, order_cost, holding_rate, unit_cost, shortage_cost) AS policy FROM {{target}}) AS t \
     ORDER BY sku\"\n\
     ---\n# reorder-proposal\n",
);

/// The same page pointed at the owner-private instance — the negative ACL path.
const QUERY_REORDER_SECRET: (&str, &str) = (
    "markdown/instances/query/reorder-secret.md",
    "---\ntype: instance\nskill: query\nid: reorder-secret\n\
     target: \"[[secret_inventory::wh]]\"\n\
     params: []\n\
     sql: \"SELECT sku, inv_optimize_qr(annual_demand, demand_std, lead_time, order_cost, holding_rate, unit_cost, shortage_cost).reorder_point AS reorder_point FROM {{target}}\"\n\
     ---\n# reorder-secret\n",
);

struct Harness {
    store: Arc<dyn LaneStore>,
    indexer: Arc<Indexer>,
    _store_dir: TempDir,
    _db_dir: TempDir,
    data_dir: TempDir,
}

/// Absolute path to the prebuilt v1.5.5 extension (sibling repo build output).
fn inventory_extension_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../anofox-inventory/build/release/extension/anofox_inventory/anofox_inventory.duckdb_extension")
}

fn fresh_harness() -> Harness {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let data_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());

    // Unsigned load must be permitted at OPEN time (a locally-built extension
    // is unsigned) — this is exactly what `Migrator::connection_config()`
    // produces when `ESCUREL_ALLOW_UNSIGNED_EXTENSIONS` is set; built directly
    // here so the crate's `forbid(unsafe_code)` (no `set_var`) is respected.
    let ext = inventory_extension_path();
    let config = duckdb::Config::default()
        .allow_unsigned_extensions()
        .unwrap();
    let conn = Connection::open_with_flags(db_dir.path().join("escurel.duckdb"), config).unwrap();
    Migrator::up(&conn).unwrap();
    // The production LOAD path (env-free core of `load_baked_extensions`).
    Migrator::load_extension_paths(&conn, &[ext.to_str().unwrap().to_owned()])
        .expect("LOAD baked anofox_inventory extension");

    let indexer = Arc::new(Indexer::new(Arc::clone(&store), embedder, conn, TENANT).unwrap());
    Harness {
        store,
        indexer,
        _store_dir: store_dir,
        _db_dir: db_dir,
        data_dir,
    }
}

async fn seed(h: &Harness, pages: &[(&str, &'static str)]) {
    for (path, body) in pages {
        let key = Key::new(TENANT, (*path).to_owned()).unwrap();
        h.store
            .write(&key, Bytes::from_static(body.as_bytes()))
            .await
            .unwrap();
        h.indexer.update_page(path, body).await.unwrap();
    }
}

/// Materialise a `sql_view` instance `skill/wh` over two SKU rows with the
/// columns `inv_optimize_qr` needs (decimals ⇒ DOUBLE inference).
async fn materialise_inventory(h: &Harness, skill: &str) {
    let dir = h.data_dir.path();
    std::fs::write(
        dir.join("sku1.json"),
        br#"{"sku":"SKU-1182","annual_demand":1200.0,"demand_std":15.0,"lead_time":5.0,"order_cost":50.0,"holding_rate":0.2,"unit_cost":10.0,"shortage_cost":25.0}"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("sku2.json"),
        br#"{"sku":"SKU-0447","annual_demand":300.0,"demand_std":8.0,"lead_time":7.0,"order_cost":40.0,"holding_rate":0.25,"unit_cost":22.0,"shortage_cost":30.0}"#,
    )
    .unwrap();
    let binding = SqlViewBinding {
        connector: SqlConnector::JsonDir,
        attach: None,
        relation: dir.to_str().unwrap().to_owned(),
        filter: None,
        project: Default::default(),
        search_text: vec!["sku".to_owned()],
    };
    SqlViewBackend::new(Arc::clone(&h.indexer))
        .create_instance(skill, &binding, "wh", "# overlay")
        .await
        .expect("materialise sql_view instance");
}

fn analyst(subject: &str) -> AclCaller<'_> {
    AclCaller {
        subject,
        is_admin: false,
        token_groups: &[],
    }
}

#[tokio::test]
async fn reorder_proposal_e2e_through_query_page_and_acl() {
    let ext = inventory_extension_path();
    if !ext.exists() {
        eprintln!(
            "SKIP: {} not built — run `make release` in anofox-inventory to exercise this E2E.",
            ext.display()
        );
        return;
    }

    let h = fresh_harness();
    seed(
        &h,
        &[
            SKILL_QUERY,
            SKILL_INVENTORY,
            SKILL_INVENTORY_SECRET,
            QUERY_REORDER,
            QUERY_REORDER_SECRET,
        ],
    )
    .await;
    materialise_inventory(&h, "inventory").await;
    materialise_inventory(&h, "secret_inventory").await;

    let no_args = serde_json::Map::new();

    // POSITIVE: a public caller runs the reorder page → the real extension
    // computes a real reorder point per SKU.
    let res = h
        .indexer
        .query_instance("reorder-proposal", &no_args, &analyst("planner@acme"))
        .await
        .expect("public reorder proposal");
    assert_eq!(res.rows.len(), 2, "one policy row per SKU");
    for row in &res.rows {
        let rop = row
            .get("reorder_point")
            .and_then(|v| v.as_f64())
            .expect("reorder_point is a number");
        let oq = row
            .get("order_quantity")
            .and_then(|v| v.as_f64())
            .expect("order_quantity is a number");
        assert!(rop > 0.0, "reorder point must be positive, got {rop}");
        assert!(oq > 0.0, "order quantity must be positive, got {oq}");
    }

    // NEGATIVE: the same computation over an owner-private instance is refused
    // for a non-owner — the fleet's fail-closed boundary, no number leaks.
    let denied = h
        .indexer
        .query_instance("reorder-secret", &no_args, &analyst("intruder@acme"))
        .await;
    assert!(
        matches!(denied, Err(QueryError::Forbidden { .. })),
        "a non-owner must be refused the owner-private reorder page, got {denied:?}"
    );
}
