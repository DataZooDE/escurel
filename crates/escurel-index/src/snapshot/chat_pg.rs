//! Chat re-homing (DuckLake program, PR 8 — Phase B).
//!
//! `chat_messages` (per-chat-group conversation history) has, until this
//! PR, lived ONLY in the per-tenant local DuckDB file — a ducklake reader
//! has no write surface to append into and no way to read it, so
//! `append_message`/`list_messages` are on the [`crate::snapshot::
//! adopt_lake`]-adjacent `UNSUPPORTED_ON_REPLICA_TOOLS` gate in
//! `escurel-server`.
//!
//! This module gives chat a durable home every replica (writer AND every
//! reader) can read and write directly: a plain, WRITABLE Postgres table
//! in the SAME Cloud SQL database the DuckLake catalog already lives in
//! (reusing [`super::LakeConfig::catalog_dsn`] — not a new database, not a
//! new instance), attached via `ATTACH … (TYPE postgres)` — the ordinary
//! DuckDB Postgres connector (same family as the sql_view backend's
//! read-only connector in `backend/sql_view.rs`), NOT the `ducklake:`
//! catalog protocol `lake.rs` uses. Spike 3
//! (docs/notes/discovered/2026-07-17-ducklake-spike-results.md) verified
//! two DuckDB processes concurrently `INSERT`ing into one such table lost
//! zero writes — that is the concurrency model this module relies on.
//!
//! `dense_vec` is stored as `FLOAT[]` (list), not the fixed-width
//! `FLOAT[768]` the local `chat_messages` table uses — mirroring the
//! lake's own `FLOAT[768]`-is-unstorable lesson (spike 1). Callers cast
//! back `::FLOAT[768]` at query time (see
//! [`crate::chat::Indexer::search_chat_messages`]).

use duckdb::Connection;

use super::SnapshotError;
use crate::backend::is_safe_sql_fragment;
use crate::chat::CHAT_PG_TABLE_NAME;

/// Fixed ATTACH alias for the chat Postgres connection. Not
/// caller-configurable, like `lake.rs`'s `LAKE_ALIAS`.
pub const CHAT_PG_ALIAS: &str = "chat_pg";

/// The `INSTALL`/`LOAD postgres` + `ATTACH IF NOT EXISTS … (TYPE
/// postgres)` statement. Read-write (unlike every other cross-tenant
/// Postgres attach in this codebase, which is `READ_ONLY`) — this is the
/// one surface where a reader deliberately gets write access, because
/// chat history is per-user/per-chat-group append-only data, not the
/// shared corpus. Splice-guarded like every other spliced DSN in this
/// crate (`is_safe_sql_fragment`).
pub fn attach_chat_pg_sql(catalog_dsn: &str) -> Result<String, SnapshotError> {
    if catalog_dsn.is_empty() {
        return Err(SnapshotError::InvalidLakeConfig(
            "chat catalog_dsn is empty".to_owned(),
        ));
    }
    if !is_safe_sql_fragment(catalog_dsn) {
        return Err(SnapshotError::InvalidLakeConfig(
            "chat catalog_dsn contains a splice-unsafe character".to_owned(),
        ));
    }
    Ok(format!(
        "ATTACH IF NOT EXISTS '{catalog_dsn}' AS {CHAT_PG_ALIAS} (TYPE postgres);"
    ))
}

/// `CREATE TABLE IF NOT EXISTS` for the shared chat table. Mirrors
/// `sql/0002_chat_messages.sql`'s columns; adds a `tenant` column (the
/// local table has none — tenancy there is implicit in "one DuckDB file
/// per tenant" — but this table is a single physical Postgres relation
/// every replica of this deployment shares, so rows are scoped
/// explicitly). No `PRIMARY KEY` clause naming `ts` — Postgres has no
/// notion of DuckDB's `TIMESTAMP` micro-precision collisions the local
/// schema relies on; `msg_id` (a ULID, globally unique) is the row key.
/// No HNSW — over one chat group's history this is small enough for a
/// brute-force `ORDER BY array_cosine_distance(...)` scan (see
/// [`crate::chat::Indexer::search_chat_messages`]); DuckLake/Postgres
/// attaches don't support the `vss` HNSW index type in any case.
pub fn create_chat_pg_table_sql() -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS {CHAT_PG_ALIAS}.{CHAT_PG_TABLE_NAME} (\
            tenant         VARCHAR    NOT NULL, \
            chat_group_id  VARCHAR    NOT NULL, \
            msg_id         VARCHAR    NOT NULL PRIMARY KEY, \
            ts             TIMESTAMP  NOT NULL, \
            role           VARCHAR    NOT NULL, \
            author         VARCHAR, \
            content        VARCHAR    NOT NULL, \
            metadata       VARCHAR, \
            dense_vec      FLOAT[], \
            embedded       BOOLEAN    NOT NULL DEFAULT TRUE, \
            created_at     TIMESTAMP  NOT NULL DEFAULT now()\
        );"
    )
}

/// Run the attach + idempotent table creation on `conn`. Idempotent like
/// [`super::attach_lake`] — `ATTACH IF NOT EXISTS` / `CREATE TABLE IF NOT
/// EXISTS` make a re-run against an already-attached connection a no-op.
pub fn attach_chat_pg(conn: &Connection, catalog_dsn: &str) -> Result<(), SnapshotError> {
    conn.execute_batch("INSTALL postgres; LOAD postgres;")?;
    conn.execute_batch(&attach_chat_pg_sql(catalog_dsn)?)?;
    conn.execute_batch(&create_chat_pg_table_sql())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attach_sql_is_read_write_and_named_chat_pg() {
        let sql = attach_chat_pg_sql("host=h user=u").unwrap();
        assert!(sql.contains("ATTACH IF NOT EXISTS 'host=h user=u' AS chat_pg"));
        assert!(sql.contains("TYPE postgres"));
        assert!(!sql.contains("READ_ONLY"), "chat attach must be read-write");
    }

    #[test]
    fn attach_sql_rejects_unsafe_dsn() {
        let err = attach_chat_pg_sql("x'; DROP TABLE chat_pg.escurel_chat_messages; --");
        assert!(matches!(err, Err(SnapshotError::InvalidLakeConfig(_))));
    }

    #[test]
    fn attach_sql_rejects_empty_dsn() {
        assert!(matches!(
            attach_chat_pg_sql(""),
            Err(SnapshotError::InvalidLakeConfig(_))
        ));
    }

    #[test]
    fn create_table_sql_uses_list_not_fixed_width_vector() {
        let sql = create_chat_pg_table_sql();
        assert!(sql.contains("dense_vec      FLOAT[]"));
        assert!(!sql.contains("FLOAT[768]"));
        assert!(sql.contains(CHAT_PG_TABLE_NAME));
    }
}
