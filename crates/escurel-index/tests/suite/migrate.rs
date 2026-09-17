//! Integration tests for [`escurel_index::Migrator`].
//!
//! Real DuckDB file in a `tempfile::TempDir`, no mocks. The vss
//! and fts extensions are auto-installed (via DuckDB autoinstall)
//! and explicitly loaded by the migrator — autoload doesn't fire
//! for `USING HNSW` index DDL or `PRAGMA create_fts_index` calls,
//! see `docs/notes/discovered/2026-05-24-duckdb-vss-fts-autoload.md`.
//! Cold runs need egress to `extensions.duckdb.org`; substrate
//! deployments bake the binaries into the golden image (§6).

use std::path::PathBuf;

use duckdb::{Connection, params};
use escurel_index::Migrator;
use tempfile::TempDir;

fn fresh_db() -> (Connection, TempDir) {
    let dir = TempDir::new().expect("tempdir");
    let path: PathBuf = dir.path().join("escurel.duckdb");
    let conn = Connection::open(&path).expect("open duckdb");
    (conn, dir)
}

fn table_exists(conn: &Connection, name: &str) -> bool {
    let count: i64 = conn
        .query_row(
            "SELECT count(*) FROM information_schema.tables \
             WHERE table_schema = 'main' AND table_name = ?",
            params![name],
            |row| row.get(0),
        )
        .expect("count tables");
    count > 0
}

fn column_type(conn: &Connection, table: &str, column: &str) -> String {
    conn.query_row(
        "SELECT data_type FROM information_schema.columns \
         WHERE table_schema = 'main' AND table_name = ? AND column_name = ?",
        params![table, column],
        |row| row.get::<_, String>(0),
    )
    .unwrap_or_else(|err| panic!("column {table}.{column}: {err}"))
}

fn index_exists(conn: &Connection, name: &str) -> bool {
    let count: i64 = conn
        .query_row(
            "SELECT count(*) FROM duckdb_indexes() WHERE index_name = ?",
            params![name],
            |row| row.get(0),
        )
        .expect("count indexes");
    count > 0
}

#[test]
fn up_creates_core_tables() {
    let (conn, _dir) = fresh_db();
    Migrator::up(&conn).expect("schema migration succeeds");

    for table in [
        "pages",
        "links",
        "blocks",
        "crdt_ops",
        "crdt_snapshots",
        "chat_messages",
        "events", // M7 — Event-sourcing surface
    ] {
        assert!(
            table_exists(&conn, table),
            "migration must create the `{table}` table",
        );
    }
}

#[test]
fn pages_has_at_ts_and_skill_columns_for_event_log_scan() {
    let (conn, _dir) = fresh_db();
    Migrator::up(&conn).expect("schema migration succeeds");

    // `at_ts` is the denormalised mirror of `frontmatter.at` for
    // the index-served event-log scan path (spec storage.md).
    let at_ts_type = column_type(&conn, "pages", "at_ts");
    assert!(
        at_ts_type.to_uppercase().contains("TIMESTAMP"),
        "pages.at_ts must be a TIMESTAMP, got: {at_ts_type:?}",
    );

    let skill_type = column_type(&conn, "pages", "skill");
    assert!(
        skill_type.to_uppercase().contains("VARCHAR"),
        "pages.skill must be VARCHAR, got: {skill_type:?}",
    );
}

#[test]
fn blocks_dense_vec_is_768_dim_float_array() {
    let (conn, _dir) = fresh_db();
    Migrator::up(&conn).expect("schema migration succeeds");

    let ty = column_type(&conn, "blocks", "dense_vec");
    let upper = ty.to_uppercase();
    assert!(
        upper.contains("FLOAT") && upper.contains("768"),
        "blocks.dense_vec must be FLOAT[768] (EmbeddingGemma default), \
         got: {ty:?}",
    );
}

#[test]
fn pages_skill_at_composite_index_exists() {
    let (conn, _dir) = fresh_db();
    Migrator::up(&conn).expect("schema migration succeeds");

    assert!(
        index_exists(&conn, "pages_skill_at"),
        "composite (skill, at_ts) index on pages is the event-log \
         scan support per spec storage.md",
    );
}

/// #431: a fresh database carries NO vector index, and nearest-neighbour
/// search works anyway.
///
/// The `vss` HNSW index hangs the writer after ~192 delete+insert cycles and
/// segfaults if rebuilt, and it bought no measurable latency because every
/// search carries a filter. `search_with` ranks by `array_cosine_distance`,
/// which needs no index — so the query below is the real one, unchanged.
#[test]
fn nearest_neighbour_works_and_no_hnsw_index_is_built() {
    let (conn, _dir) = fresh_db();
    Migrator::up(&conn).expect("schema migration succeeds");
    Migrator::ensure_vector_index(&conn).expect("vector-index DDL runs");

    assert!(
        !index_exists(&conn, "hnsw_blocks_vec"),
        "no HNSW index by default (#431) — it is what took the writer down",
    );
    assert!(
        !index_exists(&conn, "hnsw_chat_vec"),
        "the chat index goes the same way, for the same reason",
    );
    // Premise: `index_exists` can see an index that IS there, or the two
    // assertions above would pass against a helper that always says no.
    assert!(
        index_exists(&conn, "blocks_page"),
        "control: the ordinary b-tree index on blocks.page_id is present",
    );

    // Insert two distinct vectors.
    let mut zero = vec![0.0_f32; 768];
    zero[0] = 1.0;
    let mut one = vec![0.0_f32; 768];
    one[1] = 1.0;
    insert_block(&conn, "p1:b1", "p1", "blk-1", "first block", &zero);
    insert_block(&conn, "p1:b2", "p1", "blk-2", "second block", &one);

    // Search for the nearest neighbour of `zero` — must be block 1.
    let zero_literal = format_vector_literal(&zero);
    let sql = format!(
        "SELECT block_id FROM blocks \
         ORDER BY array_cosine_distance(dense_vec, {zero_literal}::FLOAT[768]) ASC \
         LIMIT 1",
    );
    let nearest: String = conn
        .query_row(&sql, [], |row| row.get(0))
        .expect("nearest neighbour query succeeds without an index");
    assert_eq!(nearest, "p1:b1", "nearest neighbour of `zero` is itself");
}

/// …and the flag still builds one, so the day `vss` is fixed we can measure it
/// again rather than reconstruct the DDL from a git log. Driven through
/// `set_vector_index` rather than the env var: `set_var` is `unsafe`, and this
/// workspace forbids `unsafe` — which is also why the env read lives in one
/// place instead of being threaded through every caller.
#[test]
fn the_hnsw_index_can_be_switched_back_on() {
    let (conn, _dir) = fresh_db();
    Migrator::up(&conn).expect("schema migration succeeds");
    Migrator::enable_hnsw_persistence(&conn).expect("experimental persistence");

    Migrator::set_vector_index(&conn, true).expect("the CREATE path still works");

    assert!(
        index_exists(&conn, "hnsw_blocks_vec"),
        "ESCUREL_INDEX_HNSW=on must build the index",
    );

    // And back off again, on the same database — the DDL runs both ways, which
    // is how a tenant provisioned with an index loses it on the next boot.
    Migrator::set_vector_index(&conn, false).expect("the DROP path still works");
    assert!(
        !index_exists(&conn, "hnsw_blocks_vec"),
        "unset must drop the index again, not leave it standing",
    );
}

#[test]
fn fts_index_on_blocks_body_works_end_to_end() {
    // Real test of the fts extension: insert two blocks with
    // different prose, query via the FTS match_bm25 function,
    // expect the matching block on top. Implicitly verifies
    // fts was auto-installed + auto-loaded.
    let (conn, _dir) = fresh_db();
    Migrator::up(&conn).expect("schema migration succeeds");

    insert_block(
        &conn,
        "p1:b1",
        "p1",
        "blk-1",
        "Acme Corp is an industrial manufacturing conglomerate",
        &vec![0.0_f32; 768],
    );
    insert_block(
        &conn,
        "p1:b2",
        "p1",
        "blk-2",
        "Globex is a Stuttgart-based fintech",
        &vec![0.0_f32; 768],
    );

    // The current fts extension has no `refresh_fts_index` PRAGMA
    // (the spec storage.md claim is out-of-date — noted in
    // discovered/). The supported pattern is `overwrite = 1` on
    // create, which rebuilds the index over the now-populated table.
    conn.execute_batch(
        "PRAGMA create_fts_index('blocks', 'block_id', 'body', \
            stemmer = 'porter', stopwords = 'english', \
            ignore = '(\\.|[^a-z])+', lower = 1, overwrite = 1);",
    )
    .expect("rebuild fts via overwrite=1");

    let top: String = conn
        .query_row(
            "SELECT block_id FROM blocks \
             WHERE fts_main_blocks.match_bm25(block_id, 'industrial') IS NOT NULL \
             ORDER BY fts_main_blocks.match_bm25(block_id, 'industrial') DESC \
             LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("fts match_bm25 query succeeds");
    assert_eq!(
        top, "p1:b1",
        "the block containing 'industrial' must rank first"
    );
}

#[test]
fn running_up_twice_returns_error() {
    let (conn, _dir) = fresh_db();
    Migrator::up(&conn).expect("first migration");

    let err = Migrator::up(&conn).expect_err("second migration must fail");
    let msg = format!("{err}");
    // DuckDB phrases the duplicate-table error differently across
    // versions; assert on the substring we know is stable.
    assert!(
        msg.to_lowercase().contains("exist"),
        "second up() must error with an 'already exists' style message, got: {msg}",
    );
}

// --- helpers ----------------------------------------------------

fn insert_block(
    conn: &Connection,
    block_id: &str,
    page_id: &str,
    anchor: &str,
    body: &str,
    dense_vec: &[f32],
) {
    let lit = format_vector_literal(dense_vec);
    let sql = format!(
        "INSERT INTO blocks (block_id, page_id, anchor, ordinal, body, dense_vec) \
         VALUES (?, ?, ?, 0, ?, {lit}::FLOAT[768])",
    );
    conn.execute(&sql, params![block_id, page_id, anchor, body])
        .unwrap_or_else(|err| panic!("insert block {block_id}: {err}"));
}

fn format_vector_literal(v: &[f32]) -> String {
    let mut out = String::with_capacity(v.len() * 8 + 2);
    out.push('[');
    for (i, x) in v.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        // Use Display, not Debug; DuckDB wants `0` not `0.0` is fine
        // either way, but exponent form should be avoided.
        out.push_str(&format!("{x}"));
    }
    out.push(']');
    out
}
