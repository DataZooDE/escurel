//! [`DependencyProbe`] — the production `/readyz` probe.
//!
//! Reports each dependency the spec's readiness contract names
//! (`docs/spec/platform.md §Health endpoints`):
//!
//! - **lane_store** — a cheap `list` on the tenant's prefix; any
//!   non-error response (including empty) means the store is
//!   reachable. For `FsStore` this is a local readdir; for `S3Store`
//!   it is a `ListObjectsV2` round-trip.
//! - **indexer** — `true` once the per-tenant DuckDB is open and
//!   migrated (it is, by the time this probe exists).
//! - **embedder** — the [`ReloadableEmbedder::is_loaded`] flag, which
//!   is `false` during a *degraded start* and flips to `true` after a
//!   successful `embedding_reload`.

use std::sync::Arc;

use async_trait::async_trait;
use escurel_embed::ReloadableEmbedder;
use escurel_index::IndexerHandle;
use escurel_storage::{Key, LaneStore};

use crate::health::{ReadinessProbe, ReadinessReport};

/// Probes the live backends behind `/readyz`.
pub struct DependencyProbe {
    store: Arc<dyn LaneStore>,
    embedder: Arc<ReloadableEmbedder>,
    tenant: String,
    /// Where the quarantine state lives (it changes at runtime: `migrate_kind --apply` lifts it).
    indexer: Option<IndexerHandle>,
    /// Real (non-zero-vector) embeddings configured: see [`ReadinessReport::semantic_search`].
    semantic_search: bool,
}

impl DependencyProbe {
    #[must_use]
    pub fn new(
        store: Arc<dyn LaneStore>,
        embedder: Arc<ReloadableEmbedder>,
        tenant: String,
    ) -> Self {
        Self {
            store,
            embedder,
            tenant,
            indexer: None,
            semantic_search: true,
        }
    }

    /// Report the tenant's quarantine state (`/readyz`, `/metrics`) from this indexer.
    #[must_use]
    pub fn with_indexer(mut self, indexer: IndexerHandle) -> Self {
        self.indexer = Some(indexer);
        self
    }

    /// Say whether real embeddings are configured (false for the zero-vector stand-in).
    #[must_use]
    pub fn with_semantic_search(mut self, enabled: bool) -> Self {
        self.semantic_search = enabled;
        self
    }
}

#[async_trait]
impl ReadinessProbe for DependencyProbe {
    async fn probe(&self) -> ReadinessReport {
        // Cheap reachability check: list the tenant root. An invalid
        // tenant key would be a programming error (the tenant string
        // is validated at config time), so a key-construction failure
        // counts as not-ready rather than panicking.
        let lane_store = match Key::new(self.tenant.as_str(), "") {
            Ok(prefix) => self.store.list(&prefix).await.is_ok(),
            Err(_) => false,
        };

        ReadinessReport {
            lane_store,
            // The indexer is constructed before this probe exists; if
            // we got here it is open + migrated.
            indexer: true,
            embedder: self.embedder.is_loaded(),
            // Single-file, ducklake-writer, and ducklake-reader all build
            // their serving index SYNCHRONOUSLY at boot (the reader's
            // `adopt_lake` runs before the HTTP listener binds — see
            // `EscurelConfig::build`), so by the time this probe can be
            // asked at all, a snapshot has already been adopted.
            index_snapshot: true,
            quarantined: self
                .indexer
                .as_ref()
                .is_some_and(|h| h.current().legacy_quarantine().is_some()),
            // The durable marker `migrate_kind` writes before its first rewrite and clears last: a
            // crash in between leaves it behind, and /readyz + /metrics say so.
            migration_pending: match Key::new(
                self.tenant.as_str(),
                escurel_index::migrate_kind::MIGRATION_MARKER_PATH,
            ) {
                Ok(k) => self.store.read(&k).await.is_ok(),
                Err(_) => false,
            },
            semantic_search: self.semantic_search && self.embedder.is_loaded(),
        }
    }
}
