//! [`SingleFileStore`] — the classic one-DuckDB-file-per-tenant
//! backend, extracted verbatim from the server boot path
//! (`escurel-server/src/config.rs::build`). Zero behaviour change:
//! `open()` performs the same steps, in the same order, as the
//! pre-seam boot did.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use duckdb::Connection;
use escurel_embed::Embedder;
use escurel_storage::LaneStore;

use super::{IndexStore, OpenedIndex, SnapshotError};
use crate::backend::ContextualizeMode;
use crate::indexer::Indexer;
use crate::schema::Migrator;

/// Hook that attaches the retrieval stages (reranker / two-pass) to a
/// freshly built [`Indexer`]. Injected by the server so model loading
/// (feature-gated, degraded-start on failure) stays out of this crate
/// while running at exactly the same point in the boot sequence as
/// before: after `Indexer::new`, before the fresh-boot `rebuild()`.
pub type AttachRetrievalFn =
    Arc<dyn Fn(Indexer) -> Pin<Box<dyn Future<Output = Indexer> + Send>> + Send + Sync>;

/// The single-file [`IndexStore`]: one persistent DuckDB at
/// `<tenant_dir>/escurel.duckdb`, HNSW persistence enabled, schema
/// migrated on a fresh file, index rebuilt from the canonical
/// markdown lane when the file was just (re)created.
///
/// Field-for-field this is what `EscurelConfig::build` used to feed
/// its inline boot sequence; the server constructs one and calls
/// [`IndexStore::open`].
pub struct SingleFileStore {
    /// Per-tenant state dir (`<data_dir>/tenants/<tenant>`); the
    /// DuckDB file lives at `<tenant_dir>/escurel.duckdb`.
    pub tenant_dir: PathBuf,
    /// `ESCUREL_REBUILD_INDEX_ON_BOOT=always`: drop the DuckDB (a
    /// rebuildable cache) + its WAL before opening, so the fresh-boot
    /// path reconstructs a clean index from the canonical markdown.
    pub rebuild_on_boot: bool,
    /// Canonical markdown lane the index is derived from.
    pub store: Arc<dyn LaneStore>,
    /// Embedder for the dense lane (may be a degraded placeholder).
    pub embedder: Arc<dyn Embedder>,
    /// Tenant id (already validated by the caller — it is joined into
    /// the filesystem path via `tenant_dir`).
    pub tenant: String,
    /// Contextual-retrieval mode stamped on the indexer
    /// (`ESCUREL_INGEST_CONTEXTUALIZE`).
    pub contextualize: ContextualizeMode,
    /// Retrieval-stage attach hook (reranker / two-pass). `None` runs
    /// the indexer with first-stage ranking only.
    pub attach_retrieval: Option<AttachRetrievalFn>,
    /// Optional markdown seed directory imported at boot (idempotent).
    pub seed_dir: Option<PathBuf>,
}

#[async_trait]
impl IndexStore for SingleFileStore {
    /// Today's boot sequence, verbatim (see the pre-seam
    /// `escurel-server/src/config.rs::build` for the original):
    ///
    /// 1. ensure `tenant_dir`, resolve the DuckDB path;
    /// 2. `rebuild_on_boot` → drop file + WAL;
    /// 3. open, `load_extensions` + `enable_hnsw_persistence` on
    ///    EVERY boot, `Migrator::up` fresh-only, `ensure_*` every
    ///    boot;
    /// 4. `try_clone` the CRDT connection BEFORE `conn` moves into
    ///    the indexer (same instance — see [`OpenedIndex`]);
    /// 5. `Indexer::new` + contextualize + retrieval attach;
    /// 6. fresh-only `rebuild()` (cattle-node-loss recovery);
    /// 7. optional `seed_from_dir`;
    /// 8. `ensure_meta_skill`.
    async fn open(&self) -> Result<OpenedIndex, SnapshotError> {
        std::fs::create_dir_all(&self.tenant_dir).map_err(|source| SnapshotError::DataDir {
            path: self.tenant_dir.display().to_string(),
            source,
        })?;
        let db_path = self.tenant_dir.join("escurel.duckdb");
        // Derived-index boot policy. `rebuild_on_boot` drops the DuckDB (a
        // rebuildable cache) + its WAL so the fresh-boot path below
        // reconstructs a clean index from the canonical markdown LaneStore
        // (vss's experimental HNSW persistence segfaults when a restart
        // reloads the on-disk index). Otherwise an existing index stays in
        // place for a fast, re-embed-free restart. The markdown corpus is
        // never touched. NOTE: this drops derived state that is NOT restored
        // by the rebuild (chat/CRDT, credential/endpoint registries).
        if self.rebuild_on_boot && db_path.exists() {
            std::fs::remove_file(&db_path).map_err(|source| SnapshotError::DataDir {
                path: db_path.display().to_string(),
                source,
            })?;
            // Best-effort WAL removal — absent after a clean checkpoint.
            let _ = std::fs::remove_file(self.tenant_dir.join("escurel.duckdb.wal"));
        }
        let fresh = !db_path.exists();
        let conn = Connection::open(&db_path).map_err(|source| SnapshotError::DuckdbOpen {
            path: db_path.display().to_string(),
            source,
        })?;
        // `vss`/`fts` + the HNSW-persistence flag are per-connection session
        // state, so load them on EVERY boot — not only when the DB is fresh.
        // The schema DDL (`up`) is one-time, but a restart against an existing
        // DB still needs these on this write connection, or modifying the
        // HNSW-indexed `blocks` table fails ("unknown index type 'HNSW'").
        // `INSTALL` is idempotent.
        Migrator::load_extensions(&conn)?;
        Migrator::enable_hnsw_persistence(&conn)?;
        if fresh {
            Migrator::up(&conn)?;
        }
        // Group ACL v1: ensure the `group_members` table on EVERY boot
        // (idempotent), so a tenant DB provisioned before this table
        // existed gains it on the next restart. `up` (fresh only) also
        // creates it; the `IF NOT EXISTS` makes this a no-op there.
        Migrator::ensure_group_members(&conn)?;
        // SQL-view credential registry: ensure on EVERY boot (idempotent),
        // like group_members. A separate canonical input, never dropped by
        // rebuild.
        Migrator::ensure_external_credentials(&conn)?;
        // Remote-backend endpoint registry (openapi/mcp): ensure on EVERY boot
        // (idempotent), like the credential registry. Separate canonical input.
        Migrator::ensure_external_endpoints(&conn)?;
        // Skill-pack subscription pins: ensure on EVERY boot (idempotent),
        // like the credential registry. Separate canonical input.
        Migrator::ensure_pack_subscriptions(&conn)?;
        // Contextual Retrieval (GH #216): ensure `blocks.context` on EVERY
        // boot (idempotent), so a tenant DB provisioned before the column
        // existed gains it before `refresh_fts` indexes it.
        Migrator::ensure_block_context(&conn)?;

        // The CRDT backend MUST share the SAME DuckDB instance as the indexer.
        // A second `Connection::open` on the same file is a separate database
        // instance with its own buffer manager + WAL; their checkpoints race
        // and the CRDT instance silently clobbers the indexer's committed
        // writes (see docs/notes/discovered/2026-05-24-duckdb-second-connection-stale.md
        // + 2026-05-26-server-binary-crdt-second-connection.md). `try_clone`
        // opens a second connection to the ALREADY-OPENED database, so the two
        // share one instance + MVCC. Clone before `conn` moves into the indexer.
        let crdt_conn = conn
            .try_clone()
            .map_err(|source| SnapshotError::DuckdbOpen {
                path: db_path.display().to_string(),
                source,
            })?;
        Migrator::load_extensions(&crdt_conn)?;
        Migrator::enable_hnsw_persistence(&crdt_conn)?;

        // Build the indexer, then attach the retrieval stages via the
        // injected hook (reranker load is degraded-start in the server —
        // never fatal — which is why the hook is infallible).
        let base_indexer = Indexer::new(
            Arc::clone(&self.store),
            Arc::clone(&self.embedder),
            conn,
            self.tenant.clone(),
        )?
        .with_contextualize(self.contextualize);
        let indexer = Arc::new(match self.attach_retrieval.as_ref() {
            Some(attach) => attach(base_indexer).await,
            None => base_indexer,
        });

        // Cattle-node-loss recovery: when the DuckDB file was just
        // created but the LaneStore still holds canonical markdown
        // (fresh host / wiped local volume), rebuild the index from
        // that markdown so the server doesn't serve an empty corpus
        // until an operator runs the admin rebuild. On a genuine
        // first boot the store is empty and this is a fast no-op.
        if fresh {
            indexer.rebuild().await?;
        }

        // Optional seed: import a directory of markdown (e.g.
        // `examples/crm-demo`) into this tenant at boot. Idempotent
        // (upsert by body_hash), so it's safe to leave set across
        // restarts.
        if let Some(dir) = self.seed_dir.as_ref() {
            indexer.seed_from_dir(dir).await?;
        }

        // Every served tenant ships the mandatory `escurel` meta-skill
        // — the agent's in-corpus navigation doc (locked decision 3,
        // docs/contract/agent-interface.md). Idempotent: a no-op when
        // the tenant already carries an `escurel` skill page.
        indexer.ensure_meta_skill().await?;

        Ok(OpenedIndex {
            indexer,
            crdt_conn: Some(crdt_conn),
        })
    }
}
