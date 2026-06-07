//! Per-tenant + global run quotas (#158).
//!
//! The runner subprocesses are heavy, and a runaway tenant (or a cascade that
//! the loop controls have not yet stopped) must not starve everyone else. The
//! [`Governor`] is the focused unit that bounds run admission, sitting at the
//! dispatch gate alongside [`crate::admit`]:
//!
//! 1. **Per-tenant runs/min** — a fixed one-minute window counting *admitted*
//!    runs per tenant. Once `runs_per_min` is reached in the current window,
//!    further triggers are **throttled** (held, not dead-lettered) until the
//!    window rolls.
//! 2. **Per-tenant max-concurrent** — a live count of in-flight runs per
//!    tenant. While `max_concurrent` runs are executing, a new trigger is
//!    throttled.
//! 3. **Global harness-subprocess cap** — a process-wide [`tokio::sync::Semaphore`]
//!    bounding how many harness subprocesses spawn concurrently across *all*
//!    tenants (subprocesses are the heaviest resource). The dispatch loop
//!    acquires a permit immediately before spawning the harness and releases it
//!    when the run finishes.
//!
//! Throttling is **not** a dead-letter: an over-quota trigger is held back so
//! the originating event stays in the inbox and the poller backstops it on the
//! next cycle (or a delayed re-enqueue re-drives it). Every throttle is
//! observable — the governor tallies a per-reason counter the runner logs and
//! exposes.
//!
//! The two per-tenant gates are simple in-memory counters behind a `Mutex`
//! (the runner is single-process); the global cap is the async semaphore so
//! the spawning task can *await* a permit rather than busy-wait.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Why a trigger was throttled by the [`Governor`]. Recorded so throttling is
/// observable (a counter the runner exposes + logs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThrottleReason {
    /// The tenant's per-minute admitted-run budget is spent for this window.
    RunsPerMin,
    /// The tenant already has its maximum concurrent runs in flight.
    MaxConcurrent,
}

impl ThrottleReason {
    /// A short, stable label for logs/metrics.
    pub fn as_str(self) -> &'static str {
        match self {
            ThrottleReason::RunsPerMin => "runs_per_min",
            ThrottleReason::MaxConcurrent => "max_concurrent",
        }
    }
}

/// The admission decision the quota gate returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaDecision {
    /// The run may proceed; the caller holds a [`RunSlot`] for its lifetime.
    Admit,
    /// The run is over quota and must be held (not run now, not dead-lettered).
    Throttle(ThrottleReason),
}

/// Static quota limits, lifted from `RunnerConfig`.
#[derive(Debug, Clone, Copy)]
pub struct QuotaLimits {
    /// Maximum admitted runs per tenant within a one-minute window.
    pub runs_per_min: u64,
    /// Maximum concurrent in-flight runs per tenant.
    pub max_concurrent: u64,
    /// Maximum concurrent harness subprocesses across all tenants.
    pub max_harness_procs: usize,
}

/// Per-tenant mutable counters: the current fixed-minute window + the live
/// in-flight count.
#[derive(Debug)]
struct TenantState {
    /// Start of the current rate window.
    window_start: Instant,
    /// Admitted runs in the current window.
    window_count: u64,
    /// Runs currently in flight for this tenant.
    in_flight: u64,
}

impl TenantState {
    fn new(now: Instant) -> Self {
        Self {
            window_start: now,
            window_count: 0,
            in_flight: 0,
        }
    }
}

/// The quota governor: per-tenant rate + concurrency gates plus the shared
/// global harness-subprocess semaphore. Cheap to clone (`Arc`-backed); share
/// one instance across the dispatch loop and the gate.
#[derive(Debug, Clone)]
pub struct Governor {
    limits: QuotaLimits,
    tenants: Arc<Mutex<HashMap<String, TenantState>>>,
    harness_sem: Arc<Semaphore>,
    throttled_runs_per_min: Arc<AtomicU64>,
    throttled_max_concurrent: Arc<AtomicU64>,
}

/// A held run slot. Decrements the tenant's in-flight count on drop, so a
/// concurrency permit is released exactly once the run (success, failure, or
/// panic) completes. Acquire a harness permit from the same governor via
/// [`Governor::acquire_harness`] for the subprocess-spawn window.
#[derive(Debug)]
pub struct RunSlot {
    tenant: String,
    tenants: Arc<Mutex<HashMap<String, TenantState>>>,
}

impl Drop for RunSlot {
    fn drop(&mut self) {
        if let Ok(mut map) = self.tenants.lock()
            && let Some(state) = map.get_mut(&self.tenant)
        {
            state.in_flight = state.in_flight.saturating_sub(1);
        }
    }
}

const WINDOW: Duration = Duration::from_secs(60);

impl Governor {
    /// Build a governor with the given limits. `max_harness_procs` is clamped
    /// to at least `1` (a zero cap would wedge every run).
    pub fn new(limits: QuotaLimits) -> Self {
        Self {
            limits,
            tenants: Arc::new(Mutex::new(HashMap::new())),
            harness_sem: Arc::new(Semaphore::new(limits.max_harness_procs.max(1))),
            throttled_runs_per_min: Arc::new(AtomicU64::new(0)),
            throttled_max_concurrent: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Try to admit a run for `tenant`. On [`QuotaDecision::Admit`] the
    /// returned [`RunSlot`] (held for the run's lifetime) keeps the tenant's
    /// in-flight count debited until it drops. On a throttle the slot is
    /// `None`, the per-reason counter is bumped, and the caller holds the
    /// trigger back (the event stays in the inbox; the poller backstops it).
    pub fn try_admit(&self, tenant: &str) -> (QuotaDecision, Option<RunSlot>) {
        self.try_admit_at(tenant, Instant::now())
    }

    /// [`Self::try_admit`] with an injectable clock (tests roll the window
    /// without sleeping a real minute).
    pub fn try_admit_at(&self, tenant: &str, now: Instant) -> (QuotaDecision, Option<RunSlot>) {
        let mut map = self.tenants.lock().expect("governor mutex");
        let state = map
            .entry(tenant.to_owned())
            .or_insert_with(|| TenantState::new(now));

        // Roll the fixed window if it has elapsed.
        if now.duration_since(state.window_start) >= WINDOW {
            state.window_start = now;
            state.window_count = 0;
        }

        // Concurrency gate first (the harder, live limit).
        if state.in_flight >= self.limits.max_concurrent {
            drop(map);
            self.throttled_max_concurrent
                .fetch_add(1, Ordering::Relaxed);
            return (QuotaDecision::Throttle(ThrottleReason::MaxConcurrent), None);
        }

        // Rate gate.
        if state.window_count >= self.limits.runs_per_min {
            drop(map);
            self.throttled_runs_per_min.fetch_add(1, Ordering::Relaxed);
            return (QuotaDecision::Throttle(ThrottleReason::RunsPerMin), None);
        }

        state.window_count += 1;
        state.in_flight += 1;
        (
            QuotaDecision::Admit,
            Some(RunSlot {
                tenant: tenant.to_owned(),
                tenants: Arc::clone(&self.tenants),
            }),
        )
    }

    /// Await a global harness-subprocess permit. The returned permit must be
    /// held across the harness `spawn`+`wait`; dropping it frees a global slot.
    /// Returns `None` only if the semaphore was closed (never, in practice).
    pub async fn acquire_harness(&self) -> Option<OwnedSemaphorePermit> {
        Arc::clone(&self.harness_sem).acquire_owned().await.ok()
    }

    /// Available global harness permits right now (queue-pressure observability).
    pub fn harness_permits_available(&self) -> usize {
        self.harness_sem.available_permits()
    }

    /// Total throttles by reason since start (for logs/metrics/tests).
    pub fn throttled(&self, reason: ThrottleReason) -> u64 {
        match reason {
            ThrottleReason::RunsPerMin => self.throttled_runs_per_min.load(Ordering::Relaxed),
            ThrottleReason::MaxConcurrent => self.throttled_max_concurrent.load(Ordering::Relaxed),
        }
    }

    /// Total throttles across all reasons.
    pub fn throttled_total(&self) -> u64 {
        self.throttled(ThrottleReason::RunsPerMin) + self.throttled(ThrottleReason::MaxConcurrent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits(runs_per_min: u64, max_concurrent: u64, max_harness_procs: usize) -> QuotaLimits {
        QuotaLimits {
            runs_per_min,
            max_concurrent,
            max_harness_procs,
        }
    }

    #[test]
    fn runs_per_min_throttles_over_the_window_budget() {
        let gov = Governor::new(limits(2, 100, 4));
        let t0 = Instant::now();
        // Two admit; hold the slots so concurrency is not the limiter.
        let (d1, s1) = gov.try_admit_at("acme", t0);
        let (d2, s2) = gov.try_admit_at("acme", t0);
        assert_eq!(d1, QuotaDecision::Admit);
        assert_eq!(d2, QuotaDecision::Admit);
        // Third in the same window is throttled on the rate.
        let (d3, s3) = gov.try_admit_at("acme", t0);
        assert_eq!(d3, QuotaDecision::Throttle(ThrottleReason::RunsPerMin));
        assert!(s3.is_none());
        assert_eq!(gov.throttled(ThrottleReason::RunsPerMin), 1);
        drop((s1, s2));

        // After the window rolls, the rate budget refreshes.
        let t1 = t0 + WINDOW + Duration::from_millis(1);
        let (d4, _s4) = gov.try_admit_at("acme", t1);
        assert_eq!(d4, QuotaDecision::Admit, "window roll refreshes the budget");
    }

    #[test]
    fn max_concurrent_throttles_until_a_slot_drops() {
        let gov = Governor::new(limits(1000, 1, 4));
        let t0 = Instant::now();
        let (d1, s1) = gov.try_admit_at("acme", t0);
        assert_eq!(d1, QuotaDecision::Admit);
        let (d2, s2) = gov.try_admit_at("acme", t0);
        assert_eq!(d2, QuotaDecision::Throttle(ThrottleReason::MaxConcurrent));
        assert!(s2.is_none());
        // Drop the held slot → the in-flight count frees up.
        drop(s1);
        let (d3, _s3) = gov.try_admit_at("acme", t0);
        assert_eq!(d3, QuotaDecision::Admit, "a freed slot re-admits");
    }

    #[test]
    fn tenants_are_isolated() {
        let gov = Governor::new(limits(1, 100, 4));
        let t0 = Instant::now();
        let (a, _sa) = gov.try_admit_at("acme", t0);
        let (b, _sb) = gov.try_admit_at("beta", t0);
        assert_eq!(a, QuotaDecision::Admit);
        assert_eq!(
            b,
            QuotaDecision::Admit,
            "a second tenant has its own budget"
        );
    }

    #[tokio::test]
    async fn global_harness_semaphore_bounds_concurrent_spawns() {
        let gov = Governor::new(limits(1000, 1000, 2));
        let p1 = gov.acquire_harness().await.expect("permit 1");
        let _p2 = gov.acquire_harness().await.expect("permit 2");
        assert_eq!(gov.harness_permits_available(), 0, "cap of 2 is exhausted");
        // A third acquire would block; prove it by checking try after a drop.
        drop(p1);
        assert_eq!(
            gov.harness_permits_available(),
            1,
            "a dropped permit frees a slot"
        );
    }
}
