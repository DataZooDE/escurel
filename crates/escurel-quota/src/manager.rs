//! `QuotaManager`: per-tenant quota state. Composes three
//! `TokenBucket`s + one concurrent-sessions `Semaphore` per
//! tenant, with config defaults + per-tenant overrides.

use std::sync::Arc;

use dashmap::DashMap;
use thiserror::Error;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::token_bucket::{QuotaExhausted, TokenBucket};

/// Per-tenant quota knobs. Defaults match
/// `docs/spec/README.md §Configuration`.
#[derive(Debug, Clone, Copy)]
pub struct QuotaConfig {
    pub queries_per_minute: u32,
    pub writes_per_minute: u32,
    pub embeds_per_minute: u32,
    pub concurrent_sessions: u32,
}

impl QuotaConfig {
    /// Spec defaults: 600 queries, 120 writes, 300 embeds per minute,
    /// 32 concurrent sessions.
    #[must_use]
    pub fn defaults() -> Self {
        Self {
            queries_per_minute: 600,
            writes_per_minute: 120,
            embeds_per_minute: 300,
            concurrent_sessions: 32,
        }
    }
}

impl Default for QuotaConfig {
    fn default() -> Self {
        Self::defaults()
    }
}

/// The rate dimension a caller is debiting. Matches the spec's
/// three buckets verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dimension {
    Queries,
    Writes,
    Embeds,
}

/// Per-tenant quota state.
struct TenantQuota {
    cfg: QuotaConfig,
    queries: TokenBucket,
    writes: TokenBucket,
    embeds: TokenBucket,
    sessions: Arc<Semaphore>,
}

impl TenantQuota {
    fn from_config(cfg: QuotaConfig) -> Self {
        Self {
            cfg,
            queries: TokenBucket::per_minute(cfg.queries_per_minute),
            writes: TokenBucket::per_minute(cfg.writes_per_minute),
            embeds: TokenBucket::per_minute(cfg.embeds_per_minute),
            sessions: Arc::new(Semaphore::new(cfg.concurrent_sessions as usize)),
        }
    }

    fn bucket(&self, d: Dimension) -> &TokenBucket {
        match d {
            Dimension::Queries => &self.queries,
            Dimension::Writes => &self.writes,
            Dimension::Embeds => &self.embeds,
        }
    }
}

/// Snapshot of a tenant's remaining quota at a single instant.
///
/// `queries_remaining` / `writes_remaining` / `embeds_remaining`
/// are the integer floors of the three buckets' current
/// token counts (rounded down so a partial token reads as
/// "none of it yet"). `concurrent_sessions_in_use` is the count
/// of session slots currently held by live [`SessionGuard`]s —
/// i.e. `configured_cap − available_permits`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuotaSnapshot {
    pub queries_remaining: u32,
    pub writes_remaining: u32,
    pub embeds_remaining: u32,
    pub concurrent_sessions_in_use: u32,
}

/// Per-tenant quota manager. Lazy-creates a [`TenantQuota`] from
/// `defaults` the first time it sees a tenant id; explicit
/// `set_for_tenant(id, cfg)` overrides those defaults afterwards.
pub struct QuotaManager {
    defaults: QuotaConfig,
    per_tenant: DashMap<String, Arc<TenantQuota>>,
}

impl std::fmt::Debug for QuotaManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuotaManager")
            .field("defaults", &self.defaults)
            .field("tenants", &self.per_tenant.len())
            .finish()
    }
}

impl QuotaManager {
    #[must_use]
    pub fn new(defaults: QuotaConfig) -> Self {
        Self {
            defaults,
            per_tenant: DashMap::new(),
        }
    }

    /// Apply a tenant-specific override. Resets the existing
    /// buckets to the new capacity.
    pub fn set_for_tenant(&self, tenant: &str, cfg: QuotaConfig) {
        self.per_tenant
            .insert(tenant.to_owned(), Arc::new(TenantQuota::from_config(cfg)));
    }

    fn tenant(&self, tenant: &str) -> Arc<TenantQuota> {
        if let Some(t) = self.per_tenant.get(tenant) {
            return Arc::clone(t.value());
        }
        let new = Arc::new(TenantQuota::from_config(self.defaults));
        self.per_tenant
            .entry(tenant.to_owned())
            .or_insert(new)
            .clone()
    }

    /// Debit one token from the named dimension's bucket for
    /// `tenant`. Returns `Ok(())` on success or
    /// [`QuotaError::Exhausted`] (with the retry-after hint).
    pub fn try_consume(&self, tenant: &str, dim: Dimension) -> Result<(), QuotaError> {
        let t = self.tenant(tenant);
        t.bucket(dim)
            .try_consume(1)
            .map_err(|e| QuotaError::Exhausted {
                dimension: dim,
                retry_after_ms: e.retry_after_ms,
            })
    }

    /// Read the current remaining-token counts (floored to whole
    /// tokens) for `tenant`, plus the in-use session-slot count.
    /// Lazy-creates the tenant's buckets if it has never been
    /// debited — the snapshot of a brand-new tenant returns the
    /// configured caps for the three rate dimensions and `0` for
    /// the in-use sessions. Used by the `quota_get` admin tool.
    #[must_use]
    pub fn snapshot(&self, tenant: &str) -> QuotaSnapshot {
        let t = self.tenant(tenant);
        // `available()` returns f64 because the bucket refills
        // continuously; the spec exposes a whole-token count so we
        // floor.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "snapshot reports integer-floored token counts"
        )]
        let to_u32 = |x: f64| x.floor().max(0.0).min(f64::from(u32::MAX)) as u32;
        let in_use = t
            .cfg
            .concurrent_sessions
            .saturating_sub(t.sessions.available_permits() as u32);
        QuotaSnapshot {
            queries_remaining: to_u32(t.queries.available()),
            writes_remaining: to_u32(t.writes.available()),
            embeds_remaining: to_u32(t.embeds.available()),
            concurrent_sessions_in_use: in_use,
        }
    }

    /// Try to acquire a session permit without blocking. Returns
    /// `None` if `concurrent_sessions` is at the cap.
    pub fn try_acquire_session(&self, tenant: &str) -> Option<SessionGuard> {
        let t = self.tenant(tenant);
        let sem = Arc::clone(&t.sessions);
        sem.try_acquire_owned().ok().map(SessionGuard)
    }

    /// Block (asynchronously) until a session permit is available.
    pub async fn acquire_session(&self, tenant: &str) -> SessionGuard {
        let t = self.tenant(tenant);
        let sem = Arc::clone(&t.sessions);
        let permit = sem.acquire_owned().await.expect("semaphore never closes");
        SessionGuard(permit)
    }
}

/// Drop-guard for an open session. While alive, it occupies one
/// slot in the tenant's `concurrent_sessions` semaphore.
#[derive(Debug)]
pub struct SessionGuard(#[allow(dead_code)] OwnedSemaphorePermit);

/// Errors returned by [`QuotaManager`].
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum QuotaError {
    #[error("{dimension:?} quota exhausted; retry after {retry_after_ms} ms")]
    Exhausted {
        dimension: Dimension,
        retry_after_ms: u64,
    },
}

impl QuotaError {
    #[must_use]
    pub fn retry_after_ms(&self) -> u64 {
        match self {
            Self::Exhausted { retry_after_ms, .. } => *retry_after_ms,
        }
    }
}

// Convenience: convert raw `QuotaExhausted` (per-bucket) into the
// manager's `QuotaError` shape when callers reach through.
impl QuotaError {
    #[must_use]
    pub fn from_bucket(dim: Dimension, raw: QuotaExhausted) -> Self {
        Self::Exhausted {
            dimension: dim,
            retry_after_ms: raw.retry_after_ms,
        }
    }
}
