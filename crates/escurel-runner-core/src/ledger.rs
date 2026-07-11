//! The runner-local **run ledger** + the idempotency half of the
//! loop-control gate (#149), extended with the depth/budget/cycle
//! dead-lettering controls (#157).
//!
//! ## Terminal-vs-retriable matrix (#149 / #155 / #157)
//!
//! The ledger distinguishes two flavours of "finished":
//!
//! | status        | idempotency-terminal? | meaning                                  |
//! |---------------|-----------------------|------------------------------------------|
//! | `pending`     | no (in-flight)        | created, not yet reconciled              |
//! | `processed`   | **yes**               | confirmed success — never re-run         |
//! | `dead_letter` | **yes**               | depth/cycle/budget block — never re-run  |
//! | `failed`      | **no (retriable)**    | transient exhaustion — an operator       |
//! |               |                       | re-drive / the poller backstop MAY       |
//! |               |                       | re-attempt it                            |
//!
//! The #149 gate originally treated `failed` as terminal-for-idempotency,
//! which permanently wedged an event that merely failed *transiently*. #155
//! gave failures a retry policy; #157 finishes the job: only `processed` and
//! `dead_letter` are genuinely terminal, so a `failed` row no longer blocks a
//! re-delivery (the poller re-pulls the still-`inbox` event, and `begin_run`
//! re-claims the failed row rather than reporting `AlreadyTerminal`). A
//! `dead_letter` IS terminal — a depth/cycle/budget block is a deliberate,
//! permanent decision the operator must DLQ-requeue to override.
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

/// Terminal/in-flight status of a run row. See the module docs for the full
/// terminal-vs-retriable matrix.
///
/// Only `processed` and `dead_letter` are **idempotency-terminal** (a
/// re-delivery of their `(tenant, event_id)` is dropped). `pending` is
/// in-flight; `failed` is *retriable* — a transient exhaustion that an
/// operator re-drive or the poller backstop may re-attempt, so it does NOT
/// wedge the event at the idempotency gate (#157 reconciles the #149
/// failed-terminal rough edge).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    /// Run created, not yet reconciled. The in-flight state.
    Pending,
    /// Run completed successfully (the agent's write was confirmed).
    Processed,
    /// Run failed transiently (exhausted retries). **Retriable** — not
    /// idempotency-terminal, so a re-drive can re-attempt it.
    Failed,
    /// Run dead-lettered by a loop control (depth/cycle/budget) — a
    /// deliberate, permanent block. Idempotency-terminal; only a DLQ
    /// requeue overrides it. Carries a `reason` (see [`DeadLetterReason`]).
    DeadLetter,
}

/// The reason a run was dead-lettered by a loop control (#157). Recorded in
/// the ledger's `reason` column and read back via `/debug/run`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeadLetterReason {
    /// `trigger.lineage.depth` exceeded `ESCUREL_RUNNER_MAX_DEPTH`.
    DepthExceeded,
    /// The candidate instance is already in the lineage's instance chain —
    /// admitting the run would close a cascade cycle.
    Cycle,
    /// The per-root run budget (`ESCUREL_RUNNER_MAX_RUNS_PER_ROOT`) is spent.
    BudgetExceeded,
    /// The reconciler exhausted its retry-attempts cap on a transient failure
    /// (#158). A terminal dead-letter — the operator must DLQ-requeue to
    /// re-attempt — rather than a bare retriable `failed`.
    RetriesExhausted,
    /// The harness produced output the adapter could not parse (#158). A
    /// re-run won't fix a broken harness contract, so it dead-letters.
    BadOutput,
}

impl DeadLetterReason {
    /// The wire/DB string for this reason.
    pub fn as_str(self) -> &'static str {
        match self {
            DeadLetterReason::DepthExceeded => "depth_exceeded",
            DeadLetterReason::Cycle => "cycle",
            DeadLetterReason::BudgetExceeded => "budget_exceeded",
            DeadLetterReason::RetriesExhausted => "retries_exhausted",
            DeadLetterReason::BadOutput => "bad_output",
        }
    }
}

impl std::fmt::Display for DeadLetterReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl RunStatus {
    /// The wire/DB string for this status.
    pub fn as_str(self) -> &'static str {
        match self {
            RunStatus::Pending => "pending",
            RunStatus::Processed => "processed",
            RunStatus::Failed => "failed",
            RunStatus::DeadLetter => "dead_letter",
        }
    }

    /// Parse a DB string back into a [`RunStatus`]. The legacy `dead` value
    /// (pre-#157) maps to [`RunStatus::DeadLetter`] for forward-compat.
    fn from_str(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(RunStatus::Pending),
            "processed" => Some(RunStatus::Processed),
            "failed" => Some(RunStatus::Failed),
            "dead_letter" | "dead" => Some(RunStatus::DeadLetter),
            _ => None,
        }
    }

    /// Whether this status is **idempotency-terminal** — a row in this state
    /// makes its `(tenant, event_id)` idempotent (a re-delivery is dropped).
    /// `failed` is deliberately NOT terminal here (it is retriable); see the
    /// module-level matrix.
    fn is_terminal(self) -> bool {
        matches!(self, RunStatus::Processed | RunStatus::DeadLetter)
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
    /// The instance page the completed run produced, read back from `/mcp`
    /// by the reconciler (#155). `None` until the run completes (and for a
    /// run that produced no instance write). Distinct from
    /// [`Self::instance_page_id`], which is the *pre-flagged* target carried
    /// by the trigger — `produced_instance_page_id` is the *confirmed*
    /// instance the effect actually landed on.
    pub produced_instance_page_id: Option<String>,
    /// The confirmed version of [`Self::produced_instance_page_id`] after the
    /// run, read back via `expand` (#155). `None` until completion.
    pub produced_version: Option<String>,
    /// Current status.
    pub status: RunStatus,
    /// Cascade depth (`0` for a webhook-origin trigger).
    pub depth: u32,
    /// The event at the root of this cascade.
    pub root_event_id: String,
    /// The dead-letter reason (`depth_exceeded` / `cycle` /
    /// `budget_exceeded`), when [`Self::status`] is
    /// [`RunStatus::DeadLetter`]; `None` otherwise.
    pub reason: Option<String>,
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
    pub(crate) fn open_in_memory() -> Result<Self, LedgerError> {
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
        // #155 read-back columns: the confirmed instance + version the run
        // produced. Added with `ALTER TABLE … ADD COLUMN` (ignoring the
        // "duplicate column" error on re-open) so an existing ledger file
        // migrates in place rather than needing a rebuild.
        add_column_if_missing(conn, "produced_instance_page_id")?;
        add_column_if_missing(conn, "produced_version")?;
        // #157 dead-letter reason: `depth_exceeded` / `cycle` /
        // `budget_exceeded` for a run blocked by a loop control. Added in
        // place via `ALTER TABLE` so an existing ledger file migrates rather
        // than rebuilds.
        add_column_if_missing(conn, "reason")?;
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
            match status {
                // Idempotency-terminal: a confirmed success or a deliberate
                // dead-letter — drop the re-delivery.
                RunStatus::Processed | RunStatus::DeadLetter => {
                    tx.commit()?;
                    return Ok(LedgerDecision::AlreadyTerminal);
                }
                // Retriable: a prior attempt failed transiently. Re-claim the
                // row (reset to `pending`, mint a fresh run id) so a re-drive /
                // poller backstop re-attempts it rather than the event wedging
                // forever (#157 reconciles the #149 failed-terminal edge).
                RunStatus::Failed => {
                    let run_id = RunId::new();
                    tx.execute(
                        "UPDATE runs
                            SET run_id = ?1, status = 'pending',
                                produced_instance_page_id = NULL,
                                produced_version = NULL, reason = NULL,
                                updated_at = ?2
                          WHERE tenant = ?3 AND event_id = ?4",
                        rusqlite::params![
                            run_id.as_str(),
                            now_iso(),
                            trigger.tenant,
                            trigger.event_id,
                        ],
                    )?;
                    tx.commit()?;
                    return Ok(LedgerDecision::Created(run_id));
                }
                // Still pending: a concurrent/overlapping delivery — drop.
                RunStatus::Pending => {
                    tx.commit()?;
                    return Ok(LedgerDecision::InFlight);
                }
            }
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

    /// Complete a run: move it to a terminal `status` **and** record the
    /// produced instance + its confirmed version (#155). The reconciler calls
    /// this on success with the read-back `(instance, version)`; on a failed
    /// run `produced` is `None` and only the status moves. Idempotent in the
    /// sense that re-completing overwrites the same terminal facts.
    pub fn complete(
        &self,
        run_id: &RunId,
        status: RunStatus,
        produced: Option<(&str, &str)>,
    ) -> Result<(), LedgerError> {
        let (instance, version) = match produced {
            Some((i, v)) => (Some(i), Some(v)),
            None => (None, None),
        };
        let conn = self.conn.lock().expect("run ledger mutex");
        let changed = conn.execute(
            "UPDATE runs
                SET status = ?1,
                    produced_instance_page_id = ?2,
                    produced_version = ?3,
                    updated_at = ?4
              WHERE run_id = ?5",
            rusqlite::params![
                status.as_str(),
                instance,
                version,
                now_iso(),
                run_id.as_str()
            ],
        )?;
        if changed == 0 {
            return Err(LedgerError::NotFound(run_id.0.clone()));
        }
        Ok(())
    }

    /// Dead-letter a run: move it to [`RunStatus::DeadLetter`] and record the
    /// loop-control `reason` (#157). Terminal and deliberate — only a DLQ
    /// requeue overrides it.
    pub fn dead_letter(&self, run_id: &RunId, reason: DeadLetterReason) -> Result<(), LedgerError> {
        let conn = self.conn.lock().expect("run ledger mutex");
        let changed = conn.execute(
            "UPDATE runs SET status = ?1, reason = ?2, updated_at = ?3 WHERE run_id = ?4",
            rusqlite::params![
                RunStatus::DeadLetter.as_str(),
                reason.as_str(),
                now_iso(),
                run_id.as_str()
            ],
        )?;
        if changed == 0 {
            return Err(LedgerError::NotFound(run_id.0.clone()));
        }
        Ok(())
    }

    /// Count the runs sharing a `root_event_id` for a tenant — the per-root
    /// run budget the loop-control gate debits (#157). Counts every row in the
    /// cascade tree, in any status, so a runaway chain is caught regardless of
    /// where its hops landed.
    pub fn count_runs_for_root(
        &self,
        tenant: &str,
        root_event_id: &str,
    ) -> Result<u64, LedgerError> {
        let conn = self.conn.lock().expect("run ledger mutex");
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM runs WHERE tenant = ?1 AND root_event_id = ?2",
            rusqlite::params![tenant, root_event_id],
            |r| r.get(0),
        )?;
        Ok(n as u64)
    }

    /// Fetch a run row by `(tenant, event_id)`, if present.
    pub fn get_run(&self, tenant: &str, event_id: &str) -> Result<Option<RunRecord>, LedgerError> {
        let conn = self.conn.lock().expect("run ledger mutex");
        let mut stmt = conn.prepare(
            "SELECT run_id, tenant, event_id, instance_page_id, content_hash,
                    produced_instance_page_id, produced_version,
                    status, depth, root_event_id, reason
             FROM runs WHERE tenant = ?1 AND event_id = ?2",
        )?;
        let row = stmt
            .query_row(rusqlite::params![tenant, event_id], |r| {
                let status_str: String = r.get(7)?;
                Ok(RunRecord {
                    run_id: r.get(0)?,
                    tenant: r.get(1)?,
                    event_id: r.get(2)?,
                    instance_page_id: r.get(3)?,
                    content_hash: r.get(4)?,
                    produced_instance_page_id: r.get(5)?,
                    produced_version: r.get(6)?,
                    status: RunStatus::from_str(&status_str).unwrap_or(RunStatus::Pending),
                    depth: r.get(8)?,
                    root_event_id: r.get(9)?,
                    reason: r.get(10)?,
                })
            })
            .ok();
        Ok(row)
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

    /// List every dead-lettered run, newest first — the operator DLQ surface
    /// (#158). Each entry carries the run id, tenant, originating event, the
    /// produced instance (if any), and the dead-letter reason, so an operator
    /// can triage and re-drive.
    pub fn list_dead_letters(&self) -> Result<Vec<RunRecord>, LedgerError> {
        let conn = self.conn.lock().expect("run ledger mutex");
        let mut stmt = conn.prepare(
            "SELECT run_id, tenant, event_id, instance_page_id, content_hash,
                    produced_instance_page_id, produced_version,
                    status, depth, root_event_id, reason
             FROM runs WHERE status = ?1
             ORDER BY updated_at DESC",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![RunStatus::DeadLetter.as_str()], |r| {
                let status_str: String = r.get(7)?;
                Ok(RunRecord {
                    run_id: r.get(0)?,
                    tenant: r.get(1)?,
                    event_id: r.get(2)?,
                    instance_page_id: r.get(3)?,
                    content_hash: r.get(4)?,
                    produced_instance_page_id: r.get(5)?,
                    produced_version: r.get(6)?,
                    status: RunStatus::from_str(&status_str).unwrap_or(RunStatus::DeadLetter),
                    depth: r.get(8)?,
                    root_event_id: r.get(9)?,
                    reason: r.get(10)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Clear a dead-lettered run's terminal block so its originating event can
    /// be re-driven — the DLQ **requeue** path (#158). The row is reset to
    /// `pending` with a fresh `run_id` (and its prior produced/reason fields
    /// cleared), so the next `begin_run` for that `(tenant, event_id)` no longer
    /// reports `AlreadyTerminal`. Looks the run up by `run_id`; returns the
    /// `(tenant, event_id)` it cleared so the caller can re-enqueue a trigger.
    ///
    /// Only a `dead_letter` row is requeued — a `processed`/`pending`/`failed`
    /// row is left untouched and reported via [`LedgerError::NotFound`] (the
    /// operator requeues a dead-letter, nothing else).
    pub fn requeue_dead_letter(&self, run_id: &str) -> Result<(String, String), LedgerError> {
        let conn = self.conn.lock().expect("run ledger mutex");
        let (tenant, event_id): (String, String) = conn
            .query_row(
                "SELECT tenant, event_id FROM runs WHERE run_id = ?1 AND status = ?2",
                rusqlite::params![run_id, RunStatus::DeadLetter.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|_| LedgerError::NotFound(run_id.to_owned()))?;
        let fresh = RunId::new();
        conn.execute(
            "UPDATE runs
                SET run_id = ?1, status = 'pending',
                    produced_instance_page_id = NULL,
                    produced_version = NULL, reason = NULL,
                    updated_at = ?2
              WHERE run_id = ?3",
            rusqlite::params![fresh.as_str(), now_iso(), run_id],
        )?;
        Ok((tenant, event_id))
    }

    /// Requeue a dead-lettered run found by its originating `(tenant,
    /// event_id)` (the operator may hold the event id, not the run id). Same
    /// semantics as [`Self::requeue_dead_letter`].
    pub fn requeue_dead_letter_by_event(
        &self,
        tenant: &str,
        event_id: &str,
    ) -> Result<String, LedgerError> {
        let conn = self.conn.lock().expect("run ledger mutex");
        let _existing: String = conn
            .query_row(
                "SELECT run_id FROM runs
                  WHERE tenant = ?1 AND event_id = ?2 AND status = ?3",
                rusqlite::params![tenant, event_id, RunStatus::DeadLetter.as_str()],
                |r| r.get(0),
            )
            .map_err(|_| LedgerError::NotFound(format!("{tenant}/{event_id}")))?;
        let fresh = RunId::new();
        conn.execute(
            "UPDATE runs
                SET run_id = ?1, status = 'pending',
                    produced_instance_page_id = NULL,
                    produced_version = NULL, reason = NULL,
                    updated_at = ?2
              WHERE tenant = ?3 AND event_id = ?4",
            rusqlite::params![fresh.as_str(), now_iso(), tenant, event_id],
        )?;
        Ok(fresh.0)
    }

    /// Every `pending` (in-flight) run across all tenants — the basis of
    /// crash recovery (#158). On restart the runner re-confirms each of these
    /// by read-back: a landed effect is marked `processed`, an unconfirmed one
    /// is reset to retriable so the poller backstops it.
    pub fn list_pending(&self) -> Result<Vec<RunRecord>, LedgerError> {
        let conn = self.conn.lock().expect("run ledger mutex");
        let mut stmt = conn.prepare(
            "SELECT run_id, tenant, event_id, instance_page_id, content_hash,
                    produced_instance_page_id, produced_version,
                    status, depth, root_event_id, reason
             FROM runs WHERE status = ?1",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![RunStatus::Pending.as_str()], |r| {
                let status_str: String = r.get(7)?;
                Ok(RunRecord {
                    run_id: r.get(0)?,
                    tenant: r.get(1)?,
                    event_id: r.get(2)?,
                    instance_page_id: r.get(3)?,
                    content_hash: r.get(4)?,
                    produced_instance_page_id: r.get(5)?,
                    produced_version: r.get(6)?,
                    status: RunStatus::from_str(&status_str).unwrap_or(RunStatus::Pending),
                    depth: r.get(8)?,
                    root_event_id: r.get(9)?,
                    reason: r.get(10)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

/// Add a nullable `TEXT` column to `runs` if it is not already present.
/// SQLite has no `ADD COLUMN IF NOT EXISTS`, so we attempt the `ALTER` and
/// treat the "duplicate column name" error as a successful no-op — that is
/// what makes re-opening an already-migrated ledger file idempotent.
fn add_column_if_missing(conn: &Connection, column: &str) -> Result<(), LedgerError> {
    let sql = format!("ALTER TABLE runs ADD COLUMN {column} TEXT");
    match conn.execute(&sql, []) {
        Ok(_) => Ok(()),
        Err(rusqlite::Error::SqliteFailure(_, Some(msg))) if msg.contains("duplicate column") => {
            Ok(())
        }
        Err(e) => Err(LedgerError::Sqlite(e)),
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
            workflow: None,
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
    fn complete_records_produced_instance_and_version() {
        // A completed run stores the read-back instance + version alongside
        // the terminal status, surviving a re-open of the real sqlite file.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("complete.sqlite");
        let run_id = {
            let ledger = Ledger::open(&path).expect("open");
            let id = match ledger.begin_run(&trigger("D")).expect("begin D") {
                LedgerDecision::Created(id) => id,
                other => panic!("expected Created, got {other:?}"),
            };
            ledger
                .complete(&id, RunStatus::Processed, Some(("inst-7", "sha256:abc")))
                .expect("complete D");
            id
        };
        let _ = run_id;

        let reopened = Ledger::open(&path).expect("re-open");
        let rec = reopened
            .get_run("acme", "D")
            .expect("get")
            .expect("present");
        assert_eq!(rec.status, RunStatus::Processed);
        assert_eq!(rec.produced_instance_page_id, Some("inst-7".to_owned()));
        assert_eq!(rec.produced_version, Some("sha256:abc".to_owned()));
        assert_eq!(
            reopened
                .count_all_by_status(RunStatus::Processed)
                .expect("count"),
            1
        );
    }

    #[test]
    fn complete_failed_leaves_produced_fields_null() {
        let ledger = Ledger::open_in_memory().expect("open");
        let id = match ledger.begin_run(&trigger("E")).expect("begin E") {
            LedgerDecision::Created(id) => id,
            other => panic!("expected Created, got {other:?}"),
        };
        ledger
            .complete(&id, RunStatus::Failed, None)
            .expect("complete failed");
        let rec = ledger.get_run("acme", "E").expect("get").expect("present");
        assert_eq!(rec.status, RunStatus::Failed);
        assert_eq!(rec.produced_instance_page_id, None);
        assert_eq!(rec.produced_version, None);
    }

    #[test]
    fn dead_letter_is_terminal_and_records_reason() {
        // A dead-lettered run is idempotency-terminal (re-delivery dropped) and
        // exposes its loop-control reason.
        let ledger = Ledger::open_in_memory().expect("open");
        let id = match ledger.begin_run(&trigger("DL")).expect("begin DL") {
            LedgerDecision::Created(id) => id,
            other => panic!("expected Created, got {other:?}"),
        };
        ledger
            .dead_letter(&id, DeadLetterReason::Cycle)
            .expect("dead-letter DL");
        let rec = ledger.get_run("acme", "DL").expect("get").expect("present");
        assert_eq!(rec.status, RunStatus::DeadLetter);
        assert_eq!(rec.reason.as_deref(), Some("cycle"));
        assert_eq!(
            ledger.begin_run(&trigger("DL")).expect("re-begin DL"),
            LedgerDecision::AlreadyTerminal,
            "a dead-lettered run is terminal — a re-delivery must drop"
        );
    }

    #[test]
    fn failed_run_is_retriable_not_wedged() {
        // #157 reconciles the #149 failed-terminal edge: a transient `failed`
        // run must be re-claimable (Created with a fresh run id), NOT reported
        // AlreadyTerminal — otherwise an operator re-drive / poller backstop
        // could never re-attempt it.
        let ledger = Ledger::open_in_memory().expect("open");
        let first = match ledger.begin_run(&trigger("FR")).expect("begin FR") {
            LedgerDecision::Created(id) => id,
            other => panic!("expected Created, got {other:?}"),
        };
        ledger
            .complete(&first, RunStatus::Failed, None)
            .expect("fail FR");
        let second = ledger.begin_run(&trigger("FR")).expect("re-begin FR");
        match second {
            LedgerDecision::Created(id) => assert_ne!(
                id.as_str(),
                first.as_str(),
                "a re-claimed failed run mints a fresh run id"
            ),
            other => panic!("a failed run must be re-claimable (Created), got {other:?}"),
        }
        // Still exactly one row for the event (re-claim updates in place).
        assert_eq!(ledger.count_runs("acme").expect("count"), 1);
    }

    #[test]
    fn dlq_list_and_requeue_round_trip() {
        // A dead-lettered run is listed in the DLQ; requeueing it clears the
        // terminal block so a re-delivery Creates again (re-driveable).
        let ledger = Ledger::open_in_memory().expect("open");
        let id = match ledger.begin_run(&trigger("DLQ1")).expect("begin") {
            LedgerDecision::Created(id) => id,
            other => panic!("expected Created, got {other:?}"),
        };
        ledger
            .dead_letter(&id, DeadLetterReason::RetriesExhausted)
            .expect("dead-letter");

        let listed = ledger.list_dead_letters().expect("list dlq");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].event_id, "DLQ1");
        assert_eq!(listed[0].reason.as_deref(), Some("retries_exhausted"));

        // Re-delivery of a dead-lettered event drops (terminal) until requeued.
        assert_eq!(
            ledger.begin_run(&trigger("DLQ1")).expect("re-begin"),
            LedgerDecision::AlreadyTerminal,
        );

        let (tenant, event_id) = ledger.requeue_dead_letter(id.as_str()).expect("requeue");
        assert_eq!((tenant.as_str(), event_id.as_str()), ("acme", "DLQ1"));
        assert!(ledger.list_dead_letters().expect("list").is_empty());
        // Requeue resets the row to `pending` (re-driveable) with a fresh run
        // id; the operator path re-enqueues a trigger directly, so a later
        // begin sees it in-flight (no longer the terminal AlreadyTerminal that
        // wedged it). It is back on the runnable path.
        let rec = ledger
            .get_run("acme", "DLQ1")
            .expect("get")
            .expect("present");
        assert_eq!(rec.status, RunStatus::Pending);
        assert_eq!(rec.reason, None, "requeue clears the dead-letter reason");
        assert_ne!(rec.run_id, id.0, "requeue mints a fresh run id");
        assert_eq!(
            ledger
                .begin_run(&trigger("DLQ1"))
                .expect("re-begin post-requeue"),
            LedgerDecision::InFlight,
            "a requeued (pending) row is in-flight, no longer terminal"
        );
    }

    #[test]
    fn requeue_by_event_only_targets_dead_letters() {
        let ledger = Ledger::open_in_memory().expect("open");
        let id = match ledger.begin_run(&trigger("DLQ2")).expect("begin") {
            LedgerDecision::Created(id) => id,
            other => panic!("expected Created, got {other:?}"),
        };
        // A pending (not dead-lettered) run is not requeueable.
        assert!(ledger.requeue_dead_letter_by_event("acme", "DLQ2").is_err());
        ledger
            .dead_letter(&id, DeadLetterReason::BadOutput)
            .expect("dead-letter");
        let fresh = ledger
            .requeue_dead_letter_by_event("acme", "DLQ2")
            .expect("requeue by event");
        assert_ne!(fresh, id.0, "requeue mints a fresh run id");
        let rec = ledger
            .get_run("acme", "DLQ2")
            .expect("get")
            .expect("present");
        assert_eq!(rec.status, RunStatus::Pending);
    }

    #[test]
    fn list_pending_finds_in_flight_rows() {
        let ledger = Ledger::open_in_memory().expect("open");
        let _ = ledger.begin_run(&trigger("P1")).expect("begin P1");
        let p2 = match ledger.begin_run(&trigger("P2")).expect("begin P2") {
            LedgerDecision::Created(id) => id,
            other => panic!("expected Created, got {other:?}"),
        };
        ledger
            .complete(&p2, RunStatus::Processed, None)
            .expect("complete P2");
        let pending = ledger.list_pending().expect("list pending");
        assert_eq!(pending.len(), 1, "only the in-flight row is pending");
        assert_eq!(pending[0].event_id, "P1");
    }

    #[test]
    fn count_runs_for_root_counts_the_cascade_tree() {
        let ledger = Ledger::open_in_memory().expect("open");
        // Two hops sharing a root, plus an unrelated root.
        let mut hop0 = trigger("R0");
        hop0.lineage = Lineage::root("R0");
        let mut hop1 = trigger("R0-HOP1");
        hop1.lineage = Lineage {
            root_event_id: "R0".into(),
            depth: 1,
            lineage_path: vec!["R0".into(), "R0-HOP1".into()],
            instance_path: vec![],
            trace_id: None,
        };
        let other = trigger("R9");
        let _ = ledger.begin_run(&hop0).expect("hop0");
        let _ = ledger.begin_run(&hop1).expect("hop1");
        let _ = ledger.begin_run(&other).expect("other");
        assert_eq!(
            ledger.count_runs_for_root("acme", "R0").expect("count"),
            2,
            "both hops sharing root R0 are counted"
        );
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
