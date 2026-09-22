//! Cancelling a live run (knowledge-workbench backend P2-3a — BRD FR-C).
//!
//! A run is cancellable from the moment the dispatch loop claims it until
//! its terminal. The loop registers a [`Cancel`] per run in a
//! [`CancelRegistry`] keyed by run id and hands a clone to the harness via
//! [`crate::TaskContext::cancel`]; a subprocess harness stops its child on
//! it (SIGTERM, then [`Cancel::grace`], then SIGKILL) and an in-process one
//! checks it between turns. Whoever asks — the `/debug/cancel` seam today,
//! the `escurel:run-control` subscriber next — names the run and a reason;
//! the reason reaches the run's `run-finished`.
//!
//! Cancelling a run that is not live is a plain `false`: the ledger's
//! terminal already stands, and there is nothing to stop.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio_util::sync::CancellationToken;

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

struct Entry {
    cancel: Cancel,
    reason: Option<String>,
}

/// The live runs' cancel handles, keyed by run id. Cheap to clone (shared).
#[derive(Clone, Default)]
pub struct CancelRegistry {
    inner: Arc<Mutex<HashMap<String, Entry>>>,
}

impl CancelRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a run as live and get its handle. Re-registering a run id
    /// replaces the handle (a re-claimed run is a new run).
    pub fn register(&self, run_id: &str, grace: Duration) -> Cancel {
        let cancel = Cancel::new(grace);
        self.inner.lock().expect("cancel registry mutex").insert(
            run_id.to_owned(),
            Entry {
                cancel: cancel.clone(),
                reason: None,
            },
        );
        cancel
    }

    /// Cancel a live run; `false` when no such run is live. The first
    /// reason wins — a second cancel of the same run changes nothing.
    pub fn cancel(&self, run_id: &str, reason: Option<&str>) -> bool {
        let mut map = self.inner.lock().expect("cancel registry mutex");
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

    /// Forget a run at its terminal, returning the cancel reason if it was
    /// cancelled (`Some(None)` = cancelled without a reason).
    pub fn finish(&self, run_id: &str) -> Option<Option<String>> {
        let entry = self
            .inner
            .lock()
            .expect("cancel registry mutex")
            .remove(run_id)?;
        entry.cancel.is_cancelled().then_some(entry.reason)
    }

    /// The live run ids.
    #[must_use]
    pub fn live(&self) -> Vec<String> {
        self.inner
            .lock()
            .expect("cancel registry mutex")
            .keys()
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_registered_run_is_cancelled_once_with_the_first_reason() {
        let reg = CancelRegistry::new();
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
        let reg = CancelRegistry::new();
        assert!(!reg.cancel("ghost", None));
        reg.register("r2", Duration::from_millis(10));
        assert_eq!(reg.live(), vec!["r2".to_owned()]);
        assert_eq!(reg.finish("r2"), None, "ran to its own terminal");
    }
}
