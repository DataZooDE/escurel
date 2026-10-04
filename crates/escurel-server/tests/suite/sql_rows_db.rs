//! Per-row instances (and their write-back) over a REAL SQL database file — no mocks: a real SQLite
//! file read through DuckDB's `sqlite` extension, a real gateway over `POST /mcp`, real DuckDB +
//! `FsStore`. (The Postgres twin of these tests is `sql_rows_postgres.rs`, behind `live-postgres`
//! because it needs Docker.)
//!
//! What this pins:
//! - the source credential is a SECRET REFERENCE (`file:` under an operator-allowed directory), never
//!   a connection string in a page, and an operator policy decides which database files / hosts a
//!   tenant admin may attach;
//! - 2,500 rows page exactly once by keyset, typed from `DESCRIBE`, NULL keys excluded;
//! - a human-gated write-back (`create_draft` with `write_back` -> human promote) updates ONE row
//!   with bound parameters and an optimistic check, and nothing else.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use duckdb::Connection;
use escurel_auth::Role;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_server::egress::EgressPolicy;
use escurel_storage::{FsStore, LaneStore};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, Opts};
use serde_json::{Value, json};
use tempfile::TempDir;

const TENANT: &str = "acme";
const SKILL_PAGE: &str = "markdown/skills/shop-order.md";
const ROWS: usize = 2_500;

fn doc(n: usize) -> String {
    format!("{:010}", 4_500_000 + n)
}
fn row_page(n: usize) -> String {
    format!("markdown/instances/shop-order/{}.md", doc(n))
}

/// A real SQLite database file with `ROWS` orders (+ one row whose key is NULL), made through DuckDB's
/// `sqlite` extension (the same engine the gateway reads it with).
pub(crate) fn seed_sqlite(path: &Path) {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch("INSTALL sqlite; LOAD sqlite;").unwrap();
    c.execute_batch(&format!(
        "ATTACH '{}' AS s (TYPE sqlite);
         CREATE TABLE s.orders (vbeln VARCHAR, kunnr VARCHAR, netwr DOUBLE, qty INTEGER, status VARCHAR);
         INSERT INTO s.orders
           SELECT printf('%010d', 4500000 + i), printf('%07d', 1000000 + i % 40), 100.0 + i, 10 + i % 7, 'open'
           FROM range({ROWS}) t(i);
         INSERT INTO s.orders VALUES (NULL, '0000000', 1.0, 1, 'orphan');",
        path.display()
    ))
    .unwrap();
}

/// Read one order straight from the database file (outside the gateway), as the source of truth.
pub(crate) fn db_row(path: &Path, n: usize) -> Value {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch("INSTALL sqlite; LOAD sqlite;").unwrap();
    c.execute_batch(&format!(
        "ATTACH '{}' AS s (TYPE sqlite, READ_ONLY);",
        path.display()
    ))
    .unwrap();
    c.query_row(
        "SELECT netwr, qty, status FROM s.orders WHERE vbeln = ?",
        [doc(n)],
        |r| {
            Ok(
                json!({ "netwr": r.get::<_, f64>(0)?, "qty": r.get::<_, i32>(1)?,
                       "status": r.get::<_, String>(2)? }),
            )
        },
    )
    .unwrap()
}

pub(crate) fn skill_page() -> String {
    r#"---
kind: skill
id: shop-order
description: An order of the shop database, one instance per row.
fields:
  - {name: sales_doc, kind: string, required: true, label: "Sales document"}
  - {name: sold_to, kind: string, label: "Sold-to"}
  - {name: net_value, kind: float, label: "Net value"}
  - {name: quantity, kind: int, label: "Quantity"}
  - {name: status, kind: string, label: "Status"}
backend:
  kind: sql_view
  instances: rows
  key: vbeln
  linked: markdown
  filterable: [kunnr]
  writable_columns: [status, qty]
  source: {connector: sqlite, attach: shop_db, relation: "main.orders"}
  project: {vbeln: sales_doc, kunnr: sold_to, netwr: net_value, qty: quantity, status: status}
---
# shop-order
"#
    .to_owned()
}

pub(crate) struct Gw {
    pub p: EscurelProcess,
    pub db: PathBuf,
    pub secrets: PathBuf,
    pub _dirs: Vec<TempDir>,
}

/// The policy a test operator sets: loopback allowed (the container/SQLite sources are local), the
/// secret directory and the database-file directory named.
pub(crate) fn policy(secret_dir: &Path, sql_dir: &Path) -> EgressPolicy {
    let mut p = EgressPolicy {
        allow_loopback: true,
        ..EgressPolicy::default()
    };
    p.secrets.file_dirs = vec![secret_dir.to_path_buf()];
    p.sql_file_dirs = vec![sql_dir.to_path_buf()];
    p
}

/// The whole JSON-RPC body, errors included.
pub(crate) async fn raw_call(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
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

pub(crate) async fn call_as(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    let body: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
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

impl Gw {
    /// A gateway whose tenant has the `shop-order` skill over a real SQLite file, with the credential
    /// registered as a `file:` secret reference.
    pub(crate) async fn start() -> Self {
        let store_dir = TempDir::new().unwrap();
        let db_dir = TempDir::new().unwrap();
        let sql_dir = TempDir::new().unwrap();
        let secret_dir = TempDir::new().unwrap();
        let db = sql_dir.path().join("shop.db");
        seed_sqlite(&db);
        let secrets = secret_dir.path().join("shop-dsn");
        std::fs::write(&secrets, format!("{}\n", db.display())).unwrap();

        let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
        let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
        let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
        Migrator::up(&conn).unwrap();
        let indexer = Arc::new(Indexer::new(store, embedder, conn, TENANT).unwrap());
        let p = EscurelProcess::spawn(Opts {
            auth: AuthMode::TestIssuer,
            config_overrides: ConfigOverrides {
                indexer: Some(indexer),
                egress: Some(policy(secret_dir.path(), sql_dir.path())),
                ..Default::default()
            },
            ..Default::default()
        })
        .await;
        let gw = Self {
            p,
            db,
            secrets,
            _dirs: vec![store_dir, db_dir, sql_dir, secret_dir],
        };
        let reg = gw
            .admin(
                "register_credential",
                json!({ "name": "shop_db", "connector": "sqlite",
                        "secret_ref": format!("file:{}", gw.secrets.display()) }),
            )
            .await;
        assert_eq!(reg["ok"], true, "the credential registers: {reg}");
        let r = gw
            .admin(
                "update_page",
                json!({ "page_id": SKILL_PAGE, "content": skill_page() }),
            )
            .await;
        assert_eq!(r["ok"], true, "the skill page is accepted: {r}");
        gw
    }

    pub(crate) async fn admin(&self, name: &str, args: Value) -> Value {
        let token = self.p.mint_token(TENANT, Role::Admin);
        call_as(&self.p, &token, name, args).await
    }
}

#[tokio::test]
async fn rows_over_a_real_sqlite_file_page_exactly_once_and_skip_null_keys() {
    let g = Gw::start().await;
    let mut seen: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    let mut pages = 0;
    loop {
        let r = g
            .admin(
                "list_instances",
                json!({ "skill": "shop-order", "limit": 500, "cursor": cursor }),
            )
            .await;
        let rows = r["instances"].as_array().expect("instances");
        for i in rows {
            assert!(
                i["frontmatter"]["net_value"].is_number(),
                "typed double: {i}"
            );
            assert!(
                i["frontmatter"]["quantity"].is_number(),
                "typed integer: {i}"
            );
            seen.push(i["page_id"].as_str().unwrap().to_owned());
        }
        pages += 1;
        match r["next_cursor"].as_str() {
            Some(c) => cursor = Some(c.to_owned()),
            None => break,
        }
        assert!(pages < 20, "the cursor must terminate");
    }
    assert_eq!(
        seen.len(),
        ROWS,
        "every real row once, the NULL-key row never"
    );
    let mut sorted = seen.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted, seen, "ascending, no duplicates");
    assert_eq!(seen[0], row_page(0));
    assert_eq!(seen[ROWS - 1], row_page(ROWS - 1));

    // A filterable column narrows with a bound parameter.
    let r = g
        .admin(
            "list_instances",
            json!({ "skill": "shop-order", "limit": 10_000,
                    "frontmatter_key": "sold_to", "frontmatter_value": "1000007" }),
        )
        .await;
    assert_eq!(
        r["instances"].as_array().unwrap().len(),
        (0..ROWS).filter(|i| i % 40 == 7).count(),
        "{r}"
    );
}

#[tokio::test]
async fn a_row_reads_as_typed_fields_with_the_discovered_schema_and_a_source_marker() {
    let g = Gw::start().await;
    let r = g.admin("expand", json!({ "page_id": row_page(7) })).await;
    assert_eq!(r["frontmatter"]["sales_doc"], doc(7), "{r}");
    // What the gateway shows is what the database holds (read straight from the file).
    let truth = db_row(&g.db, 7);
    assert_eq!(r["frontmatter"]["quantity"], truth["qty"], "{r}");
    assert_eq!(r["frontmatter"]["net_value"], truth["netwr"], "{r}");
    let proj = &r["backend_projection"];
    assert_eq!(proj["trust"], "source", "{proj}");
    assert_eq!(proj["read_only"], true, "{proj}");
    let kinds: Vec<&str> = proj["columns"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["kind"].as_str())
        .collect();
    assert!(kinds.contains(&"float") && kinds.contains(&"int"), "{proj}");
}

#[tokio::test]
async fn the_credential_is_a_reference_not_a_connection_string_and_the_operator_decides_what_it_may_name()
 {
    let g = Gw::start().await;

    // The registry holds the REFERENCE; the database path is never echoed.
    let list = g.admin("list_credentials", json!({})).await;
    assert!(
        !list.to_string().contains("shop.db"),
        "a credential listing must not carry the database path: {list}"
    );

    // A reference outside the operator's secret directory is refused at registration.
    let bad = reqwest::Client::new()
        .post(g.p.mcp_url())
        .header(
            "authorization",
            format!("Bearer {}", g.p.mint_token(TENANT, Role::Admin)),
        )
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "register_credential",
                        "arguments": { "name": "evil", "connector": "sqlite",
                                       "secret_ref": "file:/etc/hostname" } } }))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert!(
        bad.get("error").is_some() || bad["result"]["isError"] == true,
        "a `file:` reference outside the allowed directory must be refused: {bad}"
    );

    // A SQLite file outside the operator's database directory cannot be attached, even through a
    // reference that itself is allowed.
    let outside = TempDir::new().unwrap();
    let other = outside.path().join("other.db");
    seed_sqlite(&other);
    std::fs::write(&g.secrets, format!("{}\n", other.display())).unwrap();
    let refused = raw_call(
        &g.p,
        &g.p.mint_token(TENANT, Role::Admin),
        "list_instances",
        json!({ "skill": "shop-order", "limit": 5 }),
    )
    .await;
    assert!(
        refused.to_string().contains("egress policy")
            && refused.to_string().contains("ESCUREL_SQL_FILE_DIRS"),
        "a database file outside the allowed directories is refused, naming the policy: {refused}"
    );
}
