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
use escurel_storage::{Key, LaneStore};

use crate::health::{ReadinessProbe, ReadinessReport};

/// Probes the live backends behind `/readyz`.
pub struct DependencyProbe {
    store: Arc<dyn LaneStore>,
    embedder: Arc<ReloadableEmbedder>,
    tenant: String,
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
        }
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
        }
    }
}
