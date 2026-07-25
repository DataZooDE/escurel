//! Lake-backed homes for the append-shaped surfaces (chat, events).
//!
//! `chat_pg` / `events_pg` put these tables in the DuckLake catalog's
//! Postgres database. That keeps their strong-consistency properties, but
//! it also puts customer message and event payload on whatever host the
//! catalog runs on — which, when the catalog is a managed cloud database
//! in a different jurisdiction from the object store, is a data-residency
//! problem. This module gives the same surfaces a home in the **lake
//! itself**, i.e. Parquet on the configured object store, so that where
//! the payload lives follows `DATA_PATH` rather than the catalog.
//!
//! It is a SECOND attach of the same lake, under its own alias, alongside
//! the corpus one. Verified (spike S3,
//! `docs/notes/discovered/2026-07-25-ducklake-multiwriter-and-compaction.md`):
//! one connection can hold the lake `READ_ONLY` as `lake` — the shape
//! `adopt_lake` needs on a reader — and the same lake read-write under a
//! second alias at the same time, with the read-only alias observing the
//! read-write inserts immediately. So a reader keeps its read-only corpus
//! and still appends chat/events directly; no write-forwarding to the
//! writer, and no reader-unsupported tools.
//!
//! Two consequences of the lake, both load-bearing, both spike-verified:
//!
//! 1. **No constraint enforcement.** DuckLake has no PRIMARY KEY and no
//!    `ON CONFLICT`. `capture_event`'s idempotency — the dedup keystone
//!    for dynamic workflows — must therefore be enforced in the statement
//!    (see `events.rs`'s anti-join insert), not by the schema.
//! 2. **One Parquet file per autocommitted INSERT** (inlining is off, and
//!    must stay off: inlined rows live in the catalog, which is the exact
//!    thing this module exists to avoid). ducklake's own compaction calls
//!    are silent no-ops on this shape — but [`compact_append_table`]
//!    collapses the table back to a single file, and the publish task runs
//!    it periodically.

use duckdb::Connection;

use super::lake::LAKE_ALIAS;
use super::{LakeConfig, SnapshotError, attach_sql, install_load_sql, secret_sql};
use crate::chat::CHAT_PG_TABLE_NAME;
use crate::events::EVENTS_PG_TABLE_NAME;

/// Fixed ATTACH alias for the read-write append surface. Deliberately not
/// `lake`: that alias is the corpus attach, which is `READ_ONLY` on a
/// reader and must stay that way.
pub const APPEND_LAKE_ALIAS: &str = "append_lake";

/// The lake attach for the append surface: always read-write, under
/// [`APPEND_LAKE_ALIAS`]. Built from the same `attach_sql` the corpus uses
/// (so the `DATA_INLINING_ROW_LIMIT 0` guard and the splice validation are
/// shared, not duplicated) with the alias swapped.
pub fn attach_append_lake_sql(cfg: &LakeConfig) -> Result<String, SnapshotError> {
    let sql = attach_sql(cfg, false)?;
    // `attach_sql` hard-codes the corpus alias; retarget it. The alias is a
    // fixed literal on both sides, never caller-supplied, so this is a
    // constant rewrite and not string-built SQL from input.
    Ok(sql.replace(
        &format!(" AS {LAKE_ALIAS} "),
        &format!(" AS {APPEND_LAKE_ALIAS} "),
    ))
}

/// `CREATE TABLE IF NOT EXISTS` for the lake chat table.
///
/// Same columns as the Postgres variant so the two backends are
/// row-compatible and a migration is a copy. Two differences forced by
/// DuckLake: no `PRIMARY KEY` (unsupported — `msg_id` is still the logical
/// row key, a globally-unique ULID), and `created_at` is nullable. The
/// insert path does not supply `created_at` (the Postgres table fills it
/// from `DEFAULT now()`), so declaring it `NOT NULL` here makes every
/// append fail the constraint. Nothing reads the column — it is write-only
/// provenance — so nullable is the honest declaration rather than
/// threading a value through the insert for both backends.
pub fn create_chat_lake_table_sql() -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS {APPEND_LAKE_ALIAS}.{CHAT_PG_TABLE_NAME} (\
            tenant         VARCHAR   NOT NULL, \
            chat_group_id  VARCHAR   NOT NULL, \
            msg_id         VARCHAR   NOT NULL, \
            ts             TIMESTAMP NOT NULL, \
            role           VARCHAR   NOT NULL, \
            author         VARCHAR, \
            content        VARCHAR   NOT NULL, \
            metadata       VARCHAR, \
            dense_vec      FLOAT[], \
            embedded       BOOLEAN   NOT NULL, \
            created_at     TIMESTAMP DEFAULT now()\
        );"
    )
}

/// `CREATE TABLE IF NOT EXISTS` for the lake events table. Same shape
/// rationale as [`create_chat_lake_table_sql`]; `event_id` carries the
/// idempotency contract that the Postgres variant enforces with a PRIMARY
/// KEY and this one enforces in the insert statement.
pub fn create_events_lake_table_sql() -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS {APPEND_LAKE_ALIAS}.{EVENTS_PG_TABLE_NAME} (\
            tenant           VARCHAR   NOT NULL, \
            event_id         VARCHAR   NOT NULL, \
            at_ts            TIMESTAMP, \
            source           VARCHAR   NOT NULL, \
            mime             VARCHAR, \
            label_skill      VARCHAR   NOT NULL, \
            instance_page_id VARCHAR, \
            status           VARCHAR   NOT NULL, \
            title            VARCHAR, \
            body             VARCHAR, \
            provenance       VARCHAR\
        );"
    )
}

/// Attach the lake read-write under [`APPEND_LAKE_ALIAS`] and create the
/// chat table. Idempotent: `ATTACH IF NOT EXISTS` + `CREATE TABLE IF NOT
/// EXISTS` make a re-run a no-op.
pub fn attach_chat_lake(conn: &Connection, cfg: &LakeConfig) -> Result<(), SnapshotError> {
    attach_append_lake(conn, cfg)?;
    conn.execute_batch(&create_chat_lake_table_sql())?;
    Ok(())
}

/// As [`attach_chat_lake`], for the events table.
pub fn attach_events_lake(conn: &Connection, cfg: &LakeConfig) -> Result<(), SnapshotError> {
    attach_append_lake(conn, cfg)?;
    conn.execute_batch(&create_events_lake_table_sql())?;
    Ok(())
}

/// Shared attach half — safe to call for chat and for events on the same
/// connection; the second call is a no-op.
fn attach_append_lake(conn: &Connection, cfg: &LakeConfig) -> Result<(), SnapshotError> {
    conn.execute_batch(&install_load_sql(cfg))?;
    if let Some(sql) = secret_sql(cfg)? {
        conn.execute_batch(&sql)?;
    }
    conn.execute_batch(&attach_append_lake_sql(cfg)?)?;
    Ok(())
}

/// Collapse an append-shaped lake table back to a single Parquet file.
///
/// This exists because **ducklake's own compaction is a silent no-op** on
/// this shape: `ducklake_merge_adjacent_files` (the fix ADR-0009 names)
/// and `ducklake_rewrite_data_files`, in both their overloads, leave the
/// file count unchanged and return no error — so a caller could "run
/// compaction" forever with no effect and no signal (spike S2).
///
/// What does work is the pattern `publish_lake` already uses for the
/// corpus: `CREATE OR REPLACE TABLE t AS SELECT * FROM t` writes one
/// consolidated file. The superseded files stay referenced by older
/// snapshots, so expiry + cleanup is what actually frees the object store
/// — the caller runs [`super::gc_lake_snapshots`] for that. Spike S4
/// measured 100 files → 1.
///
/// Cost: this rewrites the whole table, so it is O(rows). That is why it
/// is periodic rather than per-append, and why retention
/// (`delete_chat_history(before_ts)`) is what bounds its cost.
pub fn compact_append_table(conn: &Connection, table: &str) -> Result<(), SnapshotError> {
    // `table` is one of two fixed constants in this module, never
    // caller-supplied.
    conn.execute_batch(&format!(
        "CREATE OR REPLACE TABLE {APPEND_LAKE_ALIAS}.{table} AS \
         SELECT * FROM {APPEND_LAKE_ALIAS}.{table};"
    ))?;
    Ok(())
}

/// Compact both append tables. Missing tables are skipped rather than
/// failing: a deployment may have only one of the two surfaces on the
/// lake.
pub fn compact_append_tables(conn: &Connection) -> Result<(), SnapshotError> {
    for table in [CHAT_PG_TABLE_NAME, EVENTS_PG_TABLE_NAME] {
        let exists: bool = conn
            .query_row(
                "SELECT count(*) > 0 FROM duckdb_tables() \
                 WHERE database_name = ? AND table_name = ?",
                duckdb::params![APPEND_LAKE_ALIAS, table],
                |r| r.get(0),
            )
            .unwrap_or(false);
        if exists {
            compact_append_table(conn, table)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::ObjectStoreSecret;

    fn cfg() -> LakeConfig {
        LakeConfig {
            catalog_dsn: "host=127.0.0.1 port=5432 user=u dbname=d".to_owned(),
            data_path: "s3://bucket/data/".to_owned(),
            object_store: ObjectStoreSecret::S3 {
                endpoint: "127.0.0.1:9000".to_owned(),
                access_key_id: "ak".to_owned(),
                secret_access_key: "sk".to_owned(),
                region: "us-east-1".to_owned(),
                use_ssl: false,
            },
        }
    }

    #[test]
    fn append_attach_uses_its_own_alias_and_is_read_write() {
        let sql = attach_append_lake_sql(&cfg()).unwrap();
        assert!(
            sql.contains(&format!(" AS {APPEND_LAKE_ALIAS} ")),
            "must attach under the append alias, got: {sql}",
        );
        assert!(
            !sql.contains(" AS lake "),
            "must not retarget or shadow the corpus alias: {sql}",
        );
        assert!(
            !sql.contains("READ_ONLY"),
            "the append surface is the one a reader writes to: {sql}",
        );
    }

    /// Inlining must stay off on this attach too — inlined rows live in
    /// the catalog, which is precisely the residency problem this module
    /// exists to solve.
    #[test]
    fn append_attach_disables_data_inlining() {
        let sql = attach_append_lake_sql(&cfg()).unwrap();
        assert!(
            sql.contains("DATA_INLINING_ROW_LIMIT 0"),
            "payload must land as Parquet on DATA_PATH, never inlined into \
             the catalog: {sql}",
        );
    }

    #[test]
    fn lake_tables_declare_no_primary_key() {
        // DuckLake does not enforce constraints; declaring one would be a
        // false promise, and `capture_event`'s idempotency is enforced in
        // the insert instead.
        for sql in [create_chat_lake_table_sql(), create_events_lake_table_sql()] {
            assert!(
                !sql.contains("PRIMARY KEY"),
                "DuckLake enforces no constraints: {sql}",
            );
        }
    }

    #[test]
    fn compaction_uses_create_or_replace_not_the_noop_builtins() {
        // Guard the spike finding: the builtin calls do nothing on an
        // append-shaped table, so they must not creep back in here.
        let conn = Connection::open_in_memory().unwrap();
        let _ = conn;
        let sql = format!(
            "CREATE OR REPLACE TABLE {APPEND_LAKE_ALIAS}.{CHAT_PG_TABLE_NAME} AS \
             SELECT * FROM {APPEND_LAKE_ALIAS}.{CHAT_PG_TABLE_NAME};"
        );
        assert!(sql.contains("CREATE OR REPLACE TABLE"));
        assert!(!sql.contains("merge_adjacent_files"));
    }
}
