//! The runner-local **run ledger** + the idempotency half of the
//! loop-control gate (#149).
//!
//! [`docs/contract/agent-orchestration.md`](https://github.com/DataZooDE/escurel/blob/main/docs/contract/agent-orchestration.md)
//! §Architecture item 4 calls for a *runner-local durable store — its own
//! SQLite/DuckDB file, **not** the tenant store* — with one row per run,
//! the basis of every later loop control. §Lifecycle step 4 (the
//! loop-control gate) opens with idempotency: "drop if `event_id` already
//! terminal (idempotency); drop if in-flight … else create `run_id`, write
//! `pending`."
//!
//! This module is the **durable authority** for that idempotency. It is a
//! thin embedded SQLite store (via `rusqlite` with the `bundled` feature,
//! so it carries its own SQLite and drags none of escurel-index's DuckDB
//! into the independent runner). A unique constraint on
//! `(tenant, event_id)` makes "exactly one run per event" a database
//! invariant rather than an application convention, so two racing triggers
//! for the same event yield exactly one [`LedgerDecision::Created`].
//!
//! The full depth/budget/cycle controls (#157) and cascade lineage (#156)
//! are out of scope here; the schema carries `depth` + `root_event_id`
//! columns now (minimal hooks) so those work-items extend the ledger
//! rather than migrate it.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::Connection;

use crate::Trigger;

/// Terminal/in-flight status of a run row.
///
/// `pending` is the only non-terminal state: a row is created `pending` and
/// later moved to a terminal state by [`Ledger::mark`]. `processed` and
/// `dead` are the terminal states that make a `(tenant, event_id)`
/// idempotent (a re-delivery is dropped); `failed` is terminal-for-this-run
/// but a retry policy (#157) may revive it, so it is treated like the other
/// terminal states for the idempotency gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    /// Run created, not yet reconciled. The in-flight state.
    Pending,
    /// Run completed successfully (the agent's write was confirmed).
    Processed,
    /// Run failed (may be retried by a later policy).
    Failed,
    /// Run dead-lettered (terminal; will not be retried).
    Dead,
}

impl RunStatus {
    /// The wire/DB string for this status.
    pub fn as_str(self) -> &'static str {
        match self {
            RunStatus::Pending => "pending",
            RunStatus::Processed => "processed",
            RunStatus::Failed => "failed",
            RunStatus::Dead => "dead",
        }
    }

    /// Parse a DB string back into a [`RunStatus`].
    fn from_str(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(RunStatus::Pending),
            "processed" => Some(RunStatus::Processed),
            "failed" => Some(RunStatus::Failed),
            "dead" => Some(RunStatus::Dead),
            _ => None,
        }
    }

    /// Whether this status is terminal (the run has finished). A terminal
    /// row makes its `(tenant, event_id)` idempotent.
    fn is_terminal(self) -> bool {
        matches!(
            self,
            RunStatus::Processed | RunStatus::Failed | RunStatus::Dead
        )
    }
}

/// A newtype over a run's identifier (a ULID string).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RunId(pub String);

impl RunId {
    /// Mint a fresh, monotonic-ish run id.
    fn new() -> Self {
        RunId(ulid::Ulid::new().to_string())
    }

    /// Borrow the underlying string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RunId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Outcome of [`Ledger::begin_run`] — the idempotency decision at the
/// dispatch gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedgerDecision {
    /// No prior run for `(tenant, event_id)`; a fresh `pending` row was
    /// inserted. The caller proceeds to enqueue. Carries the new run id.
    Created(RunId),
    /// A run for `(tenant, event_id)` already exists in a **terminal**
    /// state (`processed`/`failed`/`dead`). The trigger is a re-delivery of
    /// an already-handled event → drop (idempotency).
    AlreadyTerminal,
    /// A run for `(tenant, event_id)` exists and is still `pending`
    /// (in-flight). The trigger is a concurrent/overlapping delivery →
    /// drop (dedup).
    InFlight,
}

/// Errors raised by the run ledger.
#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    /// The underlying SQLite store returned an error.
    #[error("run ledger sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// A run id referenced by [`Ledger::mark`] was not found.
    #[error("run ledger: run {0} not found")]
    NotFound(String),
}

/// A snapshot of one run row, for introspection (`get_run`, the runner's
/// `/debug/ledger` surface).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRecord {
    /// The run's id.
    pub run_id: String,
    /// Owning tenant.
    pub tenant: String,
    /// The triggering event id.
    pub event_id: String,
    /// The target instance page, if the event was already assigned.
    pub instance_page_id: Option<String>,
    /// Content hash for `(instance, content-hash)` dedup (#157); `None` for
    /// now.
    pub content_hash: Option<String>,
    /// Current status.
    pub status: RunStatus,
    /// Cascade depth (`0` for a webhook-origin trigger).
    pub depth: u32,
    /// The event at the root of this cascade.
    pub root_event_id: String,
}

/// The runner-local durable run ledger.
///
/// Backed by a single SQLite file (its own — never the tenant store). The
/// connection is wrapped in a [`Mutex`] so the `Ledger` is `Send + Sync`
/// and safe to share (e.g. behind an `Arc`) across the webhook handler and
/// the poller; SQLite itself serialises writes, and the unique constraint
/// is the real concurrency guard.
#[derive(Debug)]
pub struct Ledger {
    conn: Mutex<Connection>,
}

impl Ledger {
    /// Open (creating + migrating if needed) the ledger at `path`.
    ///
    /// The schema is idempotent (`CREATE TABLE IF NOT EXISTS`), so
    /// re-opening an existing file is a no-op migration — that is what
    /// gives the ledger its survive-a-restart durability.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, LedgerError> {
        let conn = Connection::open(path)?;
        Self::migrate(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Open an in-memory ledger (tests for non-persistence cases only — the
    /// durable behaviour must be tested against a real file).
    #[cfg(test)]
    fn open_in_memory() -> Result<Self, LedgerError> {
        let conn = Connection::open_in_memory()?;
        Self::migrate(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn migrate(conn: &Connection) -> Result<(), LedgerError> {
        // WAL keeps readers from blocking the single writer; a busy_timeout
        // makes the rare lock contention wait rather than error.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS runs (
                 run_id           TEXT PRIMARY KEY,
                 tenant           TEXT NOT NULL,
                 event_id         TEXT NOT NULL,
                 instance_page_id TEXT,
                 content_hash     TEXT,
                 status           TEXT NOT NULL,
                 depth            INTEGER NOT NULL DEFAULT 0,
                 root_event_id    TEXT NOT NULL,
                 created_at       TEXT NOT NULL,
                 updated_at       TEXT NOT NULL
             );
             CREATE UNIQUE INDEX IF NOT EXISTS runs_tenant_event
                 ON runs (tenant, event_id);",
        )?;
        Ok(())
    }

    /// The idempotency gate. Atomically:
    ///
    /// - If a run for `(tenant, event_id)` already exists in a terminal
    ///   state → [`LedgerDecision::AlreadyTerminal`].
    /// - Else if one exists `pending` → [`LedgerDecision::InFlight`].
    /// - Else insert a fresh `pending` row → [`LedgerDecision::Created`].
    ///
    /// Concurrency-safe: the insert uses `ON CONFLICT(tenant, event_id) DO
    /// NOTHING`, so two racing callers both attempt the insert but only one
    /// changes a row — that one returns `Created`, the loser re-reads the
    /// winner's row and returns `InFlight`/`AlreadyTerminal`. The whole
    /// check-then-insert runs in one `IMMEDIATE` transaction so the read and
    /// the write see a consistent snapshot.
    pub fn begin_run(&self, trigger: &Trigger) -> Result<LedgerDecision, LedgerError> {
        let mut conn = self.conn.lock().expect("run ledger mutex");
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

        // Fast path: a row already exists.
        if let Some(status) = lookup_status(&tx, &trigger.tenant, &trigger.event_id)? {
            tx.commit()?;
            return Ok(if status.is_terminal() {
                LedgerDecision::AlreadyTerminal
            } else {
                LedgerDecision::InFlight
            });
        }

        // No row yet: try to claim it. ON CONFLICT DO NOTHING means a
        // racing winner who committed between our read and this insert wins;
        // we detect that by the changed-row count.
        let run_id = RunId::new();
        let now = now_iso();
        let changed = tx.execute(
            "INSERT INTO runs
                 (run_id, tenant, event_id, instance_page_id, content_hash,
                  status, depth, root_event_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, NULL, 'pending', ?5, ?6, ?7, ?7)
             ON CONFLICT(tenant, event_id) DO NOTHING",
            rusqlite::params![
                run_id.as_str(),
                trigger.tenant,
                trigger.event_id,
                trigger.instance_page_id,
                trigger.lineage.depth,
                trigger.lineage.root_event_id,
                now,
            ],
        )?;

        if changed == 1 {
            tx.commit()?;
            return Ok(LedgerDecision::Created(run_id));
        }

        // Lost the race: someone else inserted between our read and our
        // insert. Re-read their row to classify the drop.
        let status =
            lookup_status(&tx, &trigger.tenant, &trigger.event_id)?.unwrap_or(RunStatus::Pending);
        tx.commit()?;
        Ok(if status.is_terminal() {
            LedgerDecision::AlreadyTerminal
        } else {
            LedgerDecision::InFlight
        })
    }

    /// Move a run to a terminal `status` and bump `updated_at`.
    pub fn mark(&self, run_id: &RunId, status: RunStatus) -> Result<(), LedgerError> {
        let conn = self.conn.lock().expect("run ledger mutex");
        let changed = conn.execute(
            "UPDATE runs SET status = ?1, updated_at = ?2 WHERE run_id = ?3",
            rusqlite::params![status.as_str(), now_iso(), run_id.as_str()],
        )?;
        if changed == 0 {
            return Err(LedgerError::NotFound(run_id.0.clone()));
        }
        Ok(())
    }

    /// Fetch a run row by `(tenant, event_id)`, if present.
    pub fn get_run(&self, tenant: &str, event_id: &str) -> Result<Option<RunRecord>, LedgerError> {
        let conn = self.conn.lock().expect("run ledger mutex");
        let mut stmt = conn.prepare(
            "SELECT run_id, tenant, event_id, instance_page_id, content_hash,
                    status, depth, root_event_id
             FROM runs WHERE tenant = ?1 AND event_id = ?2",
        )?;
        let row = stmt
            .query_row(rusqlite::params![tenant, event_id], |r| {
                let status_str: String = r.get(5)?;
                Ok(RunRecord {
                    run_id: r.get(0)?,
                    tenant: r.get(1)?,
                    event_id: r.get(2)?,
                    instance_page_id: r.get(3)?,
                    content_hash: r.get(4)?,
                    status: RunStatus::from_str(&status_str).unwrap_or(RunStatus::Pending),
                    depth: r.get(6)?,
                    root_event_id: r.get(7)?,
                })
            })
            .ok();
        Ok(row)
    }

    /// Count rows in a given status — used by the runner's `/debug/ledger`
    /// surface and the no-mock integration test ("exactly one terminal run
    /// row").
    pub fn count_by_status(&self, tenant: &str, status: RunStatus) -> Result<u64, LedgerError> {
        let conn = self.conn.lock().expect("run ledger mutex");
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM runs WHERE tenant = ?1 AND status = ?2",
            rusqlite::params![tenant, status.as_str()],
            |r| r.get(0),
        )?;
        Ok(n as u64)
    }

    /// Total run rows for a tenant (any status). Backs the integration
    /// test's "exactly one run row" assertion.
    pub fn count_runs(&self, tenant: &str) -> Result<u64, LedgerError> {
        let conn = self.conn.lock().expect("run ledger mutex");
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM runs WHERE tenant = ?1",
            rusqlite::params![tenant],
            |r| r.get(0),
        )?;
        Ok(n as u64)
    }

    /// Total run rows across all tenants (any status). The single-tenant
    /// runner uses this for its tenant-agnostic `/debug/ledger` surface.
    pub fn count_all_runs(&self) -> Result<u64, LedgerError> {
        let conn = self.conn.lock().expect("run ledger mutex");
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM runs", [], |r| r.get(0))?;
        Ok(n as u64)
    }

    /// Run rows across all tenants in a given status. Pairs with
    /// [`Self::count_all_runs`] for the `/debug/ledger` surface.
    pub fn count_all_by_status(&self, status: RunStatus) -> Result<u64, LedgerError> {
        let conn = self.conn.lock().expect("run ledger mutex");
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM runs WHERE status = ?1",
            rusqlite::params![status.as_str()],
            |r| r.get(0),
        )?;
        Ok(n as u64)
    }
}

/// Look up the status of a `(tenant, event_id)` row within a transaction.
fn lookup_status(
    conn: &Connection,
    tenant: &str,
    event_id: &str,
) -> Result<Option<RunStatus>, LedgerError> {
    let mut stmt = conn.prepare("SELECT status FROM runs WHERE tenant = ?1 AND event_id = ?2")?;
    let status: Option<String> = stmt
        .query_row(rusqlite::params![tenant, event_id], |r| r.get(0))
        .ok();
    Ok(status.and_then(|s| RunStatus::from_str(&s)))
}

/// An ISO-8601-ish UTC timestamp (seconds + nanos since the epoch as a
/// sortable string). We avoid pulling `chrono` into the lean runner core;
/// the ledger only needs a monotone, sortable created/updated marker.
fn now_iso() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}.{:09}", now.as_secs(), now.subsec_nanos())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Lineage;

    fn trigger(event_id: &str) -> Trigger {
        Trigger {
            tenant: "acme".to_owned(),
            event_id: event_id.to_owned(),
            label_skill: "note".to_owned(),
            instance_page_id: None,
            lineage: Lineage::root(event_id),
        }
    }

    /// The #149 core DoD, against a **real SQLite file in a tempdir** (not
    /// `:memory:`, so the persistence case is genuine):
    /// - `begin_run(A)` → Created; mark processed; `begin_run(A)` →
    ///   AlreadyTerminal (idempotency).
    /// - `begin_run(B)` → Created (distinct event).
    /// - drop + re-open the SAME file → `begin_run(A)` still AlreadyTerminal
    ///   (durable across a process restart).
    #[test]
    fn idempotency_survives_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("ledger.sqlite");

        let run_a = {
            let ledger = Ledger::open(&path).expect("open ledger");

            let decision = ledger.begin_run(&trigger("A")).expect("begin A");
            let run_a = match decision {
                LedgerDecision::Created(id) => id,
                other => panic!("first begin_run(A) must Create, got {other:?}"),
            };

            // In-flight: a second begin_run while A is still pending drops.
            assert_eq!(
                ledger.begin_run(&trigger("A")).expect("begin A again"),
                LedgerDecision::InFlight,
                "a pending run must report InFlight, not create a duplicate"
            );

            ledger.mark(&run_a, RunStatus::Processed).expect("mark A");

            // Idempotency: now terminal, a re-delivery drops as AlreadyTerminal.
            assert_eq!(
                ledger
                    .begin_run(&trigger("A"))
                    .expect("begin A post-terminal"),
                LedgerDecision::AlreadyTerminal,
                "a terminal run must report AlreadyTerminal (idempotency)"
            );

            // A distinct event still creates.
            assert!(
                matches!(
                    ledger.begin_run(&trigger("B")).expect("begin B"),
                    LedgerDecision::Created(_)
                ),
                "a distinct event must Create its own run"
            );

            run_a
        }; // <- Ledger dropped here, closing the SQLite connection.
        let _ = run_a;

        // Re-open the SAME file: the terminal A row must persist.
        let reopened = Ledger::open(&path).expect("re-open ledger");
        assert_eq!(
            reopened
                .begin_run(&trigger("A"))
                .expect("begin A after reopen"),
            LedgerDecision::AlreadyTerminal,
            "the terminal A row must survive a process restart (durable)"
        );
        // Exactly one run row per event id.
        assert_eq!(reopened.count_runs("acme").expect("count"), 2);
    }

    /// Two racing `begin_run(A)` calls must yield exactly one `Created`.
    #[test]
    fn concurrent_begin_run_creates_exactly_one() {
        use std::sync::Arc;
        use std::thread;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("race.sqlite");
        let ledger = Arc::new(Ledger::open(&path).expect("open ledger"));

        let n = 8;
        let barrier = Arc::new(std::sync::Barrier::new(n));
        let handles: Vec<_> = (0..n)
            .map(|_| {
                let ledger = Arc::clone(&ledger);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    ledger.begin_run(&trigger("A")).expect("begin race")
                })
            })
            .collect();

        let created = handles
            .into_iter()
            .map(|h| h.join().expect("join"))
            .filter(|d| matches!(d, LedgerDecision::Created(_)))
            .count();
        assert_eq!(
            created, 1,
            "exactly one racing begin_run must Create; the rest must drop"
        );
        assert_eq!(ledger.count_runs("acme").expect("count"), 1);
    }

    #[test]
    fn mark_unknown_run_is_not_found() {
        let ledger = Ledger::open_in_memory().expect("open");
        let err = ledger
            .mark(&RunId("nope".to_owned()), RunStatus::Processed)
            .expect_err("marking a missing run must error");
        assert!(matches!(err, LedgerError::NotFound(_)));
    }

    #[test]
    fn get_run_round_trips_fields() {
        let ledger = Ledger::open_in_memory().expect("open");
        let mut t = trigger("C");
        t.instance_page_id = Some("inst-9".to_owned());
        let _ = ledger.begin_run(&t).expect("begin C");
        let rec = ledger.get_run("acme", "C").expect("get").expect("present");
        assert_eq!(rec.event_id, "C");
        assert_eq!(rec.instance_page_id, Some("inst-9".to_owned()));
        assert_eq!(rec.status, RunStatus::Pending);
        assert_eq!(rec.root_event_id, "C");
    }
}
