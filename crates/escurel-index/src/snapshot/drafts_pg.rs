//! Drafts re-homing — the shared home for held writes.
//!
//! Mirrors [`super::events_pg`] exactly, applied to the `drafts` table.
//! The reason is the same one, only sharper: `heron-escurel` runs with
//! `persistence: enabled: false`, so the local per-tenant DuckDB file is
//! recreated on every rollout. A draft is a queue entry a human has not
//! answered yet — a review queue that empties itself on deploy is worse
//! than no review queue, because nothing reports the loss.
//!
//! No PK migration counterpart to [`super::migrate_events_pg_pk_sql`] is
//! needed here: this table has never been deployed, so it is created
//! composite (`(tenant, draft_id)`) from the first boot. The composite
//! key is kept anyway — `draft_id` is a server-minted ULID and unique on
//! its own, but scoping every shared row by tenant is the invariant this
//! crate holds everywhere, not an artefact of who mints the id.

use duckdb::Connection;

use super::SnapshotError;
use crate::backend::is_safe_sql_fragment;
use crate::drafts::DRAFTS_PG_TABLE_NAME;

/// Fixed ATTACH alias for the drafts Postgres connection. Not
/// caller-configurable, like [`super::EVENTS_PG_ALIAS`].
pub const DRAFTS_PG_ALIAS: &str = "drafts_pg";

/// The `ATTACH IF NOT EXISTS … (TYPE postgres)` statement, read-write.
/// Splice-guarded like every other spliced DSN in this crate.
///
/// # Errors
/// When the DSN is empty or contains a splice-unsafe character.
pub fn attach_drafts_pg_sql(catalog_dsn: &str) -> Result<String, SnapshotError> {
    if catalog_dsn.is_empty() {
        return Err(SnapshotError::InvalidLakeConfig(
            "drafts catalog_dsn is empty".to_owned(),
        ));
    }
    if !is_safe_sql_fragment(catalog_dsn) {
        return Err(SnapshotError::InvalidLakeConfig(
            "drafts catalog_dsn contains a splice-unsafe character".to_owned(),
        ));
    }
    Ok(format!(
        "ATTACH IF NOT EXISTS '{catalog_dsn}' AS {DRAFTS_PG_ALIAS} (TYPE postgres);"
    ))
}

/// `CREATE TABLE IF NOT EXISTS` for the shared drafts table. Mirrors
/// `sql/0012_drafts.sql`'s columns and adds the explicit `tenant`
/// column every shared table in this crate carries.
#[must_use]
pub fn create_drafts_pg_table_sql() -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS {DRAFTS_PG_ALIAS}.{DRAFTS_PG_TABLE_NAME} (\
            tenant          VARCHAR   NOT NULL, \
            draft_id        VARCHAR   NOT NULL, \
            target_page_id  VARCHAR   NOT NULL, \
            content         VARCHAR   NOT NULL, \
            content_sha256  VARCHAR   NOT NULL, \
            base_sha256     VARCHAR, \
            author          VARCHAR   NOT NULL DEFAULT '', \
            event_id        VARCHAR, \
            status          VARCHAR   NOT NULL DEFAULT 'open', \
            reason          VARCHAR   NOT NULL DEFAULT '', \
            decided_by      VARCHAR   NOT NULL DEFAULT '', \
            created_at      TIMESTAMP NOT NULL DEFAULT now(), \
            decided_at      TIMESTAMP, \
            PRIMARY KEY (tenant, draft_id)\
        );"
    )
}

/// Run the attach + idempotent table creation on `conn`.
///
/// # Errors
/// When the DSN is unusable or the statements fail.
pub fn attach_drafts_pg(conn: &Connection, catalog_dsn: &str) -> Result<(), SnapshotError> {
    conn.execute_batch("INSTALL postgres; LOAD postgres;")?;
    conn.execute_batch(&attach_drafts_pg_sql(catalog_dsn)?)?;
    conn.execute_batch(&create_drafts_pg_table_sql())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attach_sql_is_read_write_and_named_drafts_pg() {
        let sql = attach_drafts_pg_sql("host=h user=u").unwrap();
        assert!(sql.contains("ATTACH IF NOT EXISTS 'host=h user=u' AS drafts_pg"));
        assert!(sql.contains("(TYPE postgres)"));
        // Positive control for the negative below: a plain DSN passes.
        assert!(!sql.contains("READ_ONLY"));
    }

    #[test]
    fn splice_unsafe_dsn_is_refused() {
        assert!(attach_drafts_pg_sql("x'; DROP TABLE drafts_pg.escurel_drafts; --").is_err());
        assert!(attach_drafts_pg_sql("").is_err());
    }

    #[test]
    fn table_is_tenant_scoped_and_carries_the_approval_hash() {
        let sql = create_drafts_pg_table_sql();
        assert!(sql.contains("PRIMARY KEY (tenant, draft_id)"));
        // The byte binding an approval is made against must exist and be
        // NOT NULL — an approval against a nullable hash is decoration.
        assert!(sql.contains("content_sha256  VARCHAR   NOT NULL"));
        // Alias, not the local table name: a Local-only drafts table is
        // exactly the loss this module exists to prevent.
        assert!(sql.contains("drafts_pg.escurel_drafts"));
    }
}
