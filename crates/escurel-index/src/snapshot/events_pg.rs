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
/// explicitly). The row key is `(tenant, event_id)`, NOT `event_id`
/// alone: `event_id` may be client-supplied (the idempotency key), so
/// two tenants can legitimately use the same id — a single-column PK
/// made tenant B's insert `ON CONFLICT DO NOTHING` against tenant A's
/// row and the tenant-scoped readback then found nothing (a silent
/// cross-tenant collision). `capture_event`'s conflict target matches
/// the composite key.
pub fn create_events_pg_table_sql() -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS {EVENTS_PG_ALIAS}.{EVENTS_PG_TABLE_NAME} (\
            tenant            VARCHAR    NOT NULL, \
            event_id          VARCHAR    NOT NULL, \
            at_ts             TIMESTAMP, \
            source            VARCHAR    NOT NULL DEFAULT '', \
            mime              VARCHAR    NOT NULL DEFAULT '', \
            label_skill       VARCHAR    NOT NULL DEFAULT '', \
            instance_page_id  VARCHAR, \
            status            VARCHAR    NOT NULL DEFAULT 'inbox', \
            title             VARCHAR    NOT NULL DEFAULT '', \
            body              VARCHAR    NOT NULL DEFAULT '', \
            provenance        VARCHAR, \
            created_at        TIMESTAMP  NOT NULL DEFAULT now(), \
            PRIMARY KEY (tenant, event_id)\
        );"
    )
}

/// In-place migration of an ALREADY-DEPLOYED events table from the old
/// single-column primary key (`event_id`) to the composite
/// `(tenant, event_id)`.
///
/// `CREATE TABLE IF NOT EXISTS` never alters an existing relation, so a
/// deployment whose table predates the composite key (the lab Cloud SQL
/// has one) would keep colliding across tenants forever without this.
/// Runs as ONE `postgres_execute` `DO` block on the Postgres side:
///
/// - a `pg_advisory_xact_lock` serialises replicas booting at the same
///   moment, so exactly one performs the `ALTER` and the rest see the
///   already-composite key;
/// - the current PK's columns are read from `information_schema`; only
///   the exact old shape (`event_id` alone) is touched — a fresh table
///   (already composite) and a re-run are both no-ops, keeping
///   [`attach_events_pg`] idempotent.
pub fn migrate_events_pg_pk_sql() -> String {
    let pg_sql = format!(
        "DO $escurel_pk_mig$ \
         DECLARE pk_name text; pk_cols text; \
         BEGIN \
           PERFORM pg_advisory_xact_lock(hashtext('escurel_events_pk_migration')); \
           SELECT tc.constraint_name, \
                  string_agg(kcu.column_name, ',' ORDER BY kcu.ordinal_position) \
             INTO pk_name, pk_cols \
             FROM information_schema.table_constraints tc \
             JOIN information_schema.key_column_usage kcu \
               ON kcu.constraint_name = tc.constraint_name \
              AND kcu.table_schema = tc.table_schema \
            WHERE tc.table_name = '{EVENTS_PG_TABLE_NAME}' \
              AND tc.constraint_type = 'PRIMARY KEY' \
            GROUP BY tc.constraint_name; \
           IF pk_cols = 'event_id' THEN \
             EXECUTE format('ALTER TABLE {EVENTS_PG_TABLE_NAME} DROP CONSTRAINT %I', pk_name); \
             ALTER TABLE {EVENTS_PG_TABLE_NAME} ADD PRIMARY KEY (tenant, event_id); \
           END IF; \
         END \
         $escurel_pk_mig$;"
    );
    // The Postgres-side SQL travels as a DuckDB string literal — double
    // its single quotes. Everything above is a compile-time constant
    // (no caller input), so this is encoding, not sanitisation.
    let escaped = pg_sql.replace('\'', "''");
    format!("CALL postgres_execute('{EVENTS_PG_ALIAS}', '{escaped}');")
}

/// Run the attach + idempotent table creation on `conn`. Idempotent like
/// [`super::attach_chat_pg`] — `ATTACH IF NOT EXISTS` / `CREATE TABLE IF
/// NOT EXISTS` make a re-run against an already-attached connection a
/// no-op.
pub fn attach_events_pg(conn: &Connection, catalog_dsn: &str) -> Result<(), SnapshotError> {
    conn.execute_batch("INSTALL postgres; LOAD postgres;")?;
    conn.execute_batch(&attach_events_pg_sql(catalog_dsn)?)?;
    conn.execute_batch(&create_events_pg_table_sql())?;
    // Idempotently upgrade a pre-existing deployed table from the old
    // `event_id`-only PK to `(tenant, event_id)` — `CREATE TABLE IF NOT
    // EXISTS` above is a no-op on an existing relation, so the old shape
    // survives it and must be ALTERed here (see the fn doc).
    conn.execute_batch(&migrate_events_pg_pk_sql())?;
    // The migration ran on the Postgres side, behind DuckDB's back —
    // DuckDB caches an attached table's schema (constraints included)
    // and would keep binding `ON CONFLICT (tenant, event_id)` against
    // the OLD single-column key. Drop the cache so the first insert
    // sees the composite key.
    conn.execute_batch("CALL pg_clear_cache();")?;
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

    /// The row key must be `(tenant, event_id)` — `event_id` is
    /// client-supplied (an idempotency key), so a single-column PK let
    /// tenants collide on it.
    #[test]
    fn create_table_sql_uses_composite_tenant_scoped_pk() {
        let sql = create_events_pg_table_sql();
        assert!(
            sql.contains("PRIMARY KEY (tenant, event_id)"),
            "composite key required: {sql}"
        );
        assert!(
            !sql.contains("event_id          VARCHAR    NOT NULL PRIMARY KEY"),
            "no single-column event_id PK: {sql}"
        );
    }

    /// The migration touches ONLY the exact old shape, under an
    /// advisory lock, through `postgres_execute` on the events alias.
    #[test]
    fn pk_migration_is_guarded_and_targeted() {
        let sql = migrate_events_pg_pk_sql();
        assert!(sql.starts_with(&format!("CALL postgres_execute('{EVENTS_PG_ALIAS}'")));
        assert!(sql.contains("pg_advisory_xact_lock"));
        assert!(sql.contains("IF pk_cols = ''event_id'' THEN"));
        assert!(sql.contains("ADD PRIMARY KEY (tenant, event_id)"));
    }
}
