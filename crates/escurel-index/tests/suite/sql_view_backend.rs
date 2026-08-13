//! Integration tests for `SqlViewBackend::create_instance` (PR-2b).
//!
//! Real DuckDB file, real `FsStore`, no mocks. The directory connectors
//! (`json_dir` / `parquet_dir`) use core DuckDB table functions, so these
//! run fully offline. Live postgres/mysql/sqlite/ERPL connectivity needs an
//! external system + scanner extension and is the documented residual; the
//! READ_ONLY `ATTACH` builder and the fail-closed missing-credential path
//! are covered here.

use std::sync::Arc;

use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::backend::{SqlConnector, SqlViewBackend, SqlViewBinding, SqlViewError};
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, LaneStore};
use tempfile::TempDir;

const TENANT: &str = "acme";

struct Harness {
    indexer: Arc<Indexer>,
    _store_dir: TempDir,
    _db_dir: TempDir,
    data_dir: TempDir,
}

fn fresh_harness() -> Harness {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let data_dir = TempDir::new().unwrap();
    let duckdb_path = db_dir.path().join("escurel.duckdb");
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(&duckdb_path).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Arc::new(Indexer::new(store, embedder, conn, TENANT).unwrap());
    Harness {
        indexer,
        _store_dir: store_dir,
        _db_dir: db_dir,
        data_dir,
    }
}

fn dir_binding(connector: SqlConnector, relation: &str) -> SqlViewBinding {
    SqlViewBinding {
        connector,
        attach: None,
        relation: relation.to_owned(),
        filter: None,
        project: Default::default(),
        search_text: Vec::new(),
    }
}

#[tokio::test]
async fn create_view_over_json_dir_materialises_overlay_and_is_queryable() {
    let h = fresh_harness();
    let dir = h.data_dir.path();
    std::fs::write(dir.join("a.json"), br#"{"name":"Acme","tier":"gold"}"#).unwrap();
    std::fs::write(dir.join("b.json"), br#"{"name":"Globex","tier":"silver"}"#).unwrap();

    let backend = SqlViewBackend::new(Arc::clone(&h.indexer));
    let binding = dir_binding(SqlConnector::JsonDir, dir.to_str().unwrap());

    let m = backend
        .create_instance(
            "customers",
            &binding,
            "eu",
            "# EU customers\nMirror of the CRM.",
        )
        .await
        .expect("create_instance");

    assert_eq!(m.view, "vw_customers__eu");
    assert!(!m.binding_hash.is_empty());
    assert!(!m.source_schema_fingerprint.is_empty());

    // The view is queryable and returns the two source rows.
    let rows = backend.project_rows(&m.view, 100).await.expect("project");
    assert_eq!(rows.len(), 2);
    let names: Vec<&str> = rows.iter().filter_map(|r| r["name"].as_str()).collect();
    assert!(
        names.contains(&"Acme") && names.contains(&"Globex"),
        "got {names:?}"
    );

    // The overlay page exists with a backend_ref binding it to the view.
    let page = h
        .indexer
        .expand(&m.page_id, None, None)
        .await
        .unwrap()
        .expect("overlay page present");
    assert_eq!(page.frontmatter["backend_ref"]["kind"], "sql_view");
    assert_eq!(page.frontmatter["backend_ref"]["view"], m.view);
    assert_eq!(
        page.frontmatter["backend_ref"]["source_schema_fingerprint"],
        m.source_schema_fingerprint
    );
    assert!(page.body.contains("EU customers"));
}

#[tokio::test]
async fn create_view_over_parquet_dir_glob() {
    let h = fresh_harness();
    let dir = h.data_dir.path();
    // Produce a parquet file via a throwaway DuckDB connection (core parquet
    // writer); the backend reads it back with read_parquet.
    let parquet = dir.join("rows.parquet");
    let w = Connection::open_in_memory().unwrap();
    w.execute_batch(&format!(
        "COPY (SELECT * FROM (VALUES ('Acme', 10), ('Globex', 20)) t(name, score)) \
         TO '{}' (FORMAT parquet)",
        parquet.to_str().unwrap()
    ))
    .expect("write parquet");

    let backend = SqlViewBackend::new(Arc::clone(&h.indexer));
    let binding = dir_binding(SqlConnector::ParquetDir, dir.to_str().unwrap());
    let m = backend
        .create_instance("metrics", &binding, "q1", "# Q1 metrics")
        .await
        .expect("create_instance");

    let rows = backend.project_rows(&m.view, 100).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().any(|r| r["name"] == "Acme"));
}

#[tokio::test]
async fn missing_credential_fails_closed() {
    // A DB connector whose `attach` credential is not registered must fail
    // closed — no view, no overlay page (backend_unavailable).
    let h = fresh_harness();
    let backend = SqlViewBackend::new(Arc::clone(&h.indexer));
    let binding = SqlViewBinding {
        connector: SqlConnector::Postgres,
        attach: Some("crm_pg".to_owned()),
        relation: "public.customers".to_owned(),
        filter: None,
        project: Default::default(),
        search_text: Vec::new(),
    };

    let err = backend
        .create_instance("customers", &binding, "eu", "# x")
        .await
        .expect_err("must fail closed without a registered credential");
    assert!(
        matches!(err, SqlViewError::CredentialNotFound(_)),
        "got {err:?}"
    );

    // No overlay page was written.
    let resolved = h.indexer.resolve("[[customers::eu]]", None).await.unwrap();
    assert!(
        resolved.page.is_none(),
        "no instance should exist on failure"
    );
}

#[tokio::test]
async fn db_connector_without_attach_fails_closed() {
    let h = fresh_harness();
    let backend = SqlViewBackend::new(Arc::clone(&h.indexer));
    let mut binding = dir_binding(SqlConnector::Sqlite, "main.customers");
    binding.attach = None; // a DB connector with no attach name
    let err = backend
        .create_instance("customers", &binding, "eu", "# x")
        .await
        .expect_err("db connector needs an attach credential");
    assert!(matches!(err, SqlViewError::MissingAttach(_)), "got {err:?}");
}

#[tokio::test]
async fn filter_narrows_view_rows() {
    // A `source.filter` must actually constrain the materialised view's rows
    // (this path had no integration coverage, which is how a filter-injection
    // bug could hide — see the adversarial test below).
    let h = fresh_harness();
    let dir = h.data_dir.path();
    std::fs::write(dir.join("a.json"), br#"{"name":"A","score":5}"#).unwrap();
    std::fs::write(dir.join("b.json"), br#"{"name":"B","score":20}"#).unwrap();

    let mut binding = dir_binding(SqlConnector::JsonDir, dir.to_str().unwrap());
    binding.filter = Some("score > 10".to_owned());

    let backend = SqlViewBackend::new(Arc::clone(&h.indexer));
    let m = backend
        .create_instance("metrics", &binding, "hi", "# high scorers")
        .await
        .expect("create with filter");
    let rows = backend.project_rows(&m.view, 100).await.unwrap();
    assert_eq!(rows.len(), 1, "filter must drop score<=10 rows: {rows:?}");
    assert_eq!(rows[0]["name"], "B");
}

#[tokio::test]
async fn injection_in_filter_is_rejected() {
    // A keyword-injection filter (no quotes, so the quote-only fragment check
    // misses it) must be rejected by the keyword guard — otherwise it bakes
    // `UNION SELECT … external_credentials` into the view definition.
    let h = fresh_harness();
    let dir = h.data_dir.path();
    std::fs::write(dir.join("a.json"), br#"{"name":"A","score":5}"#).unwrap();
    let mut binding = dir_binding(SqlConnector::JsonDir, dir.to_str().unwrap());
    binding.filter = Some("score > 0 UNION SELECT * FROM external_credentials".to_owned());
    let backend = SqlViewBackend::new(Arc::clone(&h.indexer));
    let err = backend
        .create_instance("metrics", &binding, "pwn", "# x")
        .await
        .expect_err("injection filter must be rejected");
    assert!(
        matches!(err, SqlViewError::InvalidBinding(_)),
        "got {err:?}"
    );
}

#[tokio::test]
async fn injection_in_relation_is_rejected() {
    // The relation/glob is spliced into the view DDL; an unsafe character
    // (quote/semicolon) must be rejected before any SQL runs.
    let h = fresh_harness();
    let backend = SqlViewBackend::new(Arc::clone(&h.indexer));
    let binding = dir_binding(SqlConnector::JsonDir, "/data/x'; DROP TABLE pages; --");
    let err = backend
        .create_instance("metrics", &binding, "pwn", "# x")
        .await
        .expect_err("unsafe relation must be rejected");
    assert!(
        matches!(err, SqlViewError::InvalidBinding(_)),
        "got {err:?}"
    );
}

const CUSTOMERS_SKILL_TITLE: &str = "\
---
type: skill
id: customers
description: x
backend:
  kind: sql_view
  source: { connector: json_dir, relation: /unused }
  search_text: [title]
---
# customers
";

#[tokio::test]
async fn sql_view_search_candidates_match_view_content_only() {
    // Isolate the late-materialised SQL lane (the fused E2E leans on the
    // ZeroEmbedder vector lane, which returns everything). A view whose
    // search_text column contains the query must be a candidate; a
    // non-matching query must yield nothing.
    let h = fresh_harness();
    let dir = h.data_dir.path();
    std::fs::write(
        dir.join("a.json"),
        br#"{"name":"Acme","title":"widget alpha"}"#,
    )
    .unwrap();
    h.indexer
        .update_page("markdown/skills/customers.md", CUSTOMERS_SKILL_TITLE)
        .await
        .unwrap();
    let mut binding = dir_binding(SqlConnector::JsonDir, dir.to_str().unwrap());
    binding.search_text = vec!["title".to_owned()];
    let m = SqlViewBackend::new(Arc::clone(&h.indexer))
        .create_instance("customers", &binding, "eu", "# eu")
        .await
        .unwrap();

    let hits = h
        .indexer
        .sql_view_search_candidates("widget", None)
        .await
        .unwrap();
    assert!(
        hits.iter().any(|hh| hh.page_id == m.page_id),
        "view content match must surface the instance: {hits:?}"
    );
    let none = h
        .indexer
        .sql_view_search_candidates("zzz-no-match", None)
        .await
        .unwrap();
    assert!(
        none.is_empty(),
        "non-matching query must yield no SQL candidates"
    );
}
