//! Persistence backend trait + the DuckDB implementation.
//!
//! The two tables that back this layer (`crdt_ops`, `crdt_snapshots`)
//! are created by `escurel-index`'s `Migrator::up`; their DDL is
//! documented in `docs/spec/storage.md §CRDT persistence`. This
//! crate **does not** create the schema — callers must run the
//! migrator on the connection first.
//!
//! Production deployments share a single `Arc<Mutex<Connection>>`
//! across the indexer and this backend so reads see writes — a
//! second `Connection::open` on the same file can return a stale
//! snapshot
//! (`docs/notes/discovered/2026-05-24-duckdb-second-connection-stale.md`).

use std::sync::Arc;

use async_trait::async_trait;
use duckdb::{Connection, params};
use tokio::sync::Mutex;

use crate::{Error, Op, Snapshot};

/// Snapshot row plus the ops that arrived strictly after it.
///
/// This is the shape `LiveDoc::open` consumes during replay: import
/// the snapshot, then replay the ops in HLC order.
pub type LoadedState = (Snapshot, Vec<Op>);

/// Persistence boundary for the live-CRDT layer.
///
/// Implementations must be `Send + Sync + 'static` so the
/// `LiveDoc` actor can hold an `Arc<dyn CrdtBackend>` across
/// `.await` points.
#[async_trait]
pub trait CrdtBackend: Send + Sync + 'static {
    /// Persist a single op atomically. The returned [`crate::Version`]
    /// is derived by the caller (the `LiveDoc` actor) — backends
    /// don't pick the version, they just write.
    ///
    /// `hlc` is the monotonic op-count for v1; M4.6 will switch
    /// to a real HLC.
    async fn append_op(&self, page_id: &str, op_id: &str, hlc: i64, op: &Op) -> Result<(), Error>;

    /// Insert a snapshot row. Called on session close
    /// (`commit=true`) and on periodic checkpoints.
    async fn snapshot(&self, page_id: &str, hlc: i64, snap: &Snapshot) -> Result<(), Error>;

    /// Replay state for a page: the most recent snapshot (if any)
    /// and every op with `hlc > snapshot.snapshot_hlc`. Returns
    /// `Ok(None)` for pages with no CRDT state.
    async fn load(&self, page_id: &str) -> Result<Option<LoadedState>, Error>;

    /// Highest `hlc` already stored across `crdt_ops` and
    /// `crdt_snapshots` for `page_id`. Returns `0` for never-seen
    /// pages. The `LiveDoc` actor uses this to seed its monotonic
    /// op-count after a reopen, so a new op never reuses an
    /// existing `(page_id, op_id)` primary key.
    async fn max_hlc(&self, page_id: &str) -> Result<i64, Error>;

    /// Every `page_id` that has at least one row in
    /// `crdt_snapshots`. Used by the admin `compact_lanes` sweep
    /// to enumerate compaction-eligible pages — pages with no
    /// snapshot have nothing to compact (the spec says ops are
    /// only eligible once `hlc <= snapshot.snapshot_hlc`, so a
    /// page with zero snapshots has zero subsumed ops by
    /// construction).
    async fn pages_with_snapshots(&self) -> Result<Vec<String>, Error>;

    /// Delete `crdt_ops` rows whose `hlc <= latest_snapshot_hlc`
    /// for `page_id`. Returns `(ops_compacted, bytes_reclaimed)`,
    /// where `bytes_reclaimed` is the sum of `LENGTH(op_bytes)`
    /// over the deleted rows. Returns `(0, 0)` when the page has
    /// no snapshot or no eligible ops.
    ///
    /// The byte sum and the delete must run inside the same
    /// transaction so a partial failure rolls back cleanly
    /// (otherwise the reported bytes wouldn't match the rows that
    /// actually went away).
    async fn compact_subsumed_ops(&self, page_id: &str) -> Result<(u64, u64), Error>;
}

/// DuckDB-backed [`CrdtBackend`] over a shared
/// `Arc<Mutex<Connection>>`.
///
/// The mutex is the same kind the indexer uses: DuckDB
/// connections are not `Sync`, and concurrent async writers must
/// serialise. Reusing one connection across the backend and the
/// indexer is the production pattern — see the module-level note.
pub struct DuckdbCrdtBackend {
    conn: Arc<Mutex<Connection>>,
}

impl DuckdbCrdtBackend {
    /// Build a backend over an existing connection. The connection
    /// must already have the v1 schema applied via
    /// `escurel_index::Migrator::up`.
    #[must_use]
    pub fn new(conn: Arc<Mutex<Connection>>) -> Self {
        Self { conn }
    }
}

#[async_trait]
impl CrdtBackend for DuckdbCrdtBackend {
    async fn append_op(&self, page_id: &str, op_id: &str, hlc: i64, op: &Op) -> Result<(), Error> {
        let guard = self.conn.lock().await;
        guard.execute(
            "INSERT INTO crdt_ops (page_id, op_id, hlc, parent_op_id, op_bytes) \
             VALUES (?, ?, ?, NULL, ?)",
            params![page_id, op_id, hlc, op.as_bytes()],
        )?;
        Ok(())
    }

    async fn snapshot(&self, page_id: &str, hlc: i64, snap: &Snapshot) -> Result<(), Error> {
        let guard = self.conn.lock().await;
        guard.execute(
            "INSERT INTO crdt_snapshots (page_id, snapshot_hlc, snapshot_bytes) \
             VALUES (?, ?, ?)",
            params![page_id, hlc, snap.as_bytes()],
        )?;
        Ok(())
    }

    async fn load(&self, page_id: &str) -> Result<Option<LoadedState>, Error> {
        let guard = self.conn.lock().await;

        // Latest snapshot — None if the page has no snapshot row.
        let snap_row: Option<(i64, Vec<u8>)> = guard
            .query_row(
                "SELECT snapshot_hlc, snapshot_bytes \
                 FROM crdt_snapshots \
                 WHERE page_id = ? \
                 ORDER BY snapshot_hlc DESC \
                 LIMIT 1",
                params![page_id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
            )
            .ok();

        // Pick the snapshot floor: ops with hlc > floor are replayed.
        // If there's no snapshot, the floor is i64::MIN so we get
        // every op for the page.
        let (snapshot, floor_hlc) = match snap_row {
            Some((hlc, bytes)) => (Snapshot::new(bytes), hlc),
            None => {
                // Probe whether any ops exist at all — empty page
                // returns None.
                let op_count: i64 = guard.query_row(
                    "SELECT count(*) FROM crdt_ops WHERE page_id = ?",
                    params![page_id],
                    |row| row.get(0),
                )?;
                if op_count == 0 {
                    return Ok(None);
                }
                (Snapshot::new(Vec::new()), i64::MIN)
            }
        };

        let mut stmt = guard.prepare(
            "SELECT op_bytes FROM crdt_ops \
             WHERE page_id = ? AND hlc > ? \
             ORDER BY hlc ASC",
        )?;
        let rows = stmt.query_map(params![page_id, floor_hlc], |row| {
            Ok(Op::new(row.get::<_, Vec<u8>>(0)?))
        })?;
        let mut ops = Vec::new();
        for row in rows {
            ops.push(row?);
        }
        Ok(Some((snapshot, ops)))
    }

    async fn max_hlc(&self, page_id: &str) -> Result<i64, Error> {
        let guard = self.conn.lock().await;
        // GREATEST over the two tables' max(hlc); both default to 0
        // when empty via COALESCE so a never-seen page returns 0.
        let max_op: Option<i64> = guard
            .query_row(
                "SELECT max(hlc) FROM crdt_ops WHERE page_id = ?",
                params![page_id],
                |row| row.get(0),
            )
            .ok();
        let max_snap: Option<i64> = guard
            .query_row(
                "SELECT max(snapshot_hlc) FROM crdt_snapshots WHERE page_id = ?",
                params![page_id],
                |row| row.get(0),
            )
            .ok();
        Ok(max_op.unwrap_or(0).max(max_snap.unwrap_or(0)))
    }

    async fn pages_with_snapshots(&self) -> Result<Vec<String>, Error> {
        let guard = self.conn.lock().await;
        let mut stmt =
            guard.prepare("SELECT DISTINCT page_id FROM crdt_snapshots ORDER BY page_id ASC")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    async fn compact_subsumed_ops(&self, page_id: &str) -> Result<(u64, u64), Error> {
        let mut guard = self.conn.lock().await;
        // Resolve the snapshot floor outside the txn — if there is
        // no snapshot, nothing is eligible and we return early
        // without touching the table at all.
        let floor: Option<i64> = guard
            .query_row(
                "SELECT max(snapshot_hlc) FROM crdt_snapshots WHERE page_id = ?",
                params![page_id],
                |row| row.get(0),
            )
            .ok()
            .flatten();
        let Some(floor) = floor else {
            return Ok((0, 0));
        };

        // Measure + delete in one transaction so the reported
        // bytes always describe the rows that actually disappeared.
        // `transaction()` requires `&mut Connection`, which is why
        // the lock above is mutable.
        let tx = guard.transaction()?;
        // DuckDB's `SUM(LENGTH(blob))` returns a HUGEINT (decimal-
        // wrapped) which doesn't fit a plain `i64` getter and would
        // silently surface as 0 via unwrap_or. Cast explicitly to
        // BIGINT so the column type matches the binding.
        let bytes: i64 = tx
            .query_row(
                "SELECT CAST(COALESCE(SUM(OCTET_LENGTH(op_bytes)), 0) AS BIGINT) \
                 FROM crdt_ops WHERE page_id = ? AND hlc <= ?",
                params![page_id, floor],
                |row| row.get(0),
            )
            .unwrap_or(0);
        let deleted = tx.execute(
            "DELETE FROM crdt_ops WHERE page_id = ? AND hlc <= ?",
            params![page_id, floor],
        )?;
        tx.commit()?;

        // Clamp negatives that an absurd `LENGTH()` answer could
        // produce; both fields are u64 on the wire.
        let bytes_u64 = u64::try_from(bytes).unwrap_or(0);
        let deleted_u64 = u64::try_from(deleted).unwrap_or(0);
        Ok((deleted_u64, bytes_u64))
    }
}
