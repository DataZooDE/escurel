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
}

impl ReadinessReport {
    #[must_use]
    pub fn all_up(&self) -> bool {
        self.lane_store && self.indexer && self.embedder
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
        }
    }
}
