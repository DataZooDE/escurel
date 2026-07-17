//! The `IndexStore` seam (DuckLake program, PR 2).
//!
//! Splits "how the per-tenant index is opened / published / adopted"
//! away from the server boot code. Today there is exactly one
//! backend — [`SingleFileStore`], the classic single DuckDB file
//! under `<data_dir>/tenants/<tenant>/escurel.duckdb` — and this
//! module is a pure refactor: `SingleFileStore::open()` reproduces
//! the pre-seam boot sequence step for step. Snapshot-publishing
//! backends (DuckLake) land behind the same trait in a later PR.
//!
//! [`IndexerHandle`] is the companion hot-swap seam: request paths
//! capture the *current* [`Indexer`] once per request via
//! [`IndexerHandle::current`], so a background snapshot adoption can
//! [`IndexerHandle::swap`] a freshly opened indexer in without a
//! restart and without tearing an in-flight request.

mod store;

use std::sync::Arc;

use arc_swap::ArcSwap;
use async_trait::async_trait;
use thiserror::Error;

use crate::indexer::{Indexer, IndexerError};
use crate::schema::MigrationError;

pub use store::{AttachRetrievalFn, SingleFileStore};

/// Errors surfaced by an [`IndexStore`] backend.
#[derive(Debug, Error)]
pub enum SnapshotError {
    /// Creating (or clearing) a directory / file under the tenant dir
    /// failed.
    #[error("creating data dir {path}: {source}")]
    DataDir {
        path: String,
        #[source]
        source: std::io::Error,
    },
    /// Opening (or cloning) the DuckDB connection failed.
    #[error("opening DuckDB at {path}: {source}")]
    DuckdbOpen {
        path: String,
        #[source]
        source: duckdb::Error,
    },
    /// Loading extensions / applying schema DDL failed.
    #[error("applying DuckDB migrations: {0}")]
    Migrate(#[from] MigrationError),
    /// Building or populating the indexer failed.
    #[error("building indexer: {0}")]
    Indexer(#[from] IndexerError),
    /// The backend does not support the requested operation (e.g. the
    /// single-file backend never publishes snapshots).
    #[error("{0}")]
    Unsupported(&'static str),
}

/// A freshly opened per-tenant index: the boot-ready [`Indexer`] plus
/// (for backends that carry one) a second connection **to the same
/// DuckDB instance** for the CRDT layer.
///
/// The CRDT connection MUST be a `try_clone` of the indexer's own
/// connection — a second `Connection::open` on the same file is a
/// separate database instance whose checkpoints race the indexer's
/// (see docs/notes/discovered/2026-05-24-duckdb-second-connection-stale.md).
/// `None` when the backend has no live-CRDT surface.
pub struct OpenedIndex {
    pub indexer: Arc<Indexer>,
    pub crdt_conn: Option<duckdb::Connection>,
}

/// A snapshot adopted from the store by [`IndexStore::adopt_latest`]:
/// the reopened indexer plus the snapshot id it serves.
pub struct AdoptedIndex {
    pub indexer: Arc<Indexer>,
    pub snapshot_id: i64,
}

/// Outcome of [`IndexStore::publish`]. Placeholder for the DuckLake
/// backend (PR 3) — the single-file backend never publishes.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PublishReport {}

/// How a per-tenant index is opened at boot, published as a snapshot,
/// and re-adopted from the latest snapshot.
///
/// The single-file backend implements only [`IndexStore::open`];
/// `publish` / `adopt_latest` are the snapshot surface a
/// DuckLake-style backend fills in.
#[async_trait]
pub trait IndexStore: Send + Sync + 'static {
    /// Open (or create) the backing index and return a boot-ready
    /// [`Indexer`].
    async fn open(&self) -> Result<OpenedIndex, SnapshotError>;

    /// Publish the current state of `ix` as a durable snapshot.
    async fn publish(&self, ix: &Indexer) -> Result<PublishReport, SnapshotError> {
        let _ = ix;
        Err(SnapshotError::Unsupported(
            "single-file backend does not publish snapshots",
        ))
    }

    /// Open the newest snapshot strictly newer than `current` (the
    /// snapshot id already being served, `None` on first adoption).
    /// `Ok(None)` when there is nothing newer to adopt.
    async fn adopt_latest(
        &self,
        current: Option<i64>,
    ) -> Result<Option<AdoptedIndex>, SnapshotError> {
        let _ = current;
        Ok(None)
    }
}

/// Hot-swappable handle on the live [`Indexer`].
///
/// The gateway state holds one of these instead of a bare
/// `Arc<Indexer>`. Request paths call [`IndexerHandle::current`]
/// ONCE at dispatch entry and thread the returned `Arc<Indexer>`
/// through, so a request observes a single consistent indexer even
/// if an adoption [`IndexerHandle::swap`]s mid-flight. `fixed` wraps
/// a never-swapped indexer — today's single-file behaviour.
#[derive(Clone)]
pub struct IndexerHandle(Arc<ArcSwap<Indexer>>);

impl IndexerHandle {
    /// Wrap an indexer that (today) is never swapped.
    #[must_use]
    pub fn fixed(indexer: Arc<Indexer>) -> Self {
        Self(Arc::new(ArcSwap::new(indexer)))
    }

    /// The indexer currently being served. Capture once per request.
    #[must_use]
    pub fn current(&self) -> Arc<Indexer> {
        self.0.load_full()
    }

    /// Swap `next` in and return the previously served indexer.
    pub fn swap(&self, next: Arc<Indexer>) -> Arc<Indexer> {
        self.0.swap(next)
    }
}

impl std::fmt::Debug for IndexerHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IndexerHandle")
            .field("tenant", &self.current().tenant())
            .finish_non_exhaustive()
    }
}
