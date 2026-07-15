//! End-to-end acceptance for the crm-demo's *external instance backend*
//! content: the offline `erp_order` sql_view (over the shipped
//! `sources/erp/*.json`) and the `stock_quote` openapi skill (Yahoo-Finance-
//! shaped; tests run against a REAL local HTTP server, never the internet).
//!
//! Real gateway over `POST /mcp`, real DuckDB + `FsStore`, the REAL demo
//! seed (`examples/crm-demo` via `Indexer::seed_from_dir` — the same
//! function `ESCUREL_SEED_DIR` boot-seeding calls), and the same tool
//! sequence `scripts/demo-setup.sh` drives. No mocks at any boundary.

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

/// `examples/crm-demo`, canonicalised (the tests run from the crate dir).
fn demo_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/crm-demo")
        .canonicalize()
        .expect("examples/crm-demo exists")
}

/// Spawn a gateway seeded from the REAL demo corpus — the same
/// `seed_from_dir` path `ESCUREL_SEED_DIR` triggers at boot.
async fn spawn_demo_gateway() -> (EscurelProcess, Vec<TempDir>) {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Arc::new(Indexer::new(store, embedder, conn, TENANT).unwrap());
    indexer
        .seed_from_dir(&demo_dir())
        .await
        .expect("seed examples/crm-demo");

    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::Disabled, // the demo server runs without a verifier
        config_overrides: ConfigOverrides {
            indexer: Some(indexer),
            ..Default::default()
        },
        ..Default::default()
    })
    .await;
    (process, vec![store_dir, db_dir])
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

/// The demo-setup step for the sql_view item, exactly as
/// `scripts/demo-setup.sh` performs it: re-point the seeded `erp_order`
/// skill's relative `relation:` at the ABSOLUTE sources dir (the seed page
/// carries a repo-root-relative path; DuckDB resolves it against the server
/// process cwd, so the setup step resolves it), then materialise the `book`
/// instance from the skill's own binding via `create_sql_instance`.
async fn demo_setup_sql(p: &EscurelProcess) -> String {
    let skill_md = std::fs::read_to_string(demo_dir().join("skills/erp_order.md"))
        .expect("examples/crm-demo/skills/erp_order.md exists");
    let abs_relation = demo_dir().join("sources/erp");
    let rewritten = skill_md.replace(
        "relation: examples/crm-demo/sources/erp",
        &format!("relation: {}", abs_relation.display()),
    );
    assert_ne!(
        skill_md, rewritten,
        "the seeded erp_order skill must carry the repo-relative relation \
         the setup step rewrites"
    );
    let updated = call(
        p,
        "update_page",
        json!({ "page_id": "markdown/skills/erp_order.md", "content": rewritten }),
    )
    .await;
    assert_eq!(
        updated["result"]["structuredContent"]["ok"], true,
        "skill relation rewrite must validate: {updated}"
    );

    let created = call(
        p,
        "create_sql_instance",
        json!({
            "skill": "erp_order",
            "id": "book",
            "overlay_body": "# ERP order book\nRead-only mirror of the ERP order extract.",
        }),
    )
    .await;
    assert!(created.get("error").is_none(), "create error: {created}");
    created["result"]["structuredContent"]["page_id"]
        .as_str()
        .expect("page_id")
        .to_owned()
}

#[tokio::test]
async fn demo_erp_sql_view_expands_to_projection_with_source_namespace() {
    let (process, _dirs) = spawn_demo_gateway().await;
    let p = &process;
    let page_id = demo_setup_sql(p).await;

    let body = call(p, "expand", json!({ "page_id": page_id })).await;
    let page = &body["result"]["structuredContent"];
    assert_eq!(page["frontmatter"]["backend_ref"]["kind"], "sql_view");

    // Bounded projection of the shipped ERP rows (REQ-SQL-06).
    let proj = &page["backend_projection"];
    let rows = proj["rows"].as_array().expect("projection rows");
    assert!(
        (5..=8).contains(&rows.len()),
        "the demo ships 5-8 plausible ERP orders, got {}: {proj}",
        rows.len()
    );
    // Rows carry the documented ERP columns, keyed to seeded CRM customers.
    let first = rows[0].as_object().expect("row object");
    for col in ["order_id", "customer", "amount_eur", "status", "due_date"] {
        assert!(first.contains_key(col), "row missing `{col}`: {first:?}");
    }
    let customers: Vec<&str> = rows.iter().filter_map(|r| r["customer"].as_str()).collect();
    assert!(
        customers.contains(&"hoffmann-automotive"),
        "orders must key into the seeded CRM customers, got {customers:?}"
    );

    // Projected source columns under the `source.<field>` namespace
    // (REQ-OV-02 drift visibility).
    assert!(
        proj["source"]["customer"].is_string(),
        "source.customer namespace missing: {proj}"
    );

    process.shutdown().await;
}

#[tokio::test]
async fn demo_list_skills_reports_erp_order_sql_view_read_only() {
    let (process, _dirs) = spawn_demo_gateway().await;
    let result = call(&process, "list_skills", json!({})).await;
    let skills = result["result"]["structuredContent"]["skills"]
        .as_array()
        .unwrap();
    let erp = skills
        .iter()
        .find(|s| s["id"] == "erp_order")
        .unwrap_or_else(|| panic!("erp_order skill seeded: {skills:?}"));
    assert_eq!(erp["backend"]["kind"], "sql_view");
    assert_eq!(
        erp["capabilities"]["writable"], false,
        "sql_view is read-only: {erp}"
    );
    process.shutdown().await;
}
