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
    /// Load the per-connection extension/session state Escurel relies on:
    /// auto-install/-load, `INSTALL`+`LOAD` of `vss`+`fts`, and the
    /// experimental-HNSW-persistence flag (see `sql/0001_a_autoload.sql`).
    ///
    /// This is **per-connection session state**, not durable schema — `LOAD`
    /// and `SET` apply only to the connection that ran them. It MUST run on
    /// **every** connection that touches the index, on every boot — not just
    /// when the DuckDB file is fresh. A connection opened against an existing
    /// DB that skips this cannot modify the HNSW-indexed `blocks` table
    /// (`Cannot bind index 'blocks', unknown index type 'HNSW'`). `INSTALL` is
    /// idempotent (a no-op once the binary is on disk / baked in the image).
    pub fn load_extensions(conn: &Connection) -> Result<(), MigrationError> {
        conn.execute_batch(STAGE_1_AUTOLOAD)?;
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
        // runs `load_extensions`. Writing the HNSW table must still succeed.
        let conn = Connection::open(&path).expect("reopen");
        Migrator::load_extensions(&conn).expect("load_extensions on reopen");
        insert(&conn, "b-reopen")
            .expect("modifying the HNSW-indexed blocks table after reopen must succeed");

        drop(conn);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("duckdb.wal"));
    }
}
