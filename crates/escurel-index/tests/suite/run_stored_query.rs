//! Integration tests for `Indexer::run_stored_query`.

use std::sync::Arc;

use bytes::Bytes;
use duckdb::Connection;
use escurel_embed::{Embedder, HashEmbedder};
use escurel_index::{Indexer, Migrator, QueryError};
use escurel_storage::{FsStore, Key, LaneStore};
use serde_json::json;
use tempfile::TempDir;

const TENANT: &str = "acme";

const SKILL_QUERY: (&str, &str) = (
    "markdown/skills/query.md",
    "---\n\
     type: skill\n\
     id: query\n\
     description: A reusable SQL view over the indexed corpus.\n\
     ---\n\
     # query\n",
);

const SKILL_CUSTOMER: (&str, &str) = (
    "markdown/skills/customer.md",
    "---\n\
     type: skill\n\
     id: customer\n\
     description: A buying entity.\n\
     ---\n\
     # customer\n",
);

const INSTANCE_ACME: (&str, &str) = (
    "markdown/instances/customer/acme-corp.md",
    "---\n\
     type: instance\n\
     skill: customer\n\
     id: acme-corp\n\
     ---\n\
     # Acme\n",
);

const INSTANCE_GLOBEX: (&str, &str) = (
    "markdown/instances/customer/globex-llc.md",
    "---\n\
     type: instance\n\
     skill: customer\n\
     id: globex-llc\n\
     ---\n\
     # Globex\n",
);

/// Query that counts pages of a given skill.
const QUERY_COUNT_BY_SKILL: (&str, &str) = (
    "markdown/instances/query/count-by-skill.md",
    "---\n\
     type: instance\n\
     skill: query\n\
     id: count-by-skill\n\
     db: relational\n\
     params:\n\
       - {name: skill, type: text, required: true}\n\
     sql: \"SELECT count(*) AS n FROM pages WHERE skill = :skill AND page_type = 'instance'\"\n\
     ---\n\
     # count-by-skill\n",
);

/// Query that lists slugs of a given skill, optionally filtering
/// by page_type.
const QUERY_SLUGS_FOR_SKILL: (&str, &str) = (
    "markdown/instances/query/slugs-for-skill.md",
    "---\n\
     type: instance\n\
     skill: query\n\
     id: slugs-for-skill\n\
     db: relational\n\
     params:\n\
       - {name: skill, type: text, required: true}\n\
       - {name: page_type, type: text, required: false}\n\
     sql: \"SELECT slug FROM pages WHERE skill = :skill AND page_type = 'instance' AND (:page_type IS NULL OR page_type = :page_type) ORDER BY slug\"\n\
     ---\n\
     # slugs-for-skill\n",
);

/// Query that projects temporal literals (DATE / TIMESTAMP / TIME),
/// used to assert ISO-8601 / RFC-3339 serialization (issue #211).
const QUERY_TEMPORAL: (&str, &str) = (
    "markdown/instances/query/temporal.md",
    "---\n\
     type: instance\n\
     skill: query\n\
     id: temporal\n\
     db: relational\n\
     params: []\n\
     sql: \"SELECT DATE '1997-01-01' AS d, TIMESTAMP '1997-01-01 00:00:00' AS ts, TIME '13:45:30' AS t\"\n\
     ---\n\
     # temporal\n",
);

/// Query that declares `db: ext` to verify the unsupported-db error.
const QUERY_EXT_DB: (&str, &str) = (
    "markdown/instances/query/external-db.md",
    "---\n\
     type: instance\n\
     skill: query\n\
     id: external-db\n\
     db: ext\n\
     params: []\n\
     sql: \"SELECT 1\"\n\
     ---\n\
     # external-db\n",
);

struct Harness {
    store: Arc<dyn LaneStore>,
    indexer: Indexer,
    _store_dir: TempDir,
    _db_dir: TempDir,
}

fn fresh_harness() -> Harness {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(HashEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Indexer::new(Arc::clone(&store), embedder, conn, TENANT).unwrap();
    Harness {
        store,
        indexer,
        _store_dir: store_dir,
        _db_dir: db_dir,
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

fn args(pairs: &[(&str, serde_json::Value)]) -> serde_json::Map<String, serde_json::Value> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

#[tokio::test]
async fn unknown_query_id_errors() {
    let h = fresh_harness();
    seed(&h, &[SKILL_QUERY]).await;
    let err = h
        .indexer
        .run_stored_query("no-such-query", &args(&[]))
        .await
        .expect_err("must error");
    assert!(matches!(err, QueryError::NotFound { .. }), "got: {err}");
}

#[tokio::test]
async fn missing_required_param_errors_before_sql_runs() {
    let h = fresh_harness();
    seed(&h, &[SKILL_QUERY, QUERY_COUNT_BY_SKILL]).await;
    let err = h
        .indexer
        .run_stored_query("count-by-skill", &args(&[]))
        .await
        .expect_err("missing required must error");
    match err {
        QueryError::MissingParam { name, .. } => assert_eq!(name, "skill"),
        other => panic!("expected MissingParam, got: {other}"),
    }
}

#[tokio::test]
async fn unknown_arg_errors_before_sql_runs() {
    let h = fresh_harness();
    seed(&h, &[SKILL_QUERY, QUERY_COUNT_BY_SKILL]).await;
    let err = h
        .indexer
        .run_stored_query(
            "count-by-skill",
            &args(&[("skill", json!("customer")), ("oops", json!(1))]),
        )
        .await
        .expect_err("unknown arg must error");
    match err {
        QueryError::UnknownParam { name, .. } => assert_eq!(name, "oops"),
        other => panic!("expected UnknownParam, got: {other}"),
    }
}

#[tokio::test]
async fn ext_db_returns_unsupported() {
    let h = fresh_harness();
    seed(&h, &[SKILL_QUERY, QUERY_EXT_DB]).await;
    let err = h
        .indexer
        .run_stored_query("external-db", &args(&[]))
        .await
        .expect_err("ext db must error");
    assert!(
        matches!(&err, QueryError::UnsupportedDb { db, .. } if db == "ext"),
        "got: {err}",
    );
}

#[tokio::test]
async fn runs_simple_count_with_required_param() {
    let h = fresh_harness();
    seed(
        &h,
        &[
            SKILL_QUERY,
            SKILL_CUSTOMER,
            INSTANCE_ACME,
            INSTANCE_GLOBEX,
            QUERY_COUNT_BY_SKILL,
        ],
    )
    .await;

    let out = h
        .indexer
        .run_stored_query("count-by-skill", &args(&[("skill", json!("customer"))]))
        .await
        .expect("query runs");

    assert_eq!(out.rows.len(), 1);
    let n = out.rows[0]["n"].as_i64().unwrap();
    assert_eq!(n, 2, "expected 2 customer rows (acme + globex)");
    assert_eq!(out.schema.len(), 1);
    assert_eq!(out.schema[0].name, "n");
}

#[tokio::test]
async fn optional_param_null_when_omitted() {
    let h = fresh_harness();
    seed(
        &h,
        &[
            SKILL_QUERY,
            SKILL_CUSTOMER,
            INSTANCE_ACME,
            INSTANCE_GLOBEX,
            QUERY_SLUGS_FOR_SKILL,
        ],
    )
    .await;

    // Without `page_type`, the WHERE clause's `(:page_type IS NULL
    // OR page_type = :page_type)` collapses to true and returns
    // both customer instances.
    let out = h
        .indexer
        .run_stored_query("slugs-for-skill", &args(&[("skill", json!("customer"))]))
        .await
        .unwrap();
    let slugs: Vec<_> = out
        .rows
        .iter()
        .filter_map(|r| r["slug"].as_str().map(str::to_owned))
        .collect();
    assert_eq!(slugs, vec!["acme-corp", "globex-llc"]);

    // With `page_type` = 'instance' the count is unchanged here
    // (both seeded customers are instances); with `page_type` =
    // 'skill' we expect zero rows.
    let out = h
        .indexer
        .run_stored_query(
            "slugs-for-skill",
            &args(&[("skill", json!("customer")), ("page_type", json!("skill"))]),
        )
        .await
        .unwrap();
    assert!(
        out.rows.is_empty(),
        "no customer-skilled SKILL pages exist; got: {:?}",
        out.rows,
    );
}

#[tokio::test]
async fn temporal_columns_serialize_as_iso8601() {
    // DATE / TIMESTAMP / TIME columns must come back as usable
    // ISO-8601 / RFC-3339 strings, not the Rust `Debug` form of
    // DuckDB's `Value` (e.g. "Date32(9862)"). See issue #211.
    let h = fresh_harness();
    seed(&h, &[SKILL_QUERY, QUERY_TEMPORAL]).await;

    let out = h
        .indexer
        .run_stored_query("temporal", &args(&[]))
        .await
        .expect("query runs");

    assert_eq!(out.rows.len(), 1);
    let row = &out.rows[0];
    assert_eq!(
        row["d"].as_str(),
        Some("1997-01-01"),
        "DATE → ISO date; got {:?}",
        row["d"],
    );
    assert_eq!(
        row["ts"].as_str(),
        Some("1997-01-01T00:00:00Z"),
        "TIMESTAMP → RFC-3339 UTC; got {:?}",
        row["ts"],
    );
    assert_eq!(
        row["t"].as_str(),
        Some("13:45:30"),
        "TIME → ISO time; got {:?}",
        row["t"],
    );
}

#[tokio::test]
async fn parameter_binding_blocks_sql_injection_via_value() {
    // Classic attempt: pass `'; DROP TABLE pages; --` as the
    // `skill` value. Because args are bound as parameters, the
    // query just counts rows where `skill` literally equals that
    // string (zero) — no DDL is executed.
    let h = fresh_harness();
    seed(
        &h,
        &[
            SKILL_QUERY,
            SKILL_CUSTOMER,
            INSTANCE_ACME,
            QUERY_COUNT_BY_SKILL,
        ],
    )
    .await;

    let injected = json!("'; DROP TABLE pages; --");
    let out = h
        .indexer
        .run_stored_query("count-by-skill", &args(&[("skill", injected)]))
        .await
        .unwrap();
    assert_eq!(out.rows[0]["n"].as_i64().unwrap(), 0);

    // And pages is still there.
    let count = h
        .indexer
        .run_stored_query("count-by-skill", &args(&[("skill", json!("customer"))]))
        .await
        .unwrap();
    assert_eq!(count.rows[0]["n"].as_i64().unwrap(), 1);
}
