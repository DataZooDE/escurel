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
    /// The served tenant is QUARANTINED: it holds pages with the removed `type:` page-kind key and
    /// answers every MCP tool except `migrate_kind` / `compact_lanes` with `tenant_quarantined`.
    ///
    /// Deliberately NOT part of [`ReadinessReport::all_up`]: `/readyz` stays 200 so the one-shot
    /// `escurel admin migrate-kind` can run against the new image. It is an INFORMATIONAL field that
    /// orchestrators and humans read (JSON body + `x-escurel-quarantined` header + the
    /// `escurel_tenant_quarantined` metric); a deploy must be stop-first with the migration BEFORE
    /// the traffic swap (docs/deploy/kind-migration.md).
    pub quarantined: bool,
    /// A migration started but has not finished (set by `migrate_kind`; the tenant may be serving a
    /// half-migrated corpus). Informational, like `quarantined`.
    pub migration_pending: bool,
    /// Semantic (vector) search is actually available. `false` when the embedder is the zero-vector
    /// stand-in (`ESCUREL_EMBEDDING_PROVIDER=zero`, or `gemini` with no `ESCUREL_GEMINI_API_KEY`):
    /// lexical search still works, ranking by meaning does not. Informational.
    pub semantic_search: bool,
    /// Pages the boot-time rebuild could not parse and SKIPPED (`page (reason)`); they stay
    /// untouched in the lane but are not served. Informational.
    pub skipped_pages: Vec<String>,
    /// Authentication is DISABLED and the listener is not loopback: every caller is a tenant
    /// admin. Informational (a dev setup on a laptop is fine; a container on 0.0.0.0 is not).
    pub unauthenticated_exposed: bool,
}

impl ReadinessReport {
    #[must_use]
    pub fn all_up(&self) -> bool {
        self.lane_store && self.indexer && self.embedder && self.index_snapshot
    }

    /// Anything an operator should look at even though the instance is ready.
    #[must_use]
    pub fn notices(&self) -> Vec<&'static str> {
        let mut n = Vec::new();
        if self.quarantined {
            n.push("quarantined");
        }
        if self.migration_pending {
            n.push("migration_pending");
        }
        if !self.semantic_search {
            n.push("semantic_search_disabled");
        }
        if !self.skipped_pages.is_empty() {
            n.push("pages_skipped");
        }
        if self.unauthenticated_exposed {
            n.push("unauthenticated_exposed");
        }
        n
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
            quarantined: false,
            migration_pending: false,
            semantic_search: true,
            skipped_pages: Vec::new(),
            unauthenticated_exposed: false,
        }
    }
}
