//! CRDT op-log re-homing (DuckLake program, PR 10 — Phase B).
//!
//! Mirrors `escurel-index`'s `snapshot::chat_pg` (PR 8) and
//! `snapshot::events_pg` (PR 9) exactly, applied to `crdt_ops` /
//! `crdt_snapshots` instead: those two tables have, until this PR, lived
//! ONLY in the per-tenant local DuckDB file — a ducklake reader boots
//! with no local write surface and no way to read them, so
//! `open_session` / `apply_op` / `close_session` / `list_snapshots` are on
//! `escurel-server`'s `UNSUPPORTED_ON_REPLICA_TOOLS` gate.
//!
//! This module gives the CRDT layer a durable home every replica (writer
//! AND every reader) can read and write directly: two plain, WRITABLE
//! Postgres tables in the SAME Cloud SQL database the DuckLake catalog
//! already lives in, attached via `ATTACH … (TYPE postgres)` under their
//! own alias ([`CRDT_PG_ALIAS`]) — a third alias attaching the identical
//! DSN alongside `chat_pg` / `events_pg`, cheap for the same reason PR 9's
//! doc comment gives (DuckDB's Postgres connector pools per-alias).
//!
//! **Why this lives in `escurel-crdt`, not `escurel-index/src/snapshot`
//! (unlike `chat_pg`/`events_pg`).** `chat_pg`/`events_pg` live in
//! `escurel-index` because `Indexer` (which owns the chat/events methods)
//! lives there too. The CRDT op log has TWO independent readers/writers
//! of the same two tables:
//!
//! - [`crate::DuckdbCrdtBackend`] (this crate) — the live-session path
//!   (`LiveDoc` / `SessionManager`), which owns its OWN
//!   `Arc<Mutex<Connection>>`, separate from the indexer's.
//! - `escurel_index::Indexer::list_snapshots` / `seed_snapshot_history`
//!   (`crdt_history.rs`) — the historical-read path, which reads `crdt_
//!   snapshots` directly off the INDEXER's own connection, bypassing the
//!   `CrdtBackend` trait entirely.
//!
//! `escurel-index` already depends on `escurel-crdt` (normal dependency,
//! for the `citation.rs` reconciler); the reverse is not true (`escurel-
//! crdt` has no `escurel-index` dependency — that direction would be a
//! cycle). So the ATTACH/CREATE-TABLE SQL and the alias/table-name
//! constants have to live on the `escurel-crdt` side, where both
//! `escurel-crdt` (for `DuckdbCrdtBackend`) and `escurel-index` (for
//! `Indexer::attach_crdt_pg`, re-exporting these) can reach them.
//!
//! `op_bytes` / `snapshot_bytes` stay `BLOB` in the `CREATE TABLE` DDL —
//! unlike PR 8's `dense_vec` (which needed `FLOAT[]` instead of the
//! lake-unstorable `FLOAT[768]`), `BLOB` needs no substitution: verified
//! empirically to map to Postgres `bytea` and round-trip byte-exact
//! through `duckdb::ToSql`/`row.get::<_, Vec<u8>>` (see
//! `docs/notes/discovered/2026-07-18-duckdb-blob-bytea-round-trip.md`).

use duckdb::Connection;

use crate::error::Error;

/// Fixed ATTACH alias for the CRDT Postgres connection. Not
/// caller-configurable, mirroring `CHAT_PG_ALIAS` / `EVENTS_PG_ALIAS`.
pub const CRDT_PG_ALIAS: &str = "crdt_pg";

/// Table name for the shared attached-Postgres op-log table.
pub const CRDT_OPS_PG_TABLE: &str = "escurel_crdt_ops";

/// Table name for the shared attached-Postgres snapshot table.
pub const CRDT_SNAPSHOTS_PG_TABLE: &str = "escurel_crdt_snapshots";

/// Reject characters that could break out of a single-quoted SQL literal
/// or stack a second statement. Duplicated (not imported) from
/// `escurel_index::backend::sql_view::is_safe_sql_fragment` — see the
/// module doc for why `escurel-crdt` cannot depend on `escurel-index`.
/// Kept byte-for-byte identical to the original so the two crates never
/// silently diverge on what counts as splice-safe.
fn is_safe_sql_fragment(s: &str) -> bool {
    !s.chars()
        .any(|c| c == '\'' || c == '"' || c == ';' || c == '`' || c == '\\' || c.is_control())
}

/// The `INSTALL`/`LOAD postgres` + `ATTACH IF NOT EXISTS … (TYPE
/// postgres)` statement. Read-write, like `chat_pg`/`events_pg`'s attach
/// — every replica needs to both append ops and read them back. Splice-
/// guarded like every other spliced DSN in this codebase.
pub fn attach_crdt_pg_sql(catalog_dsn: &str) -> Result<String, Error> {
    if catalog_dsn.is_empty() {
        return Err(Error::InvalidConfig("crdt catalog_dsn is empty".to_owned()));
    }
    if !is_safe_sql_fragment(catalog_dsn) {
        return Err(Error::InvalidConfig(
            "crdt catalog_dsn contains a splice-unsafe character".to_owned(),
        ));
    }
    Ok(format!(
        "ATTACH IF NOT EXISTS '{catalog_dsn}' AS {CRDT_PG_ALIAS} (TYPE postgres);"
    ))
}

/// `CREATE TABLE IF NOT EXISTS` for the shared op-log table. Mirrors
/// `sql/0001_b_tables.sql`'s `crdt_ops` columns; adds a `tenant` column
/// for the same reason `chat_pg`/`events_pg` do (one physical Postgres
/// relation shared by every replica of this deployment). No
/// `crdt_ops_page_hlc` index — DuckDB's Postgres connector attach does
/// not support creating secondary indexes on the remote relation from
/// here; the local table's index remains local-only.
pub fn create_crdt_ops_pg_table_sql() -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS {CRDT_PG_ALIAS}.{CRDT_OPS_PG_TABLE} (\
            tenant        VARCHAR   NOT NULL, \
            page_id       VARCHAR   NOT NULL, \
            op_id         VARCHAR   NOT NULL, \
            hlc           BIGINT    NOT NULL, \
            parent_op_id  VARCHAR, \
            op_bytes      BLOB      NOT NULL, \
            applied_at    TIMESTAMP DEFAULT now(), \
            PRIMARY KEY (tenant, page_id, op_id)\
        );"
    )
}

/// `CREATE TABLE IF NOT EXISTS` for the shared snapshot table. Mirrors
/// `sql/0001_b_tables.sql`'s `crdt_snapshots` columns, `tenant`-scoped
/// like [`create_crdt_ops_pg_table_sql`].
pub fn create_crdt_snapshots_pg_table_sql() -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS {CRDT_PG_ALIAS}.{CRDT_SNAPSHOTS_PG_TABLE} (\
            tenant          VARCHAR   NOT NULL, \
            page_id         VARCHAR   NOT NULL, \
            snapshot_hlc    BIGINT    NOT NULL, \
            snapshot_bytes  BLOB      NOT NULL, \
            taken_at        TIMESTAMP DEFAULT now(), \
            PRIMARY KEY (tenant, page_id, snapshot_hlc)\
        );"
    )
}

/// Run the attach + idempotent table creation (both tables) on `conn`.
/// Idempotent like `chat_pg`/`events_pg`'s attach — `ATTACH IF NOT
/// EXISTS` / `CREATE TABLE IF NOT EXISTS` make a re-run against an
/// already-attached connection a no-op.
pub fn attach_crdt_pg(conn: &Connection, catalog_dsn: &str) -> Result<(), Error> {
    conn.execute_batch("INSTALL postgres; LOAD postgres;")?;
    conn.execute_batch(&attach_crdt_pg_sql(catalog_dsn)?)?;
    conn.execute_batch(&create_crdt_ops_pg_table_sql())?;
    conn.execute_batch(&create_crdt_snapshots_pg_table_sql())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attach_sql_is_read_write_and_named_crdt_pg() {
        let sql = attach_crdt_pg_sql("host=h user=u").unwrap();
        assert!(sql.contains("ATTACH IF NOT EXISTS 'host=h user=u' AS crdt_pg"));
        assert!(sql.contains("TYPE postgres"));
        assert!(!sql.contains("READ_ONLY"), "crdt attach must be read-write");
    }

    #[test]
    fn attach_sql_rejects_unsafe_dsn() {
        let err = attach_crdt_pg_sql("x'; DROP TABLE crdt_pg.escurel_crdt_ops; --");
        assert!(matches!(err, Err(Error::InvalidConfig(_))));
    }

    #[test]
    fn attach_sql_rejects_empty_dsn() {
        assert!(matches!(
            attach_crdt_pg_sql(""),
            Err(Error::InvalidConfig(_))
        ));
    }

    #[test]
    fn create_table_sql_scopes_by_tenant_and_uses_blob() {
        let ops_sql = create_crdt_ops_pg_table_sql();
        assert!(ops_sql.contains("tenant        VARCHAR   NOT NULL"));
        assert!(ops_sql.contains("op_bytes      BLOB      NOT NULL"));
        assert!(ops_sql.contains(CRDT_OPS_PG_TABLE));

        let snap_sql = create_crdt_snapshots_pg_table_sql();
        assert!(snap_sql.contains("tenant          VARCHAR   NOT NULL"));
        assert!(snap_sql.contains("snapshot_bytes  BLOB      NOT NULL"));
        assert!(snap_sql.contains(CRDT_SNAPSHOTS_PG_TABLE));
    }
}
