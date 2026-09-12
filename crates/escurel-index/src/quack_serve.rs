//! The per-session lifecycle for a delegated Quack data-plane session
//! (async-ops Phase 4, Path A / Half 2 — the serving-runtime integration).
//!
//! This ties the Half-2 primitives together into what escurel does when it
//! serves a delegated data step:
//!
//! 1. on a **dedicated** connection (never the single DuckLake writer — the
//!    crew's F4/R-1 concern), create the one **quarantine** result table;
//! 2. install the requester's **scoped policy** ([`crate::quack_policy`]) — the
//!    only authority the session has;
//! 3. arm the bounded-execution **watchdog** ([`crate::quack_session`]);
//! 4. start the Quack server.
//!
//! On completion or drop it reverses that: stop the server, disarm the watchdog,
//! and release the single-session slot.
//!
//! Two safety defaults the crew asked for (F4): the whole surface is **off by
//! default** (the caller passes `enabled`, sourced from `ESCUREL_QUACK_ENABLED`),
//! and **one concurrent session** per process ([`SessionSlot`]).
//!
//! The actual `CALL quack_serve(...)` / `quack_stop()` sits behind the
//! [`QuackControl`] trait, so the orchestration — DDL, policy install, the
//! lifecycle, the single-session guard, the flag — is unit-testable against a
//! real in-memory DuckDB without loading the beta extension or standing up a
//! server. The wire-level end-to-end (a client attaching, cross-tenant denials)
//! is the F7 gateway-shaped test + the live E2E.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use duckdb::Connection;

use crate::quack_policy::{PolicyError, render_policy_inserts, scoped_session_policy};
use crate::quack_session::SessionWatchdog;

/// Why a delegated session could not be opened (or a step failed).
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// The data plane is disabled (`ESCUREL_QUACK_ENABLED` off) — the default.
    #[error("delegated Quack sessions are disabled")]
    Disabled,
    /// Another delegated session already holds the single-session slot.
    #[error("a delegated session is already in progress (one at a time)")]
    Busy,
    /// The scoped policy could not be built (bad subject/object).
    #[error("scoped policy: {0}")]
    Policy(#[from] PolicyError),
    /// A DDL / policy-install / serve step failed on the connection.
    #[error("delegated session {step}: {source}")]
    Db {
        /// Which step failed, for the operator.
        step: &'static str,
        /// The underlying DuckDB error.
        #[source]
        source: duckdb::Error,
    },
}

/// The Quack server control surface — `serve`/`stop` on a connection. Behind a
/// trait so the lifecycle is testable without the beta extension; the real impl
/// issues the `quack_serve` / `quack_stop` SQL.
pub trait QuackControl: Send + Sync {
    /// Start serving on `conn` (prod: `CALL quack_serve(...)`).
    fn serve(&self, conn: &Connection) -> Result<(), duckdb::Error>;
    /// Stop serving on `conn` (prod: `CALL quack_stop()`); best-effort at teardown.
    fn stop(&self, conn: &Connection);
}

/// The production [`QuackControl`]: issues the real `quack_serve` / `quack_stop`
/// table functions the `quack` extension registers (verified on DuckDB v1.5.5).
///
/// The listen URI carries the `quack:` scheme the server requires
/// (`quack://host:port`); `token` is the pre-shared PSK a client presents on
/// `ATTACH` (the quack_oauth bearer rides the same `token` attach option and is
/// what the scoped policy actually gates). `disable_ssl` is for a trusted
/// same-host/tailnet hop; leave it false to require TLS.
#[derive(Debug, Clone)]
pub struct QuackServeControl {
    /// The `quack://host:port` the server binds.
    pub listen_uri: String,
    /// The pre-shared token clients present on attach.
    pub token: String,
    /// Accept a client connecting via a hostname other than the bind host.
    pub allow_other_hostname: bool,
    /// Serve plaintext (no TLS) — only for a trusted local/tailnet hop.
    pub disable_ssl: bool,
}

impl QuackControl for QuackServeControl {
    fn serve(&self, conn: &Connection) -> Result<(), duckdb::Error> {
        // Named args: quack_serve(<uri>, token:=, allow_other_hostname:=, disable_ssl:=).
        // A single-quote in the URI/token would break the literal; the URI is a
        // server-built `quack://host:port` and the token is a server-minted PSK,
        // so neither carries a quote — but escape defensively regardless.
        let sql = format!(
            "SELECT 1 FROM quack_serve('{}', token := '{}', allow_other_hostname := {}, disable_ssl := {});",
            self.listen_uri.replace('\'', "''"),
            self.token.replace('\'', "''"),
            self.allow_other_hostname,
            self.disable_ssl,
        );
        conn.execute_batch(&sql)
    }

    fn stop(&self, conn: &Connection) {
        let sql = format!(
            "SELECT 1 FROM quack_stop('{}');",
            self.listen_uri.replace('\'', "''")
        );
        // Best-effort: a stop failure at teardown must not mask the real outcome.
        let _ = conn.execute_batch(&sql);
    }
}

/// The single-session guard: at most one delegated session per process (F4 —
/// bounds the beta server's blast radius). Cheap, lock-free.
#[derive(Clone, Default)]
pub struct SessionSlot(Arc<AtomicBool>);

impl SessionSlot {
    /// Take the slot if free; the returned guard releases it on drop. `None`
    /// when a session already holds it.
    #[must_use]
    pub fn try_acquire(&self) -> Option<SlotGuard> {
        // Acquire iff currently false.
        if self
            .0
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            Some(SlotGuard(Arc::clone(&self.0)))
        } else {
            None
        }
    }
}

/// Releases the [`SessionSlot`] on drop.
pub struct SlotGuard(Arc<AtomicBool>);

impl Drop for SlotGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// A live delegated session. Holds the dedicated connection, the armed
/// watchdog, the server control, and the single-session slot; tearing it down
/// (drop, or [`close`](Self::close)) stops the server, disarms the watchdog and
/// frees the slot — on the happy path AND on a panic.
pub struct DelegatedSession {
    conn: Connection,
    watchdog: SessionWatchdog,
    control: Arc<dyn QuackControl>,
    _slot: SlotGuard,
}

impl DelegatedSession {
    /// The dedicated connection the session serves on (for the result read-back
    /// / seal, and tests).
    #[must_use]
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Explicit teardown (idempotent with drop): stop the server + disarm the
    /// watchdog now rather than at scope end.
    pub fn close(mut self) {
        self.teardown();
    }

    fn teardown(&mut self) {
        self.watchdog.disarm();
        self.control.stop(&self.conn);
    }
}

impl Drop for DelegatedSession {
    fn drop(&mut self) {
        self.teardown();
    }
}

impl std::fmt::Debug for DelegatedSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The connection + control aren't Debug; a marker is enough (and keeps
        // a live session out of any accidental log).
        f.write_str("DelegatedSession { .. }")
    }
}

/// Parameters for opening a delegated session.
pub struct OpenParams<'a> {
    /// The verified requester's subject (the policy binds to it).
    pub requester_subject: &'a str,
    /// The fully-qualified entitled input objects the session may `Scan`.
    pub entitled_objects: &'a [String],
    /// The single quarantine result table (`<result_schema>.<run table>`) the
    /// session may `Insert` into.
    pub result_table: &'a str,
    /// The result table's column definitions, e.g. `"id INTEGER, v VARCHAR"` —
    /// server-derived from the producer's declared schema.
    pub result_columns: &'a str,
    /// The policy table quack_oauth reads (a bounded `schema.table`).
    pub policy_table: &'a str,
    /// The bounded-execution deadline for the whole session.
    pub deadline: std::time::Duration,
}

/// Open a delegated data-plane session on `conn` (a dedicated, non-writer
/// connection), or refuse. Creates the quarantine result table, installs the
/// requester's scoped policy, arms the watchdog, and starts the server.
///
/// # Errors
/// [`SessionError::Disabled`] when the flag is off, [`SessionError::Busy`] when
/// a session already holds the slot, [`SessionError::Policy`] for a bad
/// subject/object, [`SessionError::Db`] when a DDL/install/serve step fails.
pub fn open_delegated_session(
    conn: Connection,
    enabled: bool,
    slot: &SessionSlot,
    params: &OpenParams<'_>,
    control: Arc<dyn QuackControl>,
) -> Result<DelegatedSession, SessionError> {
    if !enabled {
        return Err(SessionError::Disabled);
    }
    // Take the single-session slot BEFORE touching the connection, so a
    // rejected concurrent open changes nothing.
    let slot_guard = slot.try_acquire().ok_or(SessionError::Busy)?;

    // Build the scoped policy first — a bad subject/object fails before any DDL.
    let rows = scoped_session_policy(
        params.requester_subject,
        params.entitled_objects,
        params.result_table,
    )?;

    // 1. The one quarantine result table, in its own schema. The result_table
    //    is validated as `schema.table` by scoped_session_policy above.
    let (schema, _table) = params
        .result_table
        .split_once('.')
        .expect("scoped_session_policy validated result_table as schema.table");
    conn.execute_batch(&format!(
        "CREATE SCHEMA IF NOT EXISTS {schema}; \
         CREATE TABLE {} ({});",
        params.result_table, params.result_columns
    ))
    .map_err(|source| SessionError::Db {
        step: "create result table",
        source,
    })?;

    // 2. Install the scoped policy (the session's entire authority).
    if let Some(sql) = render_policy_inserts(&rows, params.policy_table) {
        conn.execute_batch(&sql)
            .map_err(|source| SessionError::Db {
                step: "install policy",
                source,
            })?;
    }

    // 3. Arm the watchdog on the dedicated connection.
    let watchdog = {
        let handle = conn.interrupt_handle();
        SessionWatchdog::arm(move || handle.interrupt(), params.deadline)
    };

    // 4. Start serving. On failure, teardown is automatic: the watchdog and
    //    slot guard drop here.
    if let Err(source) = control.serve(&conn) {
        return Err(SessionError::Db {
            step: "quack_serve",
            source,
        });
    }

    Ok(DelegatedSession {
        conn,
        watchdog,
        control,
        _slot: slot_guard,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::Duration;

    /// A fake server control that records serve/stop calls — the lifecycle is
    /// exercised without loading the beta extension.
    #[derive(Default)]
    struct FakeControl {
        served: AtomicBool,
        stopped: AtomicBool,
        fail_serve: bool,
        calls: Mutex<Vec<&'static str>>,
    }
    impl QuackControl for FakeControl {
        fn serve(&self, _conn: &Connection) -> Result<(), duckdb::Error> {
            self.calls.lock().unwrap().push("serve");
            if self.fail_serve {
                return Err(duckdb::Error::InvalidQuery);
            }
            self.served.store(true, Ordering::Release);
            Ok(())
        }
        fn stop(&self, _conn: &Connection) {
            self.calls.lock().unwrap().push("stop");
            self.stopped.store(true, Ordering::Release);
        }
    }

    fn objs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    fn params<'a>(entitled: &'a [String], result_table: &'a str) -> OpenParams<'a> {
        OpenParams {
            requester_subject: "google:alice@acme",
            entitled_objects: entitled,
            result_table,
            result_columns: "id INTEGER, v VARCHAR",
            policy_table: "main.policies",
            deadline: Duration::from_secs(30),
        }
    }

    fn conn_with_policy_table() -> Connection {
        let conn = Connection::open_in_memory().expect("open");
        conn.execute_batch(
            "CREATE TABLE main.policies (priority INTEGER NOT NULL, subject VARCHAR, \
             any_scope VARCHAR[], actions VARCHAR[], object_pattern VARCHAR, \
             column_pattern VARCHAR, allow BOOLEAN NOT NULL);",
        )
        .expect("policies");
        conn
    }

    #[test]
    fn disabled_refuses_and_creates_nothing() {
        let conn = conn_with_policy_table();
        let slot = SessionSlot::default();
        let entitled = objs(&["main.v"]);
        let err = open_delegated_session(
            conn,
            /*enabled=*/ false,
            &slot,
            &params(&entitled, "result_acme.res_1"),
            Arc::new(FakeControl::default()),
        )
        .expect_err("disabled");
        assert!(matches!(err, SessionError::Disabled));
        // The slot was never taken, so a later enabled open can proceed.
        assert!(slot.try_acquire().is_some());
    }

    #[test]
    fn open_creates_the_quarantine_table_installs_policy_and_serves() {
        let conn = conn_with_policy_table();
        let slot = SessionSlot::default();
        let control = Arc::new(FakeControl::default());
        let entitled = objs(&["main.vw_orders"]);
        let session = open_delegated_session(
            conn,
            true,
            &slot,
            &params(&entitled, "result_acme.res_1"),
            control.clone(),
        )
        .expect("open");

        // The quarantine result table exists and is writable.
        session
            .connection()
            .execute_batch("INSERT INTO result_acme.res_1 VALUES (1, 'ok');")
            .expect("result table writable");
        // The scoped policy was installed (subject-bound rows present).
        let n: i64 = session
            .connection()
            .query_row(
                "SELECT count(*) FROM main.policies WHERE subject = 'google:alice@acme'",
                [],
                |r| r.get(0),
            )
            .expect("policy count");
        assert_eq!(n, 2, "one Scan row + one Insert row, both subject-bound");
        assert!(control.served.load(Ordering::Acquire), "serve was called");

        // While the session is open, the slot is held.
        assert!(slot.try_acquire().is_none(), "single session: slot held");

        // Teardown stops the server and frees the slot.
        session.close();
        assert!(control.stopped.load(Ordering::Acquire), "stop was called");
        assert!(slot.try_acquire().is_some(), "slot freed after close");
    }

    #[test]
    fn a_second_concurrent_session_is_refused_busy() {
        let slot = SessionSlot::default();
        let entitled = objs(&["main.v"]);
        let first = open_delegated_session(
            conn_with_policy_table(),
            true,
            &slot,
            &params(&entitled, "result_acme.res_1"),
            Arc::new(FakeControl::default()),
        )
        .expect("first opens");

        let err = open_delegated_session(
            conn_with_policy_table(),
            true,
            &slot,
            &params(&entitled, "result_acme.res_2"),
            Arc::new(FakeControl::default()),
        )
        .expect_err("second refused");
        assert!(matches!(err, SessionError::Busy));

        drop(first); // frees the slot
        // Now a new session can open.
        assert!(
            open_delegated_session(
                conn_with_policy_table(),
                true,
                &slot,
                &params(&entitled, "result_acme.res_3"),
                Arc::new(FakeControl::default()),
            )
            .is_ok()
        );
    }

    #[test]
    fn a_bad_entitled_object_fails_before_any_ddl_and_frees_the_slot() {
        let slot = SessionSlot::default();
        let entitled = objs(&["not qualified"]);
        let err = open_delegated_session(
            conn_with_policy_table(),
            true,
            &slot,
            &params(&entitled, "result_acme.res_1"),
            Arc::new(FakeControl::default()),
        )
        .expect_err("bad object");
        assert!(matches!(err, SessionError::Policy(_)));
        // The slot guard dropped on the early return — a good open now works.
        assert!(slot.try_acquire().is_some());
    }

    #[test]
    fn a_serve_failure_tears_down_and_frees_the_slot() {
        let slot = SessionSlot::default();
        let entitled = objs(&["main.v"]);
        let control = Arc::new(FakeControl {
            fail_serve: true,
            ..Default::default()
        });
        let err = open_delegated_session(
            conn_with_policy_table(),
            true,
            &slot,
            &params(&entitled, "result_acme.res_1"),
            control,
        )
        .expect_err("serve fails");
        assert!(matches!(
            err,
            SessionError::Db {
                step: "quack_serve",
                ..
            }
        ));
        // Watchdog + slot dropped on the error path.
        assert!(
            slot.try_acquire().is_some(),
            "slot freed after serve failure"
        );
    }

    /// The REAL `QuackServeControl` against the real `quack` extension: a
    /// serve/stop round-trip on a live connection. `#[ignore]` — it INSTALLs
    /// the extension over the network and binds a local port. Verified the
    /// `quack_serve('quack://host:port', token:=…, …)` / `quack_stop(uri)`
    /// signature on DuckDB v1.5.5.
    #[test]
    #[ignore = "network install + binds a local port; the real quack serve/stop round-trip"]
    fn quack_serve_control_serves_and_stops_for_real() {
        let conn = Connection::open_in_memory().expect("open");
        conn.execute_batch("INSTALL quack; LOAD quack;")
            .expect("load quack");
        // An ephemeral-ish high port for the test bind.
        let control = QuackServeControl {
            listen_uri: "quack://127.0.0.1:47837".to_owned(),
            token: "test-psk".to_owned(),
            allow_other_hostname: true,
            disable_ssl: true,
        };
        // serve then stop must both succeed on a live connection.
        control.serve(&conn).expect("quack_serve starts");
        control.stop(&conn); // best-effort; a second stop is harmless
        // A re-serve on the same URI after stop proves the port was released.
        control.serve(&conn).expect("re-serve after stop");
        control.stop(&conn);
    }
}
