//! DuckDB schema migrator.
//!
//! Applies the v1 schema to a fresh per-tenant DuckDB file: pages,
//! links, blocks (with HNSW + FTS indexes), crdt_ops,
//! crdt_snapshots. See `docs/spec/storage.md §DuckDB
//! schema` for the canonical reference.

use duckdb::Connection;
use thiserror::Error;

/// Errors returned by [`Migrator`].
#[derive(Debug, Error)]
pub enum MigrationError {
    #[error("duckdb error: {0}")]
    Duckdb(#[from] duckdb::Error),
}

/// Applies the v1 schema to a fresh DuckDB connection.
///
/// The v1 schema is one-shot: there is no migration framework yet,
/// so running [`Migrator::up`] on an already-initialised database
/// errors with a "table exists" message from DuckDB. Future PRs
/// will add a `schema_version` table and incremental migrations.
#[derive(Debug)]
pub struct Migrator;

impl Migrator {
    /// Logical schema version of the per-tenant DuckDB layout. No
    /// migration-tracking table exists yet; this constant is the manual
    /// stand-in. **Bump it whenever the schema changes** (a new
    /// `sql/000N_*` migration). The offline batch loader records it in its
    /// artifact manifest and a DuckDB→DuckDB transfer refuses an artifact
    /// whose `SCHEMA_VERSION` differs from the live tenant's (the row shapes
    /// wouldn't line up).
    pub const SCHEMA_VERSION: u32 = 7;

    /// Load the per-connection extension/session state Escurel relies on:
    /// auto-install/-load plus `INSTALL`+`LOAD` of `vss`+`fts`
    /// (see `sql/0001_a_autoload.sql`).
    ///
    /// This is **per-connection session state**, not durable schema — `LOAD`
    /// and `SET` apply only to the connection that ran them. It MUST run on
    /// **every** connection that touches the index, on every boot — not just
    /// when the DuckDB file is fresh. A connection opened against an existing
    /// DB that skips this cannot modify the HNSW-indexed `blocks` table
    /// (`Cannot bind index 'blocks', unknown index type 'HNSW'`). `INSTALL` is
    /// idempotent (a no-op once the binary is on disk / baked in the image).
    ///
    /// NOTE: this no longer sets the experimental-HNSW-persistence flag.
    /// Connections over a **persistent (file-backed)** database must ALSO call
    /// [`Migrator::enable_hnsw_persistence`] — see that method for why the
    /// flag is opt-in per connection.
    pub fn load_extensions(conn: &Connection) -> Result<(), MigrationError> {
        conn.execute_batch(STAGE_1_AUTOLOAD)?;
        Ok(())
    }

    /// Enable experimental HNSW persistence on this connection.
    ///
    /// HNSW persistence is gated behind an "experimental" flag in the vss
    /// extension. The Escurel storage spec (storage.md §HNSW persistence
    /// model) relies on the on-disk HNSW index being loaded as-is on
    /// `DuckDB.Open()` and rolled back atomically on mid-write SIGKILL, so
    /// persistent HNSW is mandatory for the single-file backend. See
    /// docs/notes/discovered/2026-05-24-vss-hnsw-experimental-persistence.md.
    ///
    /// Like [`Migrator::load_extensions`] this is **per-connection session
    /// state**: call it on every connection that reads or writes a
    /// file-backed index (the single-file boot path, the offline loader, the
    /// eval harness, …). It is deliberately SEPARATE from `load_extensions`
    /// so snapshot-style backends can load vss/fts without opting into
    /// experimental persistence (DuckLake `IndexStore` seam). `vss` must be
    /// loaded first — the flag belongs to the extension.
    pub fn enable_hnsw_persistence(conn: &Connection) -> Result<(), MigrationError> {
        conn.execute_batch("SET hnsw_enable_experimental_persistence = true;")?;
        Ok(())
    }

    /// Ensure the `group_members` table (group ACL v1) exists. Idempotent
    /// (`CREATE TABLE IF NOT EXISTS`) and run on EVERY connection — unlike
    /// [`Migrator::up`], which only runs against a fresh DB. The v1 schema
    /// has no version framework, so this is how an already-provisioned
    /// tenant DB gains the table on the next boot. Safe to call alongside
    /// [`Migrator::up`] (the fresh path) — the `IF NOT EXISTS` makes the
    /// second call a no-op.
    pub fn ensure_group_members(conn: &Connection) -> Result<(), MigrationError> {
        conn.execute_batch(STAGE_7_GROUP_MEMBERS)?;
        Ok(())
    }

    /// Ensure the `external_credentials` table (SQL-view backend) exists.
    /// Idempotent (`CREATE TABLE IF NOT EXISTS`) and run on EVERY connection
    /// like [`Migrator::ensure_group_members`]. This is a SEPARATE canonical
    /// input (not derivable from `pages/`), so `rebuild` must NOT drop it.
    pub fn ensure_external_credentials(conn: &Connection) -> Result<(), MigrationError> {
        conn.execute_batch(STAGE_8_EXTERNAL_CREDENTIALS)?;
        Ok(())
    }

    /// Ensure the `external_endpoints` table (remote openapi/mcp backends)
    /// exists. Idempotent (`CREATE TABLE IF NOT EXISTS`) and run on EVERY
    /// connection like [`Migrator::ensure_external_credentials`]. A SEPARATE
    /// canonical input (not derivable from `pages/`), so `rebuild` must NOT
    /// drop it.
    pub fn ensure_external_endpoints(conn: &Connection) -> Result<(), MigrationError> {
        conn.execute_batch(STAGE_10_EXTERNAL_ENDPOINTS)?;
        Ok(())
    }

    /// Ensure the `pack_subscriptions` table (skill-pack pins, REQ-SUB-01)
    /// exists. Idempotent (`CREATE TABLE IF NOT EXISTS`) and run on EVERY
    /// connection like [`Migrator::ensure_external_credentials`]. A SEPARATE
    /// canonical input (not derivable from `pages/`), so `rebuild` must NOT
    /// drop it.
    pub fn ensure_pack_subscriptions(conn: &Connection) -> Result<(), MigrationError> {
        conn.execute_batch(STAGE_11_PACK_SUBSCRIPTIONS)?;
        Ok(())
    }

    /// Ensure the `blocks.context` column (Contextual Retrieval, GH #216)
    /// exists. Idempotent (`ADD COLUMN IF NOT EXISTS`) and run on EVERY
    /// connection like [`Migrator::ensure_group_members`], so a tenant DB
    /// provisioned before the column existed gains it on the next boot —
    /// required before [`crate::Indexer::refresh_fts`] rebuilds the FTS
    /// index over (`body`, `context`).
    pub fn ensure_block_context(conn: &Connection) -> Result<(), MigrationError> {
        conn.execute_batch(STAGE_9_BLOCK_CONTEXT)?;
        Ok(())
    }

    /// Apply the v1 schema. Connection should be a fresh DuckDB.
    pub fn up(conn: &Connection) -> Result<(), MigrationError> {
        // The migration is split into staged batches because the
        // `fts` extension's `PRAGMA create_fts_index` resolves the
        // target table in a fresh context — it cannot see DDL from
        // the same `execute_batch` call. Splitting forces an
        // intermediate commit so the catalog is visible when the
        // PRAGMA looks it up. See
        // docs/notes/discovered/2026-05-24-fts-pragma-batch-isolation.md.
        //
        // Stage 4 (`chat_messages`) is the M-Chat append-friendly
        // log for per-chat-group conversation history
        // (DataZooDE/escurel#63). Lives in its own batch so future
        // chat-only schema bumps can be added without disturbing
        // the core tables.
        conn.execute_batch(STAGE_1_AUTOLOAD)?;
        // Stage 2 creates the HNSW index on `blocks`; on a file-backed DB
        // that DDL requires the experimental-persistence flag on THIS
        // connection (the flag used to live in stage 1 — it moved out so
        // `load_extensions` alone no longer implies persistent HNSW).
        Self::enable_hnsw_persistence(conn)?;
        conn.execute_batch(STAGE_2_TABLES_AND_INDEXES)?;
        // `blocks.context` (GH #216) must exist BEFORE stage 3 builds the
        // FTS index over ('body', 'context').
        conn.execute_batch(STAGE_9_BLOCK_CONTEXT)?;
        conn.execute_batch(STAGE_3_FTS)?;
        conn.execute_batch(STAGE_4_CHAT_MESSAGES)?;
        conn.execute_batch(STAGE_5_SCENARIOS)?;
        conn.execute_batch(STAGE_6_EVENTS)?;
        // Group ACL v1. Idempotent (`IF NOT EXISTS`) and ALSO run on every
        // reopen via `ensure_group_members`, so a DB provisioned before
        // this table existed still gains it. Running it here too means a
        // freshly-migrated connection can use it immediately.
        conn.execute_batch(STAGE_7_GROUP_MEMBERS)?;
        // SQL-view credential registry. Idempotent + also run on every reopen
        // via `ensure_external_credentials`, so a DB provisioned before this
        // table existed still gains it.
        conn.execute_batch(STAGE_8_EXTERNAL_CREDENTIALS)?;
        // Remote-backend endpoint registry (openapi/mcp). Idempotent + also
        // run on every reopen via `ensure_external_endpoints`.
        conn.execute_batch(STAGE_10_EXTERNAL_ENDPOINTS)?;
        // Skill-pack subscription pins. Idempotent + also run on every
        // reopen via `ensure_pack_subscriptions`.
        conn.execute_batch(STAGE_11_PACK_SUBSCRIPTIONS)?;
        Ok(())
    }
}

const STAGE_1_AUTOLOAD: &str = include_str!("../sql/0001_a_autoload.sql");
const STAGE_2_TABLES_AND_INDEXES: &str = include_str!("../sql/0001_b_tables.sql");
const STAGE_3_FTS: &str = include_str!("../sql/0001_c_fts.sql");
const STAGE_4_CHAT_MESSAGES: &str = include_str!("../sql/0002_chat_messages.sql");
const STAGE_5_SCENARIOS: &str = include_str!("../sql/0003_scenarios.sql");
const STAGE_6_EVENTS: &str = include_str!("../sql/0004_events.sql");
const STAGE_7_GROUP_MEMBERS: &str = include_str!("../sql/0005_group_members.sql");
const STAGE_8_EXTERNAL_CREDENTIALS: &str = include_str!("../sql/0006_external_credentials.sql");
const STAGE_9_BLOCK_CONTEXT: &str = include_str!("../sql/0007_block_context.sql");
const STAGE_10_EXTERNAL_ENDPOINTS: &str = include_str!("../sql/0008_external_endpoints.sql");
const STAGE_11_PACK_SUBSCRIPTIONS: &str = include_str!("../sql/0009_pack_subscriptions.sql");

#[cfg(test)]
mod tests {
    use super::*;

    // Regression (DataZooDE/escurel): a restart against an EXISTING db is
    // non-fresh, so the server skips `up` and only runs `load_extensions`.
    // That connection must still be able to MODIFY the HNSW-indexed `blocks`
    // table. Before the fix only `up` (fresh-only) loaded `vss`, so a reopened
    // write connection failed with "unknown index type 'HNSW'".
    #[test]
    fn modifies_hnsw_blocks_after_reopen_with_only_load_extensions() {
        let path = std::env::temp_dir().join(format!(
            "escurel-vss-regression-{}.duckdb",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("duckdb.wal"));

        let zeros = std::iter::repeat_n("0.0", 768)
            .collect::<Vec<_>>()
            .join(",");
        let insert = |conn: &Connection, id: &str| {
            conn.execute_batch(&format!(
                "INSERT INTO blocks (block_id, page_id, body, dense_vec) \
                 VALUES ('{id}', 'p1', 'hello world', [{zeros}]::FLOAT[768]);"
            ))
        };

        // Fresh boot: full migrate creates `blocks` + the HNSW index.
        {
            let conn = Connection::open(&path).expect("open fresh");
            Migrator::up(&conn).expect("fresh migrate");
            insert(&conn, "b-fresh").expect("write on the freshly-migrated connection");
        }

        // Restart: the DB already exists, so production skips `up` and only
        // runs `load_extensions` + `enable_hnsw_persistence` (the single-file
        // reopen recipe). Writing the HNSW table must still succeed.
        let conn = Connection::open(&path).expect("reopen");
        Migrator::load_extensions(&conn).expect("load_extensions on reopen");
        Migrator::enable_hnsw_persistence(&conn).expect("enable_hnsw_persistence on reopen");
        insert(&conn, "b-reopen")
            .expect("modifying the HNSW-indexed blocks table after reopen must succeed");

        drop(conn);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("duckdb.wal"));
    }
}
