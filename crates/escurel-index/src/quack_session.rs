//! Bounded execution for a delegated Quack data-plane session (async-ops
//! Phase 4, Path A / Half 2 — crew finding F4).
//!
//! DuckDB has no statement timeout. A delegated session — a beta Quack server
//! driving a connection — that runs a runaway query (`SELECT * FROM a, a, a`, a
//! slow `read_csv('http://slowloris/')`) would otherwise stall that connection
//! indefinitely; on the gateway that is a tenant-to-tenant availability event
//! (the crew's R-1 blast radius). The only bound DuckDB offers is
//! `Connection::interrupt`, so this arms a watchdog that calls it on a
//! wall-clock cap and, either way, is torn down when the session ends.
//!
//! It is deliberately a small, standalone primitive: the watchdog is armed with
//! an interrupt *callback* — the caller captures the connection's
//! `interrupt_handle()` (a `Send + Sync` handle) in a closure — so the watchdog
//! never has to name or own the connection. Success path: call
//! [`SessionWatchdog::disarm`] (or just drop it) before the deadline and the
//! interrupt never fires. Failure path: the deadline elapses, the callback fires
//! once, and the in-flight statement aborts.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How often the watchdog thread wakes to check the cancel flag. Small enough
/// that a disarm (the success path) joins promptly, large enough not to spin.
const TICK: Duration = Duration::from_millis(25);

/// A wall-clock guard that calls `interrupt()` on a DuckDB connection if a
/// delegated session outruns its deadline, and tears the guard down when the
/// session ends (explicit [`disarm`](Self::disarm), or `Drop`).
pub struct SessionWatchdog {
    cancel: Arc<AtomicBool>,
    fired: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl SessionWatchdog {
    /// Arm a watchdog that runs `interrupt` once `deadline` elapses, unless
    /// disarmed/dropped first. The caller builds the callback from the session's
    /// connection, e.g. `{ let h = conn.interrupt_handle(); move || h.interrupt() }`.
    #[must_use]
    pub fn arm<F>(interrupt: F, deadline: Duration) -> Self
    where
        F: Fn() + Send + 'static,
    {
        let cancel = Arc::new(AtomicBool::new(false));
        let fired = Arc::new(AtomicBool::new(false));
        let join = {
            let cancel = Arc::clone(&cancel);
            let fired = Arc::clone(&fired);
            std::thread::spawn(move || {
                let start = Instant::now();
                loop {
                    if cancel.load(Ordering::Acquire) {
                        return;
                    }
                    if start.elapsed() >= deadline {
                        // Interrupt exactly once, then exit. A second session on
                        // the connection is a separate concern (the deploy caps
                        // concurrency to one delegated session).
                        interrupt();
                        fired.store(true, Ordering::Release);
                        return;
                    }
                    std::thread::sleep(TICK.min(deadline));
                }
            })
        };
        Self {
            cancel,
            fired,
            join: Some(join),
        }
    }

    /// Cancel the watchdog (the session finished within its budget) and join the
    /// thread. Idempotent; also run by `Drop`.
    pub fn disarm(&mut self) {
        self.cancel.store(true, Ordering::Release);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }

    /// Whether the watchdog actually fired an interrupt (the deadline elapsed
    /// before disarm). Meaningful after [`disarm`](Self::disarm)/drop.
    #[must_use]
    pub fn fired(&self) -> bool {
        self.fired.load(Ordering::Acquire)
    }
}

impl Drop for SessionWatchdog {
    fn drop(&mut self) {
        // Teardown on both the happy path and a panic/early-return: never leave a
        // watchdog thread outliving the session.
        self.disarm();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use duckdb::Connection;

    #[test]
    fn a_runaway_query_is_interrupted_at_the_deadline() {
        let conn = Connection::open_in_memory().expect("open");
        let mut wd = SessionWatchdog::arm(
            {
                let h = conn.interrupt_handle();
                move || h.interrupt()
            },
            Duration::from_millis(150),
        );

        // A query that would run for many seconds unbounded: a large cross
        // join aggregated. The watchdog must abort it well before it finishes.
        let started = Instant::now();
        let res: Result<i64, _> = conn.query_row(
            "SELECT count(*) FROM range(100000000) a, range(100000) b",
            [],
            |r| r.get(0),
        );
        let elapsed = started.elapsed();

        assert!(
            res.is_err(),
            "the runaway query must be interrupted, not complete"
        );
        assert!(
            elapsed < Duration::from_secs(10),
            "interrupt must fire near the deadline, took {elapsed:?}"
        );
        wd.disarm();
        assert!(wd.fired(), "the watchdog should record that it fired");
    }

    #[test]
    fn a_fast_query_completes_and_the_watchdog_never_fires() {
        let conn = Connection::open_in_memory().expect("open");
        let mut wd = SessionWatchdog::arm(
            {
                let h = conn.interrupt_handle();
                move || h.interrupt()
            },
            Duration::from_secs(30),
        );

        let n: i64 = conn
            .query_row("SELECT count(*) FROM range(1000)", [], |r| r.get(0))
            .expect("fast query completes");
        assert_eq!(n, 1000);

        wd.disarm();
        assert!(
            !wd.fired(),
            "a session within budget must not be interrupted"
        );
    }

    #[test]
    fn drop_tears_down_the_watchdog_thread() {
        // Arming and dropping without an explicit disarm must not leak the
        // thread or fire late — Drop joins it.
        let conn = Connection::open_in_memory().expect("open");
        let wd = SessionWatchdog::arm(
            {
                let h = conn.interrupt_handle();
                move || h.interrupt()
            },
            Duration::from_secs(30),
        );
        drop(wd);
        // A subsequent query on the same connection runs cleanly (no stray
        // interrupt from the dropped watchdog).
        let n: i64 = conn
            .query_row("SELECT 42", [], |r| r.get(0))
            .expect("query after drop");
        assert_eq!(n, 42);
    }
}
