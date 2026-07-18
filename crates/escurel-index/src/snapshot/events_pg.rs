//! Events re-homing (DuckLake program, PR 9 — Phase B).
//!
//! Mirrors [`super::chat_pg`] (DuckLake PR 8) exactly, applied to the
//! `events` table instead of `chat_messages`: `capture_event` /
//! `assign_event` / `list_events` / `list_inbox` have, until this PR,
//! lived ONLY in the per-tenant local DuckDB file — a ducklake reader
//! has no write surface to append into and no way to read it, so those
//! four tools are on `escurel-server`'s `UNSUPPORTED_ON_REPLICA_TOOLS`
//! gate.
//!
//! This module gives events a durable home every replica (writer AND
//! every reader) can read and write directly: a plain, WRITABLE
//! Postgres table in the SAME Cloud SQL database the DuckLake catalog
//! already lives in (reusing [`super::LakeConfig::catalog_dsn`]),
//! attached via `ATTACH … (TYPE postgres)` under its OWN alias
//! ([`EVENTS_PG_ALIAS`]), separate from [`super::CHAT_PG_ALIAS`]. A
//! second alias attaching the identical DSN is cheap (DuckDB's Postgres
//! connector pools per-alias, not per-table) and keeps this PR from
//! touching PR 8's already-merged `chat_pg` module at all — the
//! less-invasive option explicitly allowed by the PR 9 brief.
//!
//! `provenance` is stored as `VARCHAR` (JSON text), not DuckDB's native
//! `JSON` type — mirroring `chat_pg`'s `metadata` column, which made the
//! same substitution for the same reason (untested JSON round-tripping
//! through the DuckDB Postgres connector).

use duckdb::Connection;

use super::SnapshotError;
use crate::backend::is_safe_sql_fragment;
use crate::events::EVENTS_PG_TABLE_NAME;

/// Fixed ATTACH alias for the events Postgres connection. Not
/// caller-configurable, like [`super::CHAT_PG_ALIAS`].
pub const EVENTS_PG_ALIAS: &str = "events_pg";

/// The `INSTALL`/`LOAD postgres` + `ATTACH IF NOT EXISTS … (TYPE
/// postgres)` statement. Read-write, like `chat_pg`'s attach — every
/// replica of this deployment needs to both capture and assign events.
/// Splice-guarded like every other spliced DSN in this crate
/// (`is_safe_sql_fragment`).
pub fn attach_events_pg_sql(catalog_dsn: &str) -> Result<String, SnapshotError> {
    if catalog_dsn.is_empty() {
        return Err(SnapshotError::InvalidLakeConfig(
            "events catalog_dsn is empty".to_owned(),
        ));
    }
    if !is_safe_sql_fragment(catalog_dsn) {
        return Err(SnapshotError::InvalidLakeConfig(
            "events catalog_dsn contains a splice-unsafe character".to_owned(),
        ));
    }
    Ok(format!(
        "ATTACH IF NOT EXISTS '{catalog_dsn}' AS {EVENTS_PG_ALIAS} (TYPE postgres);"
    ))
}

/// `CREATE TABLE IF NOT EXISTS` for the shared events table. Mirrors
/// `sql/0004_events.sql`'s columns; adds a `tenant` column (the local
/// table has none — tenancy there is implicit in "one DuckDB file per
/// tenant" — but this table is a single physical Postgres relation
/// every replica of this deployment shares, so rows are scoped
/// explicitly). `event_id` (a ULID, globally unique) stays the row key;
/// no separate `created_at` default is needed beyond what the local
/// schema already carries.
pub fn create_events_pg_table_sql() -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS {EVENTS_PG_ALIAS}.{EVENTS_PG_TABLE_NAME} (\
            tenant            VARCHAR    NOT NULL, \
            event_id          VARCHAR    NOT NULL PRIMARY KEY, \
            at_ts             TIMESTAMP, \
            source            VARCHAR    NOT NULL DEFAULT '', \
            mime              VARCHAR    NOT NULL DEFAULT '', \
            label_skill       VARCHAR    NOT NULL DEFAULT '', \
            instance_page_id  VARCHAR, \
            status            VARCHAR    NOT NULL DEFAULT 'inbox', \
            title             VARCHAR    NOT NULL DEFAULT '', \
            body              VARCHAR    NOT NULL DEFAULT '', \
            provenance        VARCHAR, \
            created_at        TIMESTAMP  NOT NULL DEFAULT now()\
        );"
    )
}

/// Run the attach + idempotent table creation on `conn`. Idempotent like
/// [`super::attach_chat_pg`] — `ATTACH IF NOT EXISTS` / `CREATE TABLE IF
/// NOT EXISTS` make a re-run against an already-attached connection a
/// no-op.
pub fn attach_events_pg(conn: &Connection, catalog_dsn: &str) -> Result<(), SnapshotError> {
    conn.execute_batch("INSTALL postgres; LOAD postgres;")?;
    conn.execute_batch(&attach_events_pg_sql(catalog_dsn)?)?;
    conn.execute_batch(&create_events_pg_table_sql())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attach_sql_is_read_write_and_named_events_pg() {
        let sql = attach_events_pg_sql("host=h user=u").unwrap();
        assert!(sql.contains("ATTACH IF NOT EXISTS 'host=h user=u' AS events_pg"));
        assert!(sql.contains("TYPE postgres"));
        assert!(
            !sql.contains("READ_ONLY"),
            "events attach must be read-write"
        );
    }

    #[test]
    fn attach_sql_rejects_unsafe_dsn() {
        let err = attach_events_pg_sql("x'; DROP TABLE events_pg.escurel_events; --");
        assert!(matches!(err, Err(SnapshotError::InvalidLakeConfig(_))));
    }

    #[test]
    fn attach_sql_rejects_empty_dsn() {
        assert!(matches!(
            attach_events_pg_sql(""),
            Err(SnapshotError::InvalidLakeConfig(_))
        ));
    }

    #[test]
    fn create_table_sql_scopes_by_tenant() {
        let sql = create_events_pg_table_sql();
        assert!(sql.contains("tenant            VARCHAR    NOT NULL"));
        assert!(sql.contains(EVENTS_PG_TABLE_NAME));
    }
}
