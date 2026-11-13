//! The live runs and what may be done to one while it runs (knowledge-
//! workbench backend P2-3a, #820).
//!
//! A run is live from the moment the dispatch loop claims it until its
//! terminal. The loop registers it in the [`RunRegistry`], keyed by run id;
//! one entry per run carries everything a control verb acts on:
//!
//! - **cancel** — the run's [`Cancel`], handed to the harness via
//!   [`crate::TaskContext::cancel`]; a subprocess harness stops its child on
//!   it (SIGTERM, then [`Cancel::grace`], then SIGKILL) and an in-process one
//!   checks it between turns. The requester's reason reaches the run's
//!   `run-finished`.
//! - **inject** — a human's message for the live harness session, when the
//!   harness can take one (`Harness::supports_inject`). The entry holds the
//!   sending half; the harness reads the [`InjectInbox`] it gets via
//!   [`crate::TaskContext::inject`]. Each injection is numbered per run.
//!
//! Acting on a run that is not live is a plain answer, never an error: the
//! ledger's terminal already stands, and there is nothing to act on.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio_util::sync::CancellationToken;

use crate::RunEventCtx;

/// The default wait between SIGTERM and SIGKILL when a run is cancelled.
pub const DEFAULT_CANCEL_GRACE: Duration = Duration::from_secs(5);

/// A run's cancel handle: the token the harness watches and the grace it
/// gives its child between SIGTERM and SIGKILL.
#[derive(Clone, Debug)]
pub struct Cancel {
    token: CancellationToken,
    /// How long the harness waits after SIGTERM before SIGKILL.
    pub grace: Duration,
}

impl Cancel {
    #[must_use]
    pub fn new(grace: Duration) -> Self {
        Self {
            token: CancellationToken::new(),
            grace,
        }
    }

    pub fn cancel(&self) {
        self.token.cancel();
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }

    /// Resolves once cancelled; pends forever otherwise.
    pub async fn cancelled(&self) {
        self.token.cancelled().await;
    }
}

impl Default for Cancel {
    fn default() -> Self {
        Self::new(DEFAULT_CANCEL_GRACE)
    }
}

/// The harness side of a run's injections: the messages a human sent the
/// live session, in order. Shared across the run's attempts; each attempt
/// reads it through one [`InjectSession`].
///
/// The inbox accepts messages until a session closes it — the harness's
/// last turn ended and its stdin is gone — so a message that could no longer
/// reach the session is answered `not_live` instead of being queued for
/// nobody. Opening the next attempt's session accepts again; until the first
/// session opens, messages wait for it.
#[derive(Clone)]
pub struct InjectInbox {
    rx: Arc<tokio::sync::Mutex<UnboundedReceiver<String>>>,
    accepting: Arc<Mutex<bool>>,
}

/// The sending half of an [`InjectInbox`].
#[derive(Clone)]
pub struct InjectSender {
    tx: UnboundedSender<String>,
    accepting: Arc<Mutex<bool>>,
}

impl InjectSender {
    /// Queue `text` for the session; `false` when no session will read it.
    #[must_use]
    pub fn send(&self, text: &str) -> bool {
        // Held across the send: a session closing concurrently either sees
        // this message in its drain or makes this send refuse.
        let accepting = self.accepting.lock().expect("inject gate");
        *accepting && self.tx.send(text.to_owned()).is_ok()
    }
}

impl InjectInbox {
    /// A new inbox and the sender that feeds it.
    #[must_use]
    pub fn channel() -> (InjectSender, Self) {
        let (tx, rx) = unbounded_channel();
        let accepting = Arc::new(Mutex::new(true));
        (
            InjectSender {
                tx,
                accepting: accepting.clone(),
            },
            Self {
                rx: Arc::new(tokio::sync::Mutex::new(rx)),
                accepting,
            },
        )
    }

    /// Hold the inbox for one harness attempt; it accepts messages until
    /// the session is closed or dropped.
    pub async fn open_session(&self) -> InjectSession<'_> {
        let rx = self.rx.lock().await;
        *self.accepting.lock().expect("inject gate") = true;
        InjectSession {
            rx,
            accepting: &self.accepting,
        }
    }
}

impl std::fmt::Debug for InjectInbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("InjectInbox")
    }
}

/// One attempt's hold on an [`InjectInbox`].
pub struct InjectSession<'a> {
    rx: tokio::sync::MutexGuard<'a, UnboundedReceiver<String>>,
    accepting: &'a Mutex<bool>,
}

impl InjectSession<'_> {
    /// The next message; pends until one arrives.
    pub async fn recv(&mut self) -> Option<String> {
        self.rx.recv().await
    }

    /// Stop accepting, and return the messages accepted before the close
    /// that were not read yet — each was answered `injected`, so the caller
    /// still owes it to the session.
    pub fn close(&mut self) -> Vec<String> {
        let mut accepting = self.accepting.lock().expect("inject gate");
        *accepting = false;
        let mut late = Vec::new();
        while let Ok(text) = self.rx.try_recv() {
            late.push(text);
        }
        late
    }
}

impl Drop for InjectSession<'_> {
    fn drop(&mut self) {
        if let Ok(mut accepting) = self.accepting.lock() {
            *accepting = false;
        }
    }
}

/// What [`RunRegistry::inject`] did with a message.
#[derive(Debug)]
pub enum Injection {
    /// Queued for the live session as the run's `n`th injection (1-based).
    /// `run` describes the run, for the event that records it.
    Queued { n: u32, run: Box<RunEventCtx> },
    /// The run is live, but its harness cannot take a message mid-run.
    Unsupported { harness: String },
    /// No such run is live.
    NotLive,
}

struct Entry {
    cancel: Cancel,
    reason: Option<String>,
    /// Who the run is, once its harness is chosen.
    run: Option<RunEventCtx>,
    /// The sending half of the run's [`InjectInbox`]; `None` for a harness
    /// that cannot take one.
    inject: Option<InjectSender>,
    injected: u32,
}

/// The live runs, keyed by run id. Cheap to clone (shared).
#[derive(Clone, Default)]
pub struct RunRegistry {
    inner: Arc<Mutex<HashMap<String, Entry>>>,
}

impl RunRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn entries(&self) -> std::sync::MutexGuard<'_, HashMap<String, Entry>> {
        self.inner.lock().expect("run registry mutex")
    }

    /// Register a run as live and get its cancel handle. Re-registering a
    /// run id replaces the entry (a re-claimed run is a new run).
    pub fn register(&self, run_id: &str, grace: Duration) -> Cancel {
        let cancel = Cancel::new(grace);
        self.entries().insert(
            run_id.to_owned(),
            Entry {
                cancel: cancel.clone(),
                reason: None,
                run: None,
                inject: None,
                injected: 0,
            },
        );
        cancel
    }

    /// Record which harness runs `run_id` (`run`), and open its inbox when
    /// that harness can take injections. `None` when it cannot, or when the
    /// run is not live.
    pub fn attach(
        &self,
        run_id: &str,
        run: RunEventCtx,
        supports_inject: bool,
    ) -> Option<InjectInbox> {
        let mut map = self.entries();
        let entry = map.get_mut(run_id)?;
        entry.run = Some(run);
        if !supports_inject {
            return None;
        }
        let (tx, inbox) = InjectInbox::channel();
        entry.inject = Some(tx);
        Some(inbox)
    }

    /// Cancel a live run; `false` when no such run is live. The first
    /// reason wins — a second cancel of the same run changes nothing.
    pub fn cancel(&self, run_id: &str, reason: Option<&str>) -> bool {
        let mut map = self.entries();
        match map.get_mut(run_id) {
            Some(entry) => {
                if entry.reason.is_none() {
                    entry.reason = reason.map(str::to_owned);
                }
                entry.cancel.cancel();
                true
            }
            None => false,
        }
    }

    /// Send `text` to a live run's harness session.
    pub fn inject(&self, run_id: &str, text: &str) -> Injection {
        let mut map = self.entries();
        let Some(entry) = map.get_mut(run_id) else {
            return Injection::NotLive;
        };
        let (Some(tx), Some(run)) = (&entry.inject, &entry.run) else {
            return Injection::Unsupported {
                harness: entry
                    .run
                    .as_ref()
                    .map_or_else(|| "unknown".to_owned(), |r| r.harness.clone()),
            };
        };
        if !tx.send(text) {
            // The session's last turn has ended: nothing would read it.
            return Injection::NotLive;
        }
        entry.injected += 1;
        Injection::Queued {
            n: entry.injected,
            run: Box::new(run.clone()),
        }
    }

    /// Forget a run at its terminal, returning the cancel reason if it was
    /// cancelled (`Some(None)` = cancelled without a reason).
    pub fn finish(&self, run_id: &str) -> Option<Option<String>> {
        let entry = self.entries().remove(run_id)?;
        entry.cancel.is_cancelled().then_some(entry.reason)
    }

    /// The live run ids.
    #[must_use]
    pub fn live(&self) -> Vec<String> {
        self.entries().keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_registered_run_is_cancelled_once_with_the_first_reason() {
        let reg = RunRegistry::new();
        let c = reg.register("r1", Duration::from_millis(10));
        assert!(!c.is_cancelled());
        assert!(reg.cancel("r1", Some("first")));
        assert!(reg.cancel("r1", Some("second")));
        assert!(c.is_cancelled());
        assert_eq!(reg.finish("r1"), Some(Some("first".into())));
        assert_eq!(reg.finish("r1"), None, "forgotten at its terminal");
    }

    #[test]
    fn a_run_that_is_not_live_cannot_be_cancelled_and_finishes_uncancelled() {
        let reg = RunRegistry::new();
        assert!(!reg.cancel("ghost", None));
        reg.register("r2", Duration::from_millis(10));
        assert_eq!(reg.live(), vec!["r2".to_owned()]);
        assert_eq!(reg.finish("r2"), None, "ran to its own terminal");
    }

    fn ctx(run_id: &str, harness: &str) -> RunEventCtx {
        RunEventCtx {
            run_id: run_id.to_owned(),
            root_event_id: "root".to_owned(),
            trigger_event_id: "ev".to_owned(),
            parent_run_id: None,
            depth: 0,
            lineage_path: Vec::new(),
            trace_id: None,
            harness: harness.to_owned(),
            model: None,
            max_attempts: 1,
            target_page_id: None,
            manual: None,
            skill: None,
            agent: None,
        }
    }

    #[tokio::test]
    async fn injections_reach_the_inbox_in_order_and_are_numbered_per_run() {
        let reg = RunRegistry::new();
        reg.register("r1", Duration::from_millis(10));
        let inbox = reg.attach("r1", ctx("r1", "claude"), true).expect("inbox");
        for (want, text) in [(1, "first"), (2, "second")] {
            match reg.inject("r1", text) {
                Injection::Queued { n, run } => {
                    assert_eq!(n, want);
                    assert_eq!(run.run_id, "r1");
                }
                other => panic!("{other:?}"),
            }
        }
        let mut session = inbox.open_session().await;
        assert_eq!(session.recv().await.as_deref(), Some("first"));
        assert_eq!(session.recv().await.as_deref(), Some("second"));
        drop(session);
        reg.finish("r1");
        assert!(matches!(reg.inject("r1", "late"), Injection::NotLive));
    }

    /// The late-message race: a message sent after the session's last turn
    /// ended (stdin closed) but before the run's terminal is `not_live`, not
    /// `injected`-and-lost; one accepted before the close is handed back.
    #[tokio::test]
    async fn a_closed_session_answers_not_live_and_hands_back_what_it_accepted() {
        let reg = RunRegistry::new();
        reg.register("r1", Duration::from_millis(10));
        let inbox = reg.attach("r1", ctx("r1", "claude"), true).expect("inbox");
        let mut session = inbox.open_session().await;
        assert!(matches!(
            reg.inject("r1", "racing"),
            Injection::Queued { .. }
        ));
        assert_eq!(session.close(), vec!["racing".to_owned()]);
        assert!(
            matches!(reg.inject("r1", "late"), Injection::NotLive),
            "the run is still registered, but nothing reads its inbox"
        );
        drop(session);
        assert!(matches!(reg.inject("r1", "between"), Injection::NotLive));
        // A retry's attempt opens a new session, which accepts again.
        let mut retry = inbox.open_session().await;
        assert!(matches!(
            reg.inject("r1", "retry"),
            Injection::Queued { .. }
        ));
        assert_eq!(retry.recv().await.as_deref(), Some("retry"));
    }

    #[test]
    fn a_harness_that_cannot_take_injections_is_answered_unsupported() {
        let reg = RunRegistry::new();
        reg.register("r1", Duration::from_millis(10));
        assert!(reg.attach("r1", ctx("r1", "codex"), false).is_none());
        match reg.inject("r1", "hello") {
            Injection::Unsupported { harness } => assert_eq!(harness, "codex"),
            other => panic!("{other:?}"),
        }
        assert!(matches!(reg.inject("ghost", "hello"), Injection::NotLive));
        assert!(reg.attach("ghost", ctx("ghost", "echo"), true).is_none());
    }
}
