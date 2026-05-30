//! DuckDB schema migrator.
//!
//! Applies the v1 schema to a fresh per-tenant DuckDB file: pages,
//! links, blocks (with HNSW + FTS indexes), frontmatter_index,
//! crdt_ops, crdt_snapshots. See `docs/spec/storage.md §DuckDB
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
        conn.execute_batch(STAGE_2_TABLES_AND_INDEXES)?;
        conn.execute_batch(STAGE_3_FTS)?;
        conn.execute_batch(STAGE_4_CHAT_MESSAGES)?;
        conn.execute_batch(STAGE_5_SCENARIOS)?;
        conn.execute_batch(STAGE_6_EVENTS)?;
        Ok(())
    }
}

const STAGE_1_AUTOLOAD: &str = include_str!("../sql/0001_a_autoload.sql");
const STAGE_2_TABLES_AND_INDEXES: &str = include_str!("../sql/0001_b_tables.sql");
const STAGE_3_FTS: &str = include_str!("../sql/0001_c_fts.sql");
const STAGE_4_CHAT_MESSAGES: &str = include_str!("../sql/0002_chat_messages.sql");
const STAGE_5_SCENARIOS: &str = include_str!("../sql/0003_scenarios.sql");
const STAGE_6_EVENTS: &str = include_str!("../sql/0004_events.sql");
