//! Keyset paging over keys of every type, and over keys that are not well behaved (crew review).
//!
//! The ORDER BY and the `>` comparison run over `CAST(key AS VARCHAR)`, but the cursor used to be
//! built from the JSON rendering of the key: a TIMESTAMP's `T`/`Z` form sorts after its cast form
//! (`' ' < 'T'`), a DECIMAL went through f64 (`1.50` -> `1.5`), a NULL key became the empty string
//! (an empty page id, and a cursor that restarted at the first page: an infinite loop), and a
//! non-ASCII cursor panicked the page read while it held the connection lock.
//!
//! Real gateway over `POST /mcp`, real DuckDB, real parquet files with typed columns, no mocks.

use std::path::Path;
use std::sync::Arc;

use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, LaneStore};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, Opts};
use serde_json::{Value, json};
use tempfile::TempDir;

const TENANT: &str = "acme";

struct Rig {
    p: EscurelProcess,
    _dirs: Vec<TempDir>,
}

/// A gateway whose `thing` skill has one instance per row of a parquet file made by `select_sql`
/// (a DuckDB query returning a column `k` plus anything else).
async fn rig(select_sql: &str) -> Rig {
    rig_with(select_sql, None).await
}

async fn rig_with(select_sql: &str, timeout: Option<std::time::Duration>) -> Rig {
    rig_full(select_sql, timeout, "").await
}

/// `extra_backend` is appended under `backend:` (e.g. a `filterable:` line).
async fn rig_full(
    select_sql: &str,
    timeout: Option<std::time::Duration>,
    extra_backend: &str,
) -> Rig {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let src_dir = TempDir::new().unwrap();
    {
        let c = Connection::open_in_memory().unwrap();
        let file = src_dir.path().join("things.parquet");
        c.execute_batch(&format!(
            "COPY ({select_sql}) TO '{}' (FORMAT PARQUET)",
            file.display()
        ))
        .unwrap();
    }
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let mut indexer = Indexer::new(store, embedder, conn, TENANT).unwrap();
    if let Some(t) = timeout {
        indexer = indexer.with_rows_query_timeout(t);
    }
    let indexer = Arc::new(indexer);
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::Disabled,
        config_overrides: ConfigOverrides {
            indexer: Some(indexer),
            ..Default::default()
        },
        ..Default::default()
    })
    .await;
    let skill = skill_page(src_dir.path(), extra_backend);
    let r = call(
        &p,
        "update_page",
        json!({ "page_id": "markdown/skills/thing.md", "content": skill }),
    )
    .await;
    assert_eq!(r["result"]["structuredContent"]["ok"], true, "{r}");
    Rig {
        p,
        _dirs: vec![store_dir, db_dir, src_dir],
    }
}

fn skill_page(src: &Path, extra_backend: &str) -> String {
    format!(
        "---\nkind: skill\nid: thing\ndescription: Rows with a typed key.\nbackend:\n  kind: sql_view\n  \
         instances: rows\n  key: k\n  source: {{connector: parquet_dir, relation: \"{}\"}}\n{extra_backend}---\n# thing\n",
        src.display()
    )
}

/// The JSON-RPC envelope of one `tools/call`.
async fn call(p: &EscurelProcess, name: &str, args: Value) -> Value {
    reqwest::Client::new()
        .post(p.mcp_url())
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": { "name": name, "arguments": args } }))
        .send()
        .await
        .expect("the server must answer (a panic drops the connection)")
        .json()
        .await
        .expect("json")
}

/// Every page id of `thing`, paged at `limit`, in the order the server returns them.
async fn page_all(p: &EscurelProcess, limit: usize) -> Vec<String> {
    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..200 {
        let r = call(
            p,
            "list_instances",
            json!({ "skill": "thing", "limit": limit, "cursor": cursor }),
        )
        .await;
        let sc = &r["result"]["structuredContent"];
        for i in sc["instances"].as_array().unwrap_or(&Vec::new()) {
            seen.push(i["page_id"].as_str().unwrap().to_owned());
        }
        match sc["next_cursor"].as_str() {
            Some(c) => cursor = Some(c.to_owned()),
            None => return seen,
        }
    }
    panic!("the cursor never terminated: {} rows seen", seen.len());
}

fn assert_exactly_once(seen: &[String], expected: usize, what: &str) {
    let mut s = seen.to_vec();
    s.sort();
    s.dedup();
    assert_eq!(s.len(), seen.len(), "{what}: no row twice");
    assert_eq!(seen.len(), expected, "{what}: every row once");
    assert!(
        seen.iter().all(|p| !p.ends_with("/.md")),
        "{what}: no empty ids"
    );
}

#[tokio::test]
async fn a_timestamp_key_pages_every_row_exactly_once() {
    // 40 timestamps on the SAME day, minutes apart: the cast form (`2026-09-01 10:03:00`) and the
    // JSON form (`2026-09-01T10:03:00Z`) disagree at the 11th character.
    let t = rig(
        "SELECT (TIMESTAMP '2026-09-01 10:00:00' + INTERVAL (i) MINUTE) AS k, i AS n \
                 FROM range(40) t(i)",
    )
    .await;
    let seen = page_all(&t.p, 4).await;
    assert_exactly_once(&seen, 40, "timestamp keys");
    t.p.shutdown().await;
}

#[tokio::test]
async fn a_decimal_key_pages_every_row_exactly_once() {
    // 1.50, 2.00 ... : through f64 the cursor would read `1.5`/`2`.
    let t = rig("SELECT CAST(1 + i * 0.5 AS DECIMAL(10,2)) AS k, i AS n FROM range(30) t(i)").await;
    let seen = page_all(&t.p, 4).await;
    assert_exactly_once(&seen, 30, "decimal keys");
    t.p.shutdown().await;
}

#[tokio::test]
async fn varchar_and_integer_keys_still_page_exactly_once() {
    let t = rig("SELECT 'k' || lpad(CAST(i AS VARCHAR), 4, '0') AS k FROM range(25) t(i)").await;
    assert_exactly_once(&page_all(&t.p, 7).await, 25, "varchar keys");
    t.p.shutdown().await;
    let t = rig("SELECT i AS k FROM range(25) t(i)").await;
    assert_exactly_once(&page_all(&t.p, 7).await, 25, "integer keys");
    t.p.shutdown().await;
}

#[tokio::test]
async fn rows_with_a_null_key_are_not_instances_and_cannot_loop_the_cursor() {
    // 20 real keys and 6 NULL keys. A NULL key has no identity: it used to render as '' (an empty
    // page id), and a cursor of '' restarted at page one for ever.
    let t = rig("SELECT CASE WHEN i % 4 = 0 AND i < 24 THEN NULL ELSE 'k' || lpad(CAST(i AS VARCHAR), 3, '0') END AS k \
                 FROM range(26) t(i)")
    .await;
    let seen = page_all(&t.p, 3).await;
    assert_exactly_once(&seen, 20, "keys that are not NULL");
    t.p.shutdown().await;
}

#[tokio::test]
async fn a_cursor_that_is_not_ascii_is_a_clean_error_and_the_server_keeps_answering() {
    let t = rig("SELECT 'k' || lpad(CAST(i AS VARCHAR), 3, '0') AS k FROM range(10) t(i)").await;
    let bad = call(
        &t.p,
        "list_instances",
        json!({ "skill": "thing", "limit": 3, "cursor": "a\u{e9}b" }),
    )
    .await;
    assert!(
        bad.get("error").is_some() || bad["result"]["isError"] == json!(true),
        "an invalid cursor is refused: {bad}"
    );
    // The panic used to happen while the connection lock was held.
    let ok = call(
        &t.p,
        "list_instances",
        json!({ "skill": "thing", "limit": 3 }),
    )
    .await;
    assert_eq!(
        ok["result"]["structuredContent"]["instances"]
            .as_array()
            .map(Vec::len),
        Some(3),
        "{ok}"
    );
    t.p.shutdown().await;
}

#[tokio::test]
async fn a_source_query_that_runs_too_long_is_interrupted_and_does_not_hold_the_connection() {
    // 2M rows sorted by a cast key cannot finish in a millisecond: the statement must be interrupted
    // with a worded error instead of holding the single connection lock until it ends.
    let t = rig_with(
        "SELECT 'k' || lpad(CAST(i AS VARCHAR), 9, '0') AS k FROM range(2000000) t(i)",
        Some(std::time::Duration::from_millis(1)),
    )
    .await;
    let r = call(
        &t.p,
        "list_instances",
        json!({ "skill": "thing", "limit": 100 }),
    )
    .await;
    let msg = r.to_string();
    assert!(
        r.get("error").is_some() || r["result"]["isError"] == json!(true),
        "an overrunning source query is an error, not a long wait: {msg}"
    );
    assert!(
        msg.contains("interrupted") || msg.contains("did not answer"),
        "the error says what happened: {msg}"
    );
    // The lock is free again: an unrelated read on the same connection answers at once.
    let skills = call(&t.p, "list_skills", json!({})).await;
    assert!(skills.get("error").is_none(), "{skills}");
    t.p.shutdown().await;
}

#[tokio::test]
async fn reading_one_row_is_under_the_same_statement_timeout_as_listing() {
    // `rows_get` (what `expand` of a row page runs) used to be the only rows read WITHOUT the watchdog: a
    // slow source held the tenant's single DuckDB connection for as long as it liked. A 1 ms budget over a
    // 2M-row source must end in a worded error, and the connection must be free again.
    let t = rig_with(
        "SELECT 'k' || lpad(CAST(i AS VARCHAR), 9, '0') AS k FROM range(2000000) t(i)",
        Some(std::time::Duration::from_millis(1)),
    )
    .await;
    let r = call(
        &t.p,
        "expand",
        json!({ "page_id": "markdown/instances/thing/k000001999.md" }),
    )
    .await;
    let msg = r.to_string();
    assert!(
        msg.contains("interrupted") || msg.contains("did not answer"),
        "a slow single-row read must be interrupted with a worded error, not wait it out: {msg}"
    );
    let skills = call(&t.p, "list_skills", json!({})).await;
    assert!(skills.get("error").is_none(), "{skills}");
    t.p.shutdown().await;
}

// ----------------------------------------------------------------- hostile column names ---

/// A column name is data from the source (DESCRIBE) or the skill page, and it is spliced into SQL as
/// a quoted identifier: a `"` in it must be doubled, or reading the row is a syntax error (and a name
/// built for it is an injection).
const HOSTILE_COLUMNS: &str =
    r#"SELECT 1 AS k, 'v1' AS "we""ird", 'v2' AS "a"" ; DROP VIEW x; --""#;

#[tokio::test]
async fn a_column_with_a_double_quote_in_its_name_does_not_break_reading_a_row() {
    let r = rig(HOSTILE_COLUMNS).await;
    let listed = call(&r.p, "list_instances", json!({ "skill": "thing" })).await;
    assert_eq!(
        listed["result"]["isError"],
        json!(false),
        "listing survives: {listed}"
    );
    let got = call(
        &r.p,
        "expand",
        json!({ "page_id": "markdown/instances/thing/1.md" }),
    )
    .await;
    assert_eq!(
        got["result"]["isError"],
        json!(false),
        "expanding the row survives a hostile column name: {got}"
    );
    assert!(
        got["result"]["structuredContent"]["page"].is_object(),
        "the row is there: {got}"
    );
    assert!(
        got.to_string().contains("v1"),
        "the odd column's value is read back: {got}"
    );
}

#[tokio::test]
async fn a_searchable_column_with_a_double_quote_in_its_name_is_searched_with_a_bound_parameter() {
    let r = rig_full(
        r#"SELECT 1 AS k, 'needle-one' AS "we""ird" UNION ALL SELECT 2, 'other'"#,
        None,
        "  searchable: ['we\"ird']\n",
    )
    .await;
    let found = call(
        &r.p,
        "search",
        json!({ "q": "needle", "page_kind": "instance", "k": 10 }),
    )
    .await;
    let hits = found["result"]["structuredContent"]["hits"]
        .as_array()
        .unwrap_or_else(|| panic!("search answered: {found}"));
    let ids: Vec<&str> = hits.iter().filter_map(|h| h["page_id"].as_str()).collect();
    assert_eq!(ids, ["markdown/instances/thing/1.md"], "{found}");
}
