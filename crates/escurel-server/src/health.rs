//! `/readyz` probe + per-dependency report.
//!
//! The gateway calls into a single [`ReadinessProbe`] when
//! `/readyz` is requested. Substrate orchestrators (Kamal / kamal-proxy) wire
//! `/readyz` as the deployment readiness probe; blue/green
//! canary promotion respects it (a green allocation receives
//! public traffic only after every probed dependency reports
//! up — see `docs/spec/platform.md §Health endpoints`).

use async_trait::async_trait;
use serde::Serialize;

/// Per-component up/down status, surfaced verbatim in the
/// `/readyz` JSON body when one or more dependencies are down.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ReadinessReport {
    pub lane_store: bool,
    pub indexer: bool,
    pub embedder: bool,
    /// Has this instance's serving index adopted at least one snapshot?
    /// Single-file and ducklake-writer boots build their index
    /// synchronously (same as `indexer` above), so this is `true` the
    /// moment `/readyz` can be asked at all. A ducklake reader
    /// (DuckLake PR 6) also adopts synchronously at boot — before the
    /// HTTP listener binds — so today this field is `true` for every
    /// probeable instance; it exists as a distinct signal for a FUTURE
    /// async-cold-start reader design, not because this PR's readers can
    /// ever observe it `false`.
    pub index_snapshot: bool,
}

impl ReadinessReport {
    #[must_use]
    pub fn all_up(&self) -> bool {
        self.lane_store && self.indexer && self.embedder && self.index_snapshot
    }
}

/// Server-side trait the gateway calls. Implementations probe
/// real backing services and return their up/down state.
///
/// The trait is async because real probes (storage round-trip,
/// embedder smoke test) are async; for tests an in-memory impl
/// is trivial.
#[async_trait]
pub trait ReadinessProbe: Send + Sync + 'static {
    async fn probe(&self) -> ReadinessReport;
}

/// All-up trivial probe. Useful for the skeleton tests and as a
/// sane default before the real wiring (M3.4b+) lands.
#[derive(Debug, Default, Clone, Copy)]
pub struct AlwaysReady;

#[async_trait]
impl ReadinessProbe for AlwaysReady {
    async fn probe(&self) -> ReadinessReport {
        ReadinessReport {
            lane_store: true,
            indexer: true,
            embedder: true,
            index_snapshot: true,
        }
    }
}
