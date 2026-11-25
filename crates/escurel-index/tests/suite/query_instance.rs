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
    "---\nkind: skill\nid: query\ndescription: Reusable parameterised reads.\n---\n# query\n",
);

/// A public sql_view skill (default tenant read policy ⇒ `public` may read).
const SKILL_SALES: (&str, &str) = (
    "markdown/skills/sales.md",
    "---\nkind: skill\nid: sales\ndescription: Sales lines, mirrored read-only.\n\
     backend:\n  kind: sql_view\n  source: { connector: json_dir, relation: /unused }\n\
     \x20\x20search_text: [category]\n---\n# sales\n",
);

/// An owner-private sql_view skill — a non-owner, non-admin caller must be
/// denied (fail-closed), exercising the per-instance ACL on the read path.
const SKILL_SECRET: (&str, &str) = (
    "markdown/skills/secret_sales.md",
    "---\nkind: skill\nid: secret_sales\ndescription: Owner-private sales.\n\
     visibility: owner\nowner_field: credential\n\
     backend:\n  kind: sql_view\n  source: { connector: json_dir, relation: /unused }\n\
     \x20\x20search_text: [category]\n---\n# secret_sales\n",
);

/// Aggregating report: total amount per category, with a runtime floor bound
/// as `:min`. References the target view via `{{target}}`.
const QUERY_BY_CATEGORY: (&str, &str) = (
    "markdown/instances/query/sales-by-category.md",
    "---\nkind: instance\nskill: query\nid: sales-by-category\n\
     target: \"[[sales::eu]]\"\n\
     params:\n  - {name: min, type: number, required: true}\n\
     sql: \"SELECT category, SUM(amount)::BIGINT AS total FROM {{target}} WHERE amount >= :min GROUP BY category ORDER BY category\"\n\
     ---\n# sales-by-category\n",
);

/// Report filtering on a string category — used to prove an injection string
/// passed as `:cat` is bound, not interpolated.
const QUERY_BY_NAME: (&str, &str) = (
    "markdown/instances/query/sales-by-name.md",
    "---\nkind: instance\nskill: query\nid: sales-by-name\n\
     target: \"[[sales::eu]]\"\n\
     params:\n  - {name: cat, type: text, required: true}\n\
     sql: \"SELECT category, SUM(amount)::BIGINT AS total FROM {{target}} WHERE category = :cat GROUP BY category\"\n\
     ---\n# sales-by-name\n",
);

/// A query page with NO `target` — `query_instance` must refuse it (that's
/// what `run_stored_query` is for).
const QUERY_NO_TARGET: (&str, &str) = (
    "markdown/instances/query/no-target.md",
    "---\nkind: instance\nskill: query\nid: no-target\n\
     params: []\nsql: \"SELECT 1 AS n\"\n---\n# no-target\n",
);

/// A query whose target points at an owner-private instance.
const QUERY_SECRET: (&str, &str) = (
    "markdown/instances/query/secret-by-category.md",
    "---\nkind: instance\nskill: query\nid: secret-by-category\n\
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
        actor: None,
        run_id: None,
        root_event_id: None,
        agent_skill: None,
        agent: None,
        service: None,
        same_person: None,
    }
}

fn admin(subject: &str) -> AclCaller<'_> {
    AclCaller {
        subject,
        is_admin: true,
        token_groups: &[],
        actor: None,
        run_id: None,
        root_event_id: None,
        agent_skill: None,
        agent: None,
        service: None,
        same_person: None,
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
            None,
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
            None,
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
        .query_instance(
            "sales-by-name",
            &args(&[("cat", injected)]),
            None,
            &analyst("u1"),
        )
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
            None,
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
        .query_instance("sales-by-category", &args(&[]), None, &analyst("u1"))
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
        .query_instance("no-target", &args(&[]), None, &analyst("u1"))
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
        .query_instance("secret-by-category", &args(&[]), None, &analyst("intruder"))
        .await
        .expect_err("non-owner must be denied");
    assert!(matches!(err, QueryError::Forbidden { .. }), "got {err}");

    // Admin bypasses the per-instance ACL.
    let out = h
        .indexer
        .query_instance("secret-by-category", &args(&[]), None, &admin("root"))
        .await
        .expect("admin reads");
    assert_eq!(out.rows.len(), 2);
}

// ── Resource bounds on the query path (#455) ─────────────────────────────
//
// The fleet sub-epic names these as gating the whole feature, and its own
// words are "none exist today": an authored query has no statement timeout,
// so a runaway one holds the tenant's single DuckDB connection for as long as
// it likes. DuckDB has no statement-timeout setting at all — the only bound
// it offers is `Connection::interrupt` — so the bound has to be armed around
// the execution.
//
// `SessionWatchdog` already proves the primitive works for a delegated Quack
// session; what was missing is it being wired to the surface that actually
// executes tenant-authored SQL.

/// A query page whose SQL runs effectively forever on any real engine: a
/// three-way cross join over a generated range, aggregated so nothing can be
/// streamed out early.
/// A deliberately runaway query. It cross-joins its target with three
/// four-million-row ranges, so it names `{{target}}` — which `substitute_target`
/// now requires — while still being unfinishable. The earlier fixture omitted
/// the placeholder entirely, which is exactly the shape that is refused before
/// execution now, and would have left this test asserting nothing.
const QUERY_RUNAWAY: (&str, &str) = (
    "markdown/instances/query/runaway.md",
    "---\nkind: instance\nskill: query\nid: runaway\n\
     target: \"[[sales::eu]]\"\n\
     sql: \"SELECT COUNT(*)::BIGINT AS n FROM {{target}} t, range(4000000) a, range(4000000) b, range(4000000) c\"\n\
     ---\n# runaway\n",
);

/// The bound the sub-epic asks for, with the failing test it asks for:
/// a runaway authored query is KILLED, and it does not hold the tenant's
/// query surface for ever afterwards.
#[tokio::test]
async fn a_runaway_query_is_killed_at_its_deadline() {
    let h = fresh_harness();
    seed(&h, &[SKILL_QUERY, SKILL_SALES]).await;
    materialise_sales(&h, "sales", "eu").await;
    seed(&h, &[QUERY_BY_CATEGORY, QUERY_RUNAWAY]).await;

    // A short bound, so the test is about the mechanism and not about
    // waiting. Production configures this; the default is generous.
    h.indexer
        .set_query_timeout(std::time::Duration::from_millis(400));

    let started = std::time::Instant::now();
    let err = h
        .indexer
        .query_instance("runaway", &args(&[]), None, &admin("root"))
        .await
        .expect_err("a runaway query must not be allowed to finish");
    let elapsed = started.elapsed();

    assert!(
        matches!(err, QueryError::Interrupted { .. }),
        "the refusal must say the query was cut short, not report a generic \
         engine error a caller cannot act on: {err:?}"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(20),
        "the deadline must actually bound the work; took {elapsed:?}"
    );

    // ...and the surface still works afterwards. A bound that leaves the
    // connection poisoned, or the interrupt flag set, would turn one bad
    // query into a dead tenant — which is the availability event the bound
    // exists to prevent.
    let out = h
        .indexer
        .query_instance(
            "sales-by-category",
            &args(&[("min", json!(0))]),
            None,
            &admin("root"),
        )
        .await
        .expect("the next query must run normally");
    assert!(
        !out.rows.is_empty(),
        "the tenant's query surface must survive a killed query: {out:?}"
    );
}

/// An ordinary query is not affected by the bound existing — the guard is
/// disarmed on the success path, and nothing is reported as interrupted.
#[tokio::test]
async fn a_normal_query_is_unaffected_by_the_bound() {
    let h = fresh_harness();
    seed(&h, &[SKILL_QUERY, SKILL_SALES]).await;
    materialise_sales(&h, "sales", "eu").await;
    seed(&h, &[QUERY_BY_CATEGORY]).await;

    // Even a tight bound must not interfere with a query that finishes.
    h.indexer
        .set_query_timeout(std::time::Duration::from_secs(30));
    let out = h
        .indexer
        .query_instance(
            "sales-by-category",
            &args(&[("min", json!(0))]),
            None,
            &admin("root"),
        )
        .await
        .expect("a normal query must not be interrupted");
    assert!(!out.rows.is_empty(), "{out:?}");
}

// ── Authored SQL reaches only its target (#854) ──────────────────────────
//
// `query_instance` checks the caller may read the TARGET, then runs the
// authored SQL on the indexer's own connection — the one that also holds
// `external_credentials` (plaintext DSNs), every other instance's `vw_` view,
// `pages`, the search projection, and DuckDB's file readers. The ACL check was
// about the target; the SQL could name anything else. These pin the gate: a
// statement is accepted only when every relation it reads is the target view
// or one of its own CTEs, and every table function is a pure generator.

/// A query page `gate-<n>` over the public `sales::eu` target with `sql`.
fn gate_page(n: usize, sql: &str) -> (&'static str, &'static str) {
    let path: &'static str =
        Box::leak(format!("markdown/instances/query/gate-{n}.md").into_boxed_str());
    let body = format!(
        "---\nkind: instance\nskill: query\nid: gate-{n}\n\
         target: \"[[sales::eu]]\"\n\
         sql: {}\n---\n# gate-{n}\n",
        serde_json::to_string(sql).unwrap()
    );
    (path, Box::leak(body.into_boxed_str()))
}

async fn gate_harness() -> Harness {
    let h = fresh_harness();
    seed(&h, &[SKILL_QUERY, SKILL_SALES, SKILL_SECRET]).await;
    materialise_sales(&h, "sales", "eu").await;
    materialise_sales(&h, "secret_sales", "eu").await;
    h.indexer
        .register_credential(
            "crm_pg",
            "postgres",
            "postgres://u:hunter2@db/crm",
            Some("root"),
        )
        .await
        .unwrap();
    h
}

#[tokio::test]
async fn authored_sql_cannot_read_beyond_its_target() {
    let h = gate_harness().await;
    let refused = [
        "SELECT e.secret FROM {{target}} CROSS JOIN external_credentials e",
        "SELECT secret FROM main.external_credentials, {{target}}",
        "SELECT (SELECT max(secret) FROM external_credentials) AS s FROM {{target}}",
        "SELECT * FROM {{target}} WHERE category IN (SELECT secret FROM external_credentials)",
        "WITH c AS (SELECT secret FROM external_credentials) SELECT * FROM {{target}}, c",
        "SELECT s.* FROM {{target}}, vw_secret_sales__eu s",
        "SELECT x.text FROM {{target}}, sql_view_search_text x",
        "SELECT p.page_id FROM {{target}}, pages p",
        "SELECT * FROM {{target}}, read_text('/etc/hostname')",
        "SELECT * FROM {{target}}, read_json_auto('/etc/hostname')",
        "SELECT * FROM {{target}}, '/etc/hostname'",
        "SELECT * FROM {{target}}, duckdb_secrets()",
        "SELECT * FROM {{target}}, duckdb_tables()",
        "SELECT * FROM {{target}}, query_table('external_credentials')",
        "SELECT * FROM {{target}}, query('SELECT secret FROM external_credentials')",
        "SELECT getenv('HOME') AS h FROM {{target}}",
        "SELECT current_setting('threads') AS t FROM {{target}}",
        "COPY (SELECT * FROM {{target}}) TO '/tmp/escurel-gate-854.csv'",
        "SELECT 1 FROM {{target}}; DELETE FROM pages",
        "DELETE FROM pages WHERE page_id IN (SELECT category FROM {{target}})",
        // A CTE is visible only below the query that declares it: a sibling
        // `pages` is the real table.
        "SELECT p.page_id FROM {{target}}, (WITH pages AS (SELECT 1 AS x) SELECT * FROM pages) a, pages p",
        "SELECT category FROM {{target}} UNION ALL SELECT secret FROM external_credentials",
        "FROM external_credentials, {{target}} SELECT secret",
        "SELECT * FROM {{target}}, (SUMMARIZE external_credentials)",
        "SELECT * FROM {{target}} PIVOT (sum(amount) FOR category IN ('hw', 'sw'))",
        "SELECT * FROM {{target}}, system.main.duckdb_secrets()",
        "SELECT main.getenv('HOME') AS h FROM {{target}}",
    ];
    for (n, sql) in refused.iter().enumerate() {
        seed(&h, &[gate_page(n, sql)]).await;
        let r = h
            .indexer
            .query_instance(&format!("gate-{n}"), &args(&[]), None, &analyst("reader"))
            .await;
        match r {
            Err(QueryError::SqlNotAllowed { .. }) => {}
            Err(other) => panic!("{sql}\n  refused for the wrong reason: {other}"),
            Ok(out) => panic!(
                "{sql}\n  ran and returned {} rows: {:?}",
                out.rows.len(),
                out.rows.first()
            ),
        }
    }
    assert!(
        !std::path::Path::new("/tmp/escurel-gate-854.csv").exists(),
        "COPY ran"
    );
    // The multi-statement and DELETE cases left the corpus alone.
    let still = h
        .indexer
        .query_instance(
            "sales-by-category",
            &args(&[("min", json!(0))]),
            None,
            &analyst("reader"),
        )
        .await;
    assert!(
        matches!(still, Err(QueryError::NotFound { .. })) || still.is_ok(),
        "pages table damaged: {still:?}"
    );
}

#[tokio::test]
async fn authored_sql_over_its_target_keeps_working() {
    let h = gate_harness().await;
    // The shapes the lab's report pages use: CTEs (also shadowing nothing),
    // self-joins, range()/generate_series()/unnest() generators, VALUES,
    // window functions, subqueries over the target, and `:param` binds.
    let allowed = [
        "SELECT category, SUM(amount)::BIGINT AS total FROM {{target}} GROUP BY category ORDER BY category",
        "WITH o AS (SELECT * FROM {{target}}), m AS (SELECT m FROM range(1, 3) AS t(m)) \
         SELECT count(*)::BIGINT AS n FROM o, m JOIN {{target}} t2 ON true",
        "SELECT g::BIGINT AS g FROM generate_series(1, 2) AS s(g), (SELECT 1 FROM {{target}} LIMIT 1) x",
        "SELECT unnest([1, 2])::BIGINT AS u FROM {{target}} LIMIT 2",
        "SELECT v.a FROM (VALUES (1), (2)) AS v(a), (SELECT max(amount) AS mx FROM {{target}}) t",
        "SELECT category, row_number() OVER (ORDER BY amount) AS r FROM {{target}}",
        "SELECT * FROM {{target}} WHERE amount > (SELECT avg(amount) FROM {{target}})",
        "SELECT EXTRACT(year FROM DATE '2026-01-01')::BIGINT AS y FROM {{target}} LIMIT 1",
        // A CTE may shadow an internal name: it is the CTE that is read.
        "WITH pages AS (SELECT * FROM {{target}}) SELECT count(*)::BIGINT AS n FROM pages",
        "WITH RECURSIVE r(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM r WHERE i < 3) \
         SELECT i::BIGINT AS i FROM r, (SELECT 1 FROM {{target}} LIMIT 1) t",
        "SELECT category, list(amount ORDER BY amount) AS amounts, [1, 2] AS l, {'a': 1} AS s \
         FROM {{target}} GROUP BY category",
        "SELECT t.category FROM {{target}} t, LATERAL (SELECT t.amount * 2 AS d) x",
    ];
    for (n, sql) in allowed.iter().enumerate() {
        let n = 100 + n;
        seed(&h, &[gate_page(n, sql)]).await;
        h.indexer
            .query_instance(&format!("gate-{n}"), &args(&[]), None, &analyst("reader"))
            .await
            .unwrap_or_else(|e| panic!("{sql}\n  refused: {e}"));
    }
}
