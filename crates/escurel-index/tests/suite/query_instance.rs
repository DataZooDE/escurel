//! Integration tests for `Indexer::query_instance` (issue #205).
//!
//! A `[[query::*]]` page that declares a `target: [[skill::id]]` runs its
//! parameterised SQL **against that instance's managed `vw_…` view**, binding
//! every `:param` runtime value as a DuckDB prepared-statement parameter (the
//! `run_stored_query` pattern) and substituting the `{{target}}` placeholder
//! with the allow-listed view identifier. Real DuckDB + `FsStore`, no mocks;
//! the view is materialised over an offline `json_dir`.

use std::sync::Arc;

use bytes::Bytes;
use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::backend::{SqlConnector, SqlViewBackend, SqlViewBinding};
use escurel_index::{AclCaller, Indexer, Migrator, QueryError};
use escurel_storage::{FsStore, Key, LaneStore};
use serde_json::json;
use tempfile::TempDir;

const TENANT: &str = "acme";

const SKILL_QUERY: (&str, &str) = (
    "markdown/skills/query.md",
    "---\ntype: skill\nid: query\ndescription: Reusable parameterised reads.\n---\n# query\n",
);

/// A public sql_view skill (default tenant read policy ⇒ `public` may read).
const SKILL_SALES: (&str, &str) = (
    "markdown/skills/sales.md",
    "---\ntype: skill\nid: sales\ndescription: Sales lines, mirrored read-only.\n\
     backend:\n  kind: sql_view\n  source: { connector: json_dir, relation: /unused }\n\
     search_text: [category]\n---\n# sales\n",
);

/// An owner-private sql_view skill — a non-owner, non-admin caller must be
/// denied (fail-closed), exercising the per-instance ACL on the read path.
const SKILL_SECRET: (&str, &str) = (
    "markdown/skills/secret_sales.md",
    "---\ntype: skill\nid: secret_sales\ndescription: Owner-private sales.\n\
     visibility: owner\nowner_field: credential\n\
     backend:\n  kind: sql_view\n  source: { connector: json_dir, relation: /unused }\n\
     search_text: [category]\n---\n# secret_sales\n",
);

/// Aggregating report: total amount per category, with a runtime floor bound
/// as `:min`. References the target view via `{{target}}`.
const QUERY_BY_CATEGORY: (&str, &str) = (
    "markdown/instances/query/sales-by-category.md",
    "---\ntype: instance\nskill: query\nid: sales-by-category\n\
     target: \"[[sales::eu]]\"\n\
     params:\n  - {name: min, type: number, required: true}\n\
     sql: \"SELECT category, SUM(amount)::BIGINT AS total FROM {{target}} WHERE amount >= :min GROUP BY category ORDER BY category\"\n\
     ---\n# sales-by-category\n",
);

/// Report filtering on a string category — used to prove an injection string
/// passed as `:cat` is bound, not interpolated.
const QUERY_BY_NAME: (&str, &str) = (
    "markdown/instances/query/sales-by-name.md",
    "---\ntype: instance\nskill: query\nid: sales-by-name\n\
     target: \"[[sales::eu]]\"\n\
     params:\n  - {name: cat, type: text, required: true}\n\
     sql: \"SELECT category, SUM(amount)::BIGINT AS total FROM {{target}} WHERE category = :cat GROUP BY category\"\n\
     ---\n# sales-by-name\n",
);

/// A query page with NO `target` — `query_instance` must refuse it (that's
/// what `run_stored_query` is for).
const QUERY_NO_TARGET: (&str, &str) = (
    "markdown/instances/query/no-target.md",
    "---\ntype: instance\nskill: query\nid: no-target\n\
     params: []\nsql: \"SELECT 1 AS n\"\n---\n# no-target\n",
);

/// A query whose target points at an owner-private instance.
const QUERY_SECRET: (&str, &str) = (
    "markdown/instances/query/secret-by-category.md",
    "---\ntype: instance\nskill: query\nid: secret-by-category\n\
     target: \"[[secret_sales::eu]]\"\n\
     params: []\n\
     sql: \"SELECT category, SUM(amount) AS total FROM {{target}} GROUP BY category\"\n\
     ---\n# secret-by-category\n",
);

struct Harness {
    store: Arc<dyn LaneStore>,
    indexer: Arc<Indexer>,
    _store_dir: TempDir,
    _db_dir: TempDir,
    data_dir: TempDir,
}

fn fresh_harness() -> Harness {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let data_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
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
        let key = Key::new(TENANT, path.to_owned()).unwrap();
        h.store
            .write(&key, Bytes::from_static(body.as_bytes()))
            .await
            .unwrap();
        h.indexer.update_page(path, body).await.unwrap();
    }
}

/// Materialise a `sql_view` instance `skill/id` over the harness data dir,
/// after writing three sales rows (hw 30, hw 20, sw 5).
async fn materialise_sales(h: &Harness, skill: &str, id: &str) {
    let dir = h.data_dir.path();
    std::fs::write(dir.join("a.json"), br#"{"category":"hw","amount":30}"#).unwrap();
    std::fs::write(dir.join("b.json"), br#"{"category":"hw","amount":20}"#).unwrap();
    std::fs::write(dir.join("c.json"), br#"{"category":"sw","amount":5}"#).unwrap();
    let binding = SqlViewBinding {
        connector: SqlConnector::JsonDir,
        attach: None,
        relation: dir.to_str().unwrap().to_owned(),
        filter: None,
        project: Default::default(),
        search_text: vec!["category".to_owned()],
    };
    SqlViewBackend::new(Arc::clone(&h.indexer))
        .create_instance(skill, &binding, id, "# overlay")
        .await
        .expect("materialise sql_view instance");
}

fn args(pairs: &[(&str, serde_json::Value)]) -> serde_json::Map<String, serde_json::Value> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

fn analyst(subject: &str) -> AclCaller<'_> {
    AclCaller {
        subject,
        is_admin: false,
        token_groups: &[],
    }
}

fn admin(subject: &str) -> AclCaller<'_> {
    AclCaller {
        subject,
        is_admin: true,
        token_groups: &[],
    }
}

#[tokio::test]
async fn aggregates_rows_with_bound_runtime_param() {
    let h = fresh_harness();
    seed(&h, &[SKILL_QUERY, SKILL_SALES]).await;
    materialise_sales(&h, "sales", "eu").await;
    seed(&h, &[QUERY_BY_CATEGORY]).await;

    // min=10 drops the sw row (amount 5); hw aggregates 30+20=50.
    let out = h
        .indexer
        .query_instance(
            "sales-by-category",
            &args(&[("min", json!(10))]),
            &analyst("u1"),
        )
        .await
        .expect("query runs");

    assert_eq!(out.rows.len(), 1, "sw filtered out: {:?}", out.rows);
    assert_eq!(out.rows[0]["category"], "hw");
    assert_eq!(out.rows[0]["total"].as_i64().unwrap(), 50);
    assert!(!out.truncated);
    assert!(out.schema.iter().any(|c| c.name == "total"));

    // min=0 keeps both categories (aggregation pushed into the view query).
    let out = h
        .indexer
        .query_instance(
            "sales-by-category",
            &args(&[("min", json!(0))]),
            &analyst("u1"),
        )
        .await
        .unwrap();
    assert_eq!(out.rows.len(), 2, "both categories: {:?}", out.rows);
}

#[tokio::test]
async fn runtime_param_is_bound_not_interpolated() {
    // The classic injection string passed as a runtime value must bind as a
    // literal (zero matching rows) — never reach the SQL text.
    let h = fresh_harness();
    seed(&h, &[SKILL_QUERY, SKILL_SALES]).await;
    materialise_sales(&h, "sales", "eu").await;
    seed(&h, &[QUERY_BY_NAME]).await;

    let injected = json!("hw'; DROP TABLE pages; --");
    let out = h
        .indexer
        .query_instance("sales-by-name", &args(&[("cat", injected)]), &analyst("u1"))
        .await
        .expect("injection value binds, does not error");
    assert!(
        out.rows.is_empty(),
        "no category equals the injection literal"
    );

    // `pages` survives, and a legitimate value still matches.
    let ok = h
        .indexer
        .query_instance(
            "sales-by-name",
            &args(&[("cat", json!("hw"))]),
            &analyst("u1"),
        )
        .await
        .unwrap();
    assert_eq!(ok.rows[0]["total"].as_i64().unwrap(), 50);
}

#[tokio::test]
async fn missing_required_param_errors_before_sql() {
    let h = fresh_harness();
    seed(&h, &[SKILL_QUERY, SKILL_SALES]).await;
    materialise_sales(&h, "sales", "eu").await;
    seed(&h, &[QUERY_BY_CATEGORY]).await;

    let err = h
        .indexer
        .query_instance("sales-by-category", &args(&[]), &analyst("u1"))
        .await
        .expect_err("missing required must error");
    assert!(matches!(err, QueryError::MissingParam { .. }), "got {err}");
}

#[tokio::test]
async fn query_without_target_is_rejected() {
    let h = fresh_harness();
    seed(&h, &[SKILL_QUERY, QUERY_NO_TARGET]).await;
    let err = h
        .indexer
        .query_instance("no-target", &args(&[]), &analyst("u1"))
        .await
        .expect_err("query_instance requires a target");
    assert!(matches!(err, QueryError::MissingTarget { .. }), "got {err}");
}

#[tokio::test]
async fn acl_denies_non_owner_on_owner_private_target() {
    // The target instance is owner-private; a non-admin, non-owner caller is
    // denied (fail-closed) — admin bypasses and reads the aggregated rows.
    let h = fresh_harness();
    seed(&h, &[SKILL_QUERY, SKILL_SECRET]).await;
    materialise_sales(&h, "secret_sales", "eu").await;
    seed(&h, &[QUERY_SECRET]).await;

    let err = h
        .indexer
        .query_instance("secret-by-category", &args(&[]), &analyst("intruder"))
        .await
        .expect_err("non-owner must be denied");
    assert!(matches!(err, QueryError::Forbidden { .. }), "got {err}");

    // Admin bypasses the per-instance ACL.
    let out = h
        .indexer
        .query_instance("secret-by-category", &args(&[]), &admin("root"))
        .await
        .expect("admin reads");
    assert_eq!(out.rows.len(), 2);
}
