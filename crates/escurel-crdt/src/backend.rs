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

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use duckdb::{Connection, params};
use tokio::sync::Mutex;

use crate::{Error, Op, Snapshot};

/// Which physical tables a [`DuckdbCrdtBackend`] reads/writes (DuckLake
/// PR 10, Phase B). Mirrors `escurel_index::chat::ChatBackend` /
/// `escurel_index::events::EventsBackend` exactly: `Local` (the default,
/// today's single-file behaviour, byte-identical — the per-tenant
/// `crdt_ops`/`crdt_snapshots` tables, no `tenant` column, tenancy
/// implicit in "one DuckDB file per tenant") until
/// [`DuckdbCrdtBackend::attach_shared_pg`] runs, after which every method
/// routes to the attached, tenant-scoped Postgres tables instead.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CrdtBackendMode {
    /// The local per-tenant `crdt_ops` / `crdt_snapshots` tables.
    Local,
    /// The attached, read-write Postgres tables shared by every replica,
    /// scoped by an explicit `tenant` column (the physical tables are one
    /// relation shared by the whole deployment, unlike the local tables).
    AttachedPostgres { alias: String, tenant: String },
}

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

    /// Insert a snapshot row.
    ///
    /// Called on session close (`commit=true`) — and only there. An earlier
    /// version of this comment also claimed "and on periodic checkpoints";
    /// no such call site exists (`LiveDoc::handle_close` is the sole caller),
    /// so a long-lived session accumulates an unbounded op tail that crash
    /// recovery must replay in full. That is a real gap, tracked in
    /// `docs/notes/concurrency-fix-plan.md` F3.3, but it is a missing feature
    /// rather than something this trait promises. Documentation asserting
    /// behaviour the code does not have is the more damaging of the two.
    async fn snapshot(&self, page_id: &str, hlc: i64, snap: &Snapshot) -> Result<(), Error>;

    /// Replay state for a page: the most recent snapshot (if any)
    /// and every op with `hlc > snapshot.snapshot_hlc`. Returns
    /// `Ok(None)` for pages with no CRDT state.
    async fn load(&self, page_id: &str) -> Result<Option<LoadedState>, Error>;

    /// The exact snapshot blob stored at `(page_id, snapshot_hlc = hlc)`,
    /// or `None` if no snapshot was taken at that hlc. Used by the
    /// `update_page` three-way auto-merge (#246) to reconstruct the
    /// *base* a stale `base_version` branched from — every `update_page`
    /// write snapshots the whole page at its version's hlc, so a
    /// `base_version = "v<N>"` maps to the snapshot at `hlc = N`. A version
    /// with no snapshot row (e.g. a bare op-count from the session path)
    /// returns `None`, and the caller falls back to a plain conflict.
    async fn snapshot_at(&self, page_id: &str, hlc: i64) -> Result<Option<Vec<u8>>, Error>;

    /// Highest `hlc` already stored across `crdt_ops` and
    /// `crdt_snapshots` for `page_id`. Returns `0` for never-seen
    /// pages. The `LiveDoc` actor uses this to seed its monotonic
    /// op-count after a reopen, so a new op never reuses an
    /// existing `(page_id, op_id)` primary key.
    async fn max_hlc(&self, page_id: &str) -> Result<i64, Error>;

    /// Append an op, **allocating its hlc from the persisted maximum** in the
    /// same critical section as the insert. Returns the hlc used.
    ///
    /// This exists because `max_hlc` + `append_op` as two calls is not one
    /// allocation. `LiveDoc` used to seed a counter from `max_hlc` once at
    /// open and increment it locally, so a whole-page `update_page` landing
    /// during an open session took the next slot without the session ever
    /// hearing about it — and both writers then stamped *different content*
    /// with the *same version*. `expand.version` could no longer identify a
    /// head, which makes every `base_version` comparison built on it unsound.
    /// See `docs/notes/concurrency-fix-plan.md` F1.3.
    ///
    /// The implementation is the single hlc authority for a page: read the
    /// maximum and insert without releasing the store lock in between.
    async fn append_op_next(&self, page_id: &str, op: &Op) -> Result<i64, Error>;

    /// Insert a snapshot, allocating its hlc the same way as
    /// [`CrdtBackend::append_op_next`]. Returns the hlc used.
    async fn snapshot_next(&self, page_id: &str, snap: &Snapshot) -> Result<i64, Error>;

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

    /// Who wrote each op on `page_id`, oldest first (escurel#357 / CR-6).
    ///
    /// The read side of the [`Op::principal`] stamp, and deliberately NOT a
    /// read of `op_bytes`: it answers "who edited this page, in what order"
    /// without handing out the document's edit payloads. Compaction is
    /// visible here as it is everywhere else — an op subsumed by a snapshot
    /// and swept by `compact_subsumed_ops` is gone, so this is the tail of
    /// the history, not all of it.
    async fn op_authors(&self, page_id: &str) -> Result<Vec<OpAuthor>, Error>;
}

/// One row of [`CrdtBackend::op_authors`]: an op and the principal that
/// submitted it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpAuthor {
    pub op_id: String,
    pub hlc: i64,
    /// RFC-3339-ish `applied_at`, formatted at the SQL layer.
    pub applied_at: Option<String>,
    /// The verified principal that applied the op, or `None` for an op
    /// written before the gateway recorded one (or by a gateway with no
    /// verifier). Never derived from the Loro peer id in `op_bytes` — see
    /// [`Op`].
    pub principal: Option<String>,
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
    /// Unset (→ [`CrdtBackendMode::Local`]) until
    /// [`Self::attach_shared_pg`] runs. A `OnceLock`, not a plain field,
    /// for the same reason `escurel_index::Indexer::chat_backend` is one:
    /// the setter needs `&self`, and this backend is already handed out
    /// as `Arc<dyn CrdtBackend>` by the time the server boot code knows
    /// whether to attach the shared Postgres tables.
    mode: OnceLock<CrdtBackendMode>,
}

impl DuckdbCrdtBackend {
    /// Build a backend over an existing connection. The connection
    /// must already have the v1 schema applied via
    /// `escurel_index::Migrator::up`.
    #[must_use]
    pub fn new(conn: Arc<Mutex<Connection>>) -> Self {
        Self {
            conn,
            mode: OnceLock::new(),
        }
    }

    /// The mode this backend currently routes reads/writes through —
    /// [`CrdtBackendMode::Local`] until [`Self::attach_shared_pg`] runs.
    fn mode(&self) -> CrdtBackendMode {
        self.mode.get().cloned().unwrap_or(CrdtBackendMode::Local)
    }

    /// `true` once [`Self::attach_shared_pg`] has wired this backend onto
    /// the shared CRDT Postgres tables (DuckLake PR 10). `escurel-server`'s
    /// ducklake-reader dispatch gate consults this (indirectly, via
    /// `escurel_index::Indexer::has_shared_crdt`, which is attached to the
    /// SAME `catalog_dsn` at the same boot step) to decide whether
    /// `open_session`/`apply_op`/`close_session`/`list_snapshots` are
    /// servable on a reader.
    #[must_use]
    pub fn has_shared_crdt(&self) -> bool {
        matches!(self.mode(), CrdtBackendMode::AttachedPostgres { .. })
    }

    /// Attach the shared CRDT Postgres tables onto THIS backend's own
    /// connection, read-write and idempotently (DuckLake PR 10, mirrors
    /// `escurel_index::Indexer::attach_chat_pg`'s shape), and point every
    /// subsequent [`CrdtBackend`] method at them instead of the local
    /// `crdt_ops`/`crdt_snapshots` tables. Idempotent to call twice (the
    /// underlying `ATTACH IF NOT EXISTS` / `CREATE TABLE IF NOT EXISTS`
    /// are); the server calls this once at boot for a ducklake writer OR
    /// reader, reusing `LakeConfig::catalog_dsn` — no separate CRDT
    /// config needed. `tenant` scopes every row this backend writes or
    /// reads from here on (the attached tables are one physical relation
    /// shared by every replica of this deployment, unlike the local
    /// tables' implicit one-file-per-tenant scoping).
    ///
    /// # Errors
    ///
    /// See [`crate::pg::attach_crdt_pg`].
    pub async fn attach_shared_pg(&self, catalog_dsn: &str, tenant: &str) -> Result<(), Error> {
        {
            let conn = self.conn.lock().await;
            crate::pg::attach_crdt_pg(&conn, catalog_dsn)?;
        }
        let _ = self.mode.set(CrdtBackendMode::AttachedPostgres {
            alias: crate::pg::CRDT_PG_ALIAS.to_owned(),
            tenant: tenant.to_owned(),
        });
        Ok(())
    }

    /// The table this backend's methods read/write for ops:
    /// `crdt_ops` (local) or `<alias>.escurel_crdt_ops` (attached
    /// Postgres, DuckLake PR 10).
    fn ops_table(&self) -> String {
        match self.mode() {
            CrdtBackendMode::Local => "crdt_ops".to_owned(),
            CrdtBackendMode::AttachedPostgres { alias, .. } => {
                format!("{alias}.{}", crate::pg::CRDT_OPS_PG_TABLE)
            }
        }
    }

    /// The table this backend's methods read/write for snapshots.
    fn snapshots_table(&self) -> String {
        match self.mode() {
            CrdtBackendMode::Local => "crdt_snapshots".to_owned(),
            CrdtBackendMode::AttachedPostgres { alias, .. } => {
                format!("{alias}.{}", crate::pg::CRDT_SNAPSHOTS_PG_TABLE)
            }
        }
    }

    /// `Some(tenant)` when rows must be scoped by an explicit `tenant`
    /// column (the attached-Postgres tables); `None` for the local
    /// tables, whose tenancy is implicit.
    fn tenant_scope(&self) -> Option<String> {
        match self.mode() {
            CrdtBackendMode::Local => None,
            CrdtBackendMode::AttachedPostgres { tenant, .. } => Some(tenant),
        }
    }
}

/// `"AND tenant = ?"` when `tenant_scope` is `Some`, `""` otherwise —
/// shared by every query below so the WHERE-clause shape stays uniform.
fn tenant_clause(tenant_scope: &Option<String>) -> &'static str {
    if tenant_scope.is_some() {
        "AND tenant = ?"
    } else {
        ""
    }
}

#[async_trait]
impl CrdtBackend for DuckdbCrdtBackend {
    async fn append_op(&self, page_id: &str, op_id: &str, hlc: i64, op: &Op) -> Result<(), Error> {
        let guard = self.conn.lock().await;
        self.insert_op_locked(&guard, page_id, op_id, hlc, op)
    }

    async fn append_op_next(&self, page_id: &str, op: &Op) -> Result<i64, Error> {
        // One lock spans the read and the insert; that is what makes this an
        // allocation rather than a guess.
        let guard = self.conn.lock().await;
        let hlc = self.max_hlc_locked(&guard, page_id)? + 1;
        // The op id stays derived from the allocated hlc, so it inherits the
        // uniqueness the `(page_id, op_id)` primary key needs.
        let op_id = format!("{page_id}:{hlc}");
        self.insert_op_locked(&guard, page_id, &op_id, hlc, op)?;
        Ok(hlc)
    }

    async fn snapshot_next(&self, page_id: &str, snap: &Snapshot) -> Result<i64, Error> {
        let guard = self.conn.lock().await;
        let hlc = self.max_hlc_locked(&guard, page_id)? + 1;
        self.insert_snapshot_locked(&guard, page_id, hlc, snap)?;
        Ok(hlc)
    }

    async fn snapshot(&self, page_id: &str, hlc: i64, snap: &Snapshot) -> Result<(), Error> {
        let guard = self.conn.lock().await;
        self.insert_snapshot_locked(&guard, page_id, hlc, snap)
    }

    async fn load(&self, page_id: &str) -> Result<Option<LoadedState>, Error> {
        let guard = self.conn.lock().await;
        let ops_table = self.ops_table();
        let snapshots_table = self.snapshots_table();
        let tenant_scope = self.tenant_scope();
        let tc = tenant_clause(&tenant_scope);

        // Latest snapshot — None if the page has no snapshot row.
        let snap_row: Option<(i64, Vec<u8>)> = match &tenant_scope {
            None => guard
                .query_row(
                    &format!(
                        "SELECT snapshot_hlc, snapshot_bytes FROM {snapshots_table} \
                         WHERE page_id = ? {tc} ORDER BY snapshot_hlc DESC LIMIT 1"
                    ),
                    params![page_id],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
                )
                .ok(),
            Some(tenant) => guard
                .query_row(
                    &format!(
                        "SELECT snapshot_hlc, snapshot_bytes FROM {snapshots_table} \
                         WHERE page_id = ? {tc} ORDER BY snapshot_hlc DESC LIMIT 1"
                    ),
                    params![page_id, tenant],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
                )
                .ok(),
        };

        // Pick the snapshot floor: ops with hlc > floor are replayed.
        // If there's no snapshot, the floor is i64::MIN so we get
        // every op for the page.
        let (snapshot, floor_hlc) = match snap_row {
            Some((hlc, bytes)) => (Snapshot::new(bytes), hlc),
            None => {
                // Probe whether any ops exist at all — empty page
                // returns None.
                let op_count: i64 = match &tenant_scope {
                    None => guard.query_row(
                        &format!("SELECT count(*) FROM {ops_table} WHERE page_id = ? {tc}"),
                        params![page_id],
                        |row| row.get(0),
                    )?,
                    Some(tenant) => guard.query_row(
                        &format!("SELECT count(*) FROM {ops_table} WHERE page_id = ? {tc}"),
                        params![page_id, tenant],
                        |row| row.get(0),
                    )?,
                };
                if op_count == 0 {
                    return Ok(None);
                }
                (Snapshot::new(Vec::new()), i64::MIN)
            }
        };

        let sql = format!(
            "SELECT op_bytes FROM {ops_table} \
             WHERE page_id = ? AND hlc > ? {tc} \
             ORDER BY hlc ASC"
        );
        let mut stmt = guard.prepare(&sql)?;
        let mut ops = Vec::new();
        match &tenant_scope {
            None => {
                let rows = stmt.query_map(params![page_id, floor_hlc], |row| {
                    Ok(Op::new(row.get::<_, Vec<u8>>(0)?))
                })?;
                for row in rows {
                    ops.push(row?);
                }
            }
            Some(tenant) => {
                let rows = stmt.query_map(params![page_id, floor_hlc, tenant], |row| {
                    Ok(Op::new(row.get::<_, Vec<u8>>(0)?))
                })?;
                for row in rows {
                    ops.push(row?);
                }
            }
        }
        Ok(Some((snapshot, ops)))
    }

    async fn snapshot_at(&self, page_id: &str, hlc: i64) -> Result<Option<Vec<u8>>, Error> {
        let guard = self.conn.lock().await;
        let table = self.snapshots_table();
        let tenant_scope = self.tenant_scope();
        let tc = tenant_clause(&tenant_scope);
        let sql = format!(
            "SELECT snapshot_bytes FROM {table} WHERE page_id = ? AND snapshot_hlc = ? {tc}"
        );
        let bytes: Option<Vec<u8>> = match &tenant_scope {
            None => guard
                .query_row(&sql, params![page_id, hlc], |row| row.get::<_, Vec<u8>>(0))
                .ok(),
            Some(tenant) => guard
                .query_row(&sql, params![page_id, hlc, tenant], |row| {
                    row.get::<_, Vec<u8>>(0)
                })
                .ok(),
        };
        Ok(bytes)
    }

    async fn max_hlc(&self, page_id: &str) -> Result<i64, Error> {
        let guard = self.conn.lock().await;
        self.max_hlc_locked(&guard, page_id)
    }

    async fn pages_with_snapshots(&self) -> Result<Vec<String>, Error> {
        let guard = self.conn.lock().await;
        let table = self.snapshots_table();
        let tenant_scope = self.tenant_scope();
        let mut out = Vec::new();
        match &tenant_scope {
            None => {
                let mut stmt = guard.prepare(&format!(
                    "SELECT DISTINCT page_id FROM {table} ORDER BY page_id ASC"
                ))?;
                let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
                for row in rows {
                    out.push(row?);
                }
            }
            Some(tenant) => {
                let mut stmt = guard.prepare(&format!(
                    "SELECT DISTINCT page_id FROM {table} WHERE tenant = ? ORDER BY page_id ASC"
                ))?;
                let rows = stmt.query_map(params![tenant], |row| row.get::<_, String>(0))?;
                for row in rows {
                    out.push(row?);
                }
            }
        }
        Ok(out)
    }

    async fn compact_subsumed_ops(&self, page_id: &str) -> Result<(u64, u64), Error> {
        let mut guard = self.conn.lock().await;
        let ops_table = self.ops_table();
        let snapshots_table = self.snapshots_table();
        let tenant_scope = self.tenant_scope();
        let tc = tenant_clause(&tenant_scope);

        // Resolve the snapshot floor outside the txn — if there is
        // no snapshot, nothing is eligible and we return early
        // without touching the table at all.
        let floor: Option<i64> = match &tenant_scope {
            None => guard
                .query_row(
                    &format!(
                        "SELECT max(snapshot_hlc) FROM {snapshots_table} WHERE page_id = ? {tc}"
                    ),
                    params![page_id],
                    |row| row.get(0),
                )
                .ok()
                .flatten(),
            Some(tenant) => guard
                .query_row(
                    &format!(
                        "SELECT max(snapshot_hlc) FROM {snapshots_table} WHERE page_id = ? {tc}"
                    ),
                    params![page_id, tenant],
                    |row| row.get(0),
                )
                .ok()
                .flatten(),
        };
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
        let (bytes, deleted): (i64, usize) = match &tenant_scope {
            None => {
                let bytes: i64 = tx
                    .query_row(
                        &format!(
                            "SELECT CAST(COALESCE(SUM(OCTET_LENGTH(op_bytes)), 0) AS BIGINT) \
                             FROM {ops_table} WHERE page_id = ? AND hlc <= ? {tc}"
                        ),
                        params![page_id, floor],
                        |row| row.get(0),
                    )
                    .unwrap_or(0);
                let deleted = tx.execute(
                    &format!("DELETE FROM {ops_table} WHERE page_id = ? AND hlc <= ? {tc}"),
                    params![page_id, floor],
                )?;
                (bytes, deleted)
            }
            Some(tenant) => {
                let bytes: i64 = tx
                    .query_row(
                        &format!(
                            "SELECT CAST(COALESCE(SUM(OCTET_LENGTH(op_bytes)), 0) AS BIGINT) \
                             FROM {ops_table} WHERE page_id = ? AND hlc <= ? {tc}"
                        ),
                        params![page_id, floor, tenant],
                        |row| row.get(0),
                    )
                    .unwrap_or(0);
                let deleted = tx.execute(
                    &format!("DELETE FROM {ops_table} WHERE page_id = ? AND hlc <= ? {tc}"),
                    params![page_id, floor, tenant],
                )?;
                (bytes, deleted)
            }
        };
        tx.commit()?;

        // Clamp negatives that an absurd `LENGTH()` answer could
        // produce; both fields are u64 on the wire.
        let bytes_u64 = u64::try_from(bytes).unwrap_or(0);
        let deleted_u64 = u64::try_from(deleted).unwrap_or(0);
        Ok((deleted_u64, bytes_u64))
    }

    async fn op_authors(&self, page_id: &str) -> Result<Vec<OpAuthor>, Error> {
        let guard = self.conn.lock().await;
        let table = self.ops_table();
        let tenant_scope = self.tenant_scope();
        let tc = tenant_clause(&tenant_scope);
        // `hlc` is the allocation order, so ordering by it puts the ops in
        // the order they were accepted — which is what an authorship
        // timeline means. `applied_at` is a wall clock and can tie.
        let sql = format!(
            "SELECT op_id, hlc, strftime(applied_at, '%Y-%m-%dT%H:%M:%SZ'), principal \
             FROM {table} WHERE page_id = ? {tc} ORDER BY hlc ASC, op_id ASC"
        );
        let row_to_author = |row: &duckdb::Row<'_>| {
            Ok(OpAuthor {
                op_id: row.get::<_, String>(0)?,
                hlc: row.get::<_, i64>(1)?,
                applied_at: row.get::<_, Option<String>>(2)?,
                principal: row.get::<_, Option<String>>(3)?,
            })
        };
        let mut stmt = guard.prepare(&sql)?;
        let rows = match &tenant_scope {
            None => stmt.query_map(params![page_id], row_to_author)?,
            Some(tenant) => stmt.query_map(params![page_id, tenant], row_to_author)?,
        };
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
}

/// Helpers shared by the plain and the allocating write paths.
///
/// Each takes an already-held connection guard, so a caller that must read
/// and write atomically (`append_op_next`, `snapshot_next`) can do both
/// inside one critical section instead of taking the lock twice.
impl DuckdbCrdtBackend {
    /// `append_op`'s insert, callable with the connection guard already held.
    fn insert_op_locked(
        &self,
        guard: &Connection,
        page_id: &str,
        op_id: &str,
        hlc: i64,
        op: &Op,
    ) -> Result<(), Error> {
        let table = self.ops_table();
        match self.tenant_scope() {
            None => {
                guard.execute(
                    &format!(
                        "INSERT INTO {table} \
                         (page_id, op_id, hlc, parent_op_id, op_bytes, principal) \
                         VALUES (?, ?, ?, NULL, ?, ?)"
                    ),
                    params![page_id, op_id, hlc, op.as_bytes(), op.principal()],
                )?;
            }
            Some(tenant) => {
                guard.execute(
                    &format!(
                        "INSERT INTO {table} \
                         (tenant, page_id, op_id, hlc, parent_op_id, op_bytes, principal) \
                         VALUES (?, ?, ?, ?, NULL, ?, ?)"
                    ),
                    params![tenant, page_id, op_id, hlc, op.as_bytes(), op.principal()],
                )?;
            }
        }
        Ok(())
    }

    /// `snapshot`'s insert, callable with the connection guard already held.
    fn insert_snapshot_locked(
        &self,
        guard: &Connection,
        page_id: &str,
        hlc: i64,
        snap: &Snapshot,
    ) -> Result<(), Error> {
        let table = self.snapshots_table();
        match self.tenant_scope() {
            None => {
                guard.execute(
                    &format!(
                        "INSERT INTO {table} (page_id, snapshot_hlc, snapshot_bytes) \
                         VALUES (?, ?, ?)"
                    ),
                    params![page_id, hlc, snap.as_bytes()],
                )?;
            }
            Some(tenant) => {
                guard.execute(
                    &format!(
                        "INSERT INTO {table} \
                         (tenant, page_id, snapshot_hlc, snapshot_bytes) \
                         VALUES (?, ?, ?, ?)"
                    ),
                    params![tenant, page_id, hlc, snap.as_bytes()],
                )?;
            }
        }
        Ok(())
    }

    /// `max_hlc`'s query, callable with the connection guard already held.
    ///
    /// Sharing it is what lets `append_op_next` allocate and insert without
    /// releasing the lock in between.
    fn max_hlc_locked(&self, guard: &Connection, page_id: &str) -> Result<i64, Error> {
        let ops_table = self.ops_table();
        let snapshots_table = self.snapshots_table();
        let tenant_scope = self.tenant_scope();
        let tc = tenant_clause(&tenant_scope);

        // GREATEST over the two tables' max(hlc); both default to 0
        // when empty via COALESCE so a never-seen page returns 0.
        let max_op: Option<i64> = match &tenant_scope {
            None => guard
                .query_row(
                    &format!("SELECT max(hlc) FROM {ops_table} WHERE page_id = ? {tc}"),
                    params![page_id],
                    |row| row.get(0),
                )
                .ok(),
            Some(tenant) => guard
                .query_row(
                    &format!("SELECT max(hlc) FROM {ops_table} WHERE page_id = ? {tc}"),
                    params![page_id, tenant],
                    |row| row.get(0),
                )
                .ok(),
        };
        let max_snap: Option<i64> = match &tenant_scope {
            None => guard
                .query_row(
                    &format!(
                        "SELECT max(snapshot_hlc) FROM {snapshots_table} WHERE page_id = ? {tc}"
                    ),
                    params![page_id],
                    |row| row.get(0),
                )
                .ok(),
            Some(tenant) => guard
                .query_row(
                    &format!(
                        "SELECT max(snapshot_hlc) FROM {snapshots_table} WHERE page_id = ? {tc}"
                    ),
                    params![page_id, tenant],
                    |row| row.get(0),
                )
                .ok(),
        };
        Ok(max_op.unwrap_or(0).max(max_snap.unwrap_or(0)))
    }
}
