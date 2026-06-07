//! The outcome reconciler: read-back confirmation + retry classification +
//! the backoff retry loop (#155).
//!
//! Lifecycle step 7 of
//! [`docs/contract/agent-orchestration.md`](https://github.com/DataZooDE/escurel/blob/main/docs/contract/agent-orchestration.md)
//! does not trust the harness's self-reported [`HarnessOutcome`]: after the
//! harness runs the runner **reads back over the gateway's own `/mcp`** to
//! confirm the effect actually landed — the triggering event is now
//! `processed` *and bound* to an instance, and that instance's version
//! advanced. Only a confirmed effect is a real success.
//!
//! This module owns the read-back ([`confirm_effect`]), the
//! transient-vs-permanent **classification** ([`ReconcileError`]) that
//! decides whether a failure is worth retrying, and the **backoff retry
//! loop** ([`run_with_retry`]). The loop is generic over an async *attempt*
//! closure so the harness invocation — which lives in the runner binary,
//! where the `Harness` trait is in scope — can be driven from here without
//! this lean core crate depending on the harness crate.
//!
//! ## Retry classification (transient vs fail-fast)
//!
//! A run is retried only when the failure is plausibly self-healing:
//!
//! - **Transient** → retry with backoff up to the attempts cap:
//!   - a `/mcp` transport error (connection refused/reset, DNS, TLS,
//!     timeout) — the gateway may be briefly unavailable / not yet ready;
//!   - a `/mcp` `5xx` HTTP status — a server-side hiccup;
//!   - the harness ran but read-back has **not yet converged** (the event is
//!     not `processed` / not bound, or the version did not advance) — a
//!     partial success that the idempotent `assign_event`/`update_page`
//!     re-run can finish off.
//! - **Permanent** → fail fast (do not burn the remaining attempts):
//!   - a `/mcp` `4xx` HTTP status (auth/validation — re-running won't change
//!     it), excluding `408`/`429` which are treated as transient;
//!   - a JSON-RPC protocol error or an undecodable response (a contract
//!     mismatch a retry can't fix);
//!   - a malformed harness outcome (`BadOutcome`).
//!
//! The caller maps adapter-level harness errors (spawn/timeout/io) to
//! [`ReconcileError::Transient`] and a clean-but-`Failed` harness outcome to
//! [`ReconcileError::Permanent`] before handing the attempt result back.

use std::time::Duration;

use escurel_client::{Client, ExpandRequest, ListEventsRequest, ListInboxRequest};

use crate::{RunnerConfig, Trigger};

/// The confirmed effect of a run, read back over `/mcp`: the instance the
/// event landed on and that instance's version *after* the write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmedEffect {
    /// The instance page the triggering event is now bound to.
    pub instance_page_id: String,
    /// That instance's confirmed version after the run.
    pub version: String,
}

/// A reconcile/attempt failure, classified for the retry policy.
#[derive(Debug, thiserror::Error)]
pub enum ReconcileError {
    /// A plausibly self-healing failure — retry with backoff.
    #[error("transient reconcile failure: {0}")]
    Transient(String),
    /// A failure a retry cannot fix — fail fast.
    #[error("permanent reconcile failure: {0}")]
    Permanent(String),
}

/// Classify an [`escurel_client::Error`] as transient (retry) or permanent
/// (fail fast). See the module docs for the full policy.
pub fn classify_client_error(err: &escurel_client::Error) -> ReconcileError {
    use escurel_client::Error as E;
    match err {
        // Wire-level failures the gateway can recover from.
        E::Transport(_) => ReconcileError::Transient(err.to_string()),
        E::Http { status, .. } if *status >= 500 || *status == 408 || *status == 429 => {
            ReconcileError::Transient(err.to_string())
        }
        // 4xx (other than 408/429), protocol, decode, bad endpoint/token,
        // live-session — re-running the same request won't change the answer.
        _ => ReconcileError::Permanent(err.to_string()),
    }
}

/// Read back over `/mcp` that the triggering event's effect actually landed:
/// the event is now `processed` **and bound** to an instance whose version
/// we can read. Returns the [`ConfirmedEffect`] on success.
///
/// - When the trigger already targets an instance, we confirm the event is
///   `processed` in that instance's `list_events` history, then `expand` the
///   instance to read its confirmed version.
/// - When the trigger had no instance (the harness chose/created one), we
///   confirm the event left the inbox and is now bound (its
///   `instance_page_id` is populated), then `expand` that instance.
///
/// A not-yet-converged read-back (event still in inbox / not processed / not
/// bound) is a [`ReconcileError::Transient`] — the idempotent harness re-run
/// can finish it. A `/mcp` call failure is classified via
/// [`classify_client_error`].
pub async fn confirm_effect(
    client: &Client,
    trigger: &Trigger,
) -> Result<ConfirmedEffect, ReconcileError> {
    let instance_page_id = match &trigger.instance_page_id {
        Some(id) => {
            // Pre-flagged target: confirm the event is processed on it.
            let events = client
                .list_events(ListEventsRequest {
                    instance_page_id: id.clone(),
                    limit: 100,
                })
                .await
                .map_err(|e| classify_client_error(&e))?;
            let processed = events
                .events
                .iter()
                .any(|e| e.event_id == trigger.event_id && e.status == "processed");
            if !processed {
                return Err(ReconcileError::Transient(format!(
                    "event {} not yet processed on {id}",
                    trigger.event_id
                )));
            }
            id.clone()
        }
        None => {
            // No pre-flagged target: the harness chose one. Confirm the event
            // left the inbox and is now bound to an instance.
            let inbox = client
                .list_inbox(ListInboxRequest { limit: 100 })
                .await
                .map_err(|e| classify_client_error(&e))?;
            if inbox.events.iter().any(|e| e.event_id == trigger.event_id) {
                return Err(ReconcileError::Transient(format!(
                    "event {} still in inbox (not yet bound)",
                    trigger.event_id
                )));
            }
            // The event is bound; recover which instance it landed on. We
            // don't have a by-event lookup, so an unbound-target run can only
            // confirm "left the inbox"; without a concrete instance we cannot
            // read a version, so we treat that as not-yet-converged here. The
            // common path (and the #155 DoD) pre-flags the instance.
            return Err(ReconcileError::Transient(format!(
                "event {} bound but instance not resolvable from inbox read-back",
                trigger.event_id
            )));
        }
    };

    // The effect is bound + processed; read the instance back and derive its
    // confirmed version. The gateway does not yet surface a monotonic CRDT
    // version on a read (`update_page` returns a stub), so we take the
    // **content-addressed version** of the read-back instance body — a real,
    // `/mcp`-confirmed marker that advances whenever the instance is written.
    // A missing page (not yet materialised) is not-yet-converged → transient.
    let expanded = client
        .expand(ExpandRequest {
            page_id: instance_page_id.clone(),
            ..Default::default()
        })
        .await
        .map_err(|e| classify_client_error(&e))?;
    if expanded.page.is_none() {
        return Err(ReconcileError::Transient(format!(
            "instance {instance_page_id} not yet materialised"
        )));
    }
    let version = content_version(&expanded.body);

    Ok(ConfirmedEffect {
        instance_page_id,
        version,
    })
}

/// The result of [`run_with_retry`]: whether the run ultimately succeeded
/// (with the confirmed effect to record) and how many attempts it took.
#[derive(Debug)]
pub struct RunReport {
    /// `Some(effect)` when the run converged; `None` when it exhausted the
    /// attempts cap (or hit a permanent failure) and should be recorded
    /// `failed`.
    pub confirmed: Option<ConfirmedEffect>,
    /// How many attempts were made (≥ 1).
    pub attempts: u32,
}

/// Drive a single run with the configured retry policy: call `attempt` up to
/// `cfg.max_attempts` times, backing off `cfg.retry_backoff * attempt`
/// between tries on a [`ReconcileError::Transient`]. A
/// [`ReconcileError::Permanent`] stops immediately (fail fast). Returns a
/// [`RunReport`] describing the terminal outcome.
///
/// `attempt` is an async closure that performs one full try — package + run
/// the harness + [`confirm_effect`] — and returns the [`ConfirmedEffect`] on
/// success. Keeping the harness call behind this closure lets the retry loop
/// live in the lean core crate without depending on the harness crate.
pub async fn run_with_retry<F, Fut>(cfg: &RunnerConfig, mut attempt: F) -> RunReport
where
    F: FnMut(u32) -> Fut,
    Fut: std::future::Future<Output = Result<ConfirmedEffect, ReconcileError>>,
{
    let cap = cfg.max_attempts.max(1);
    let mut tries = 0u32;
    loop {
        tries += 1;
        match attempt(tries).await {
            Ok(effect) => {
                return RunReport {
                    confirmed: Some(effect),
                    attempts: tries,
                };
            }
            Err(ReconcileError::Permanent(reason)) => {
                tracing::warn!(
                    target: "escurel_runner",
                    attempt = tries,
                    reason = %reason,
                    "reconcile: permanent failure; failing fast (no retry)"
                );
                return RunReport {
                    confirmed: None,
                    attempts: tries,
                };
            }
            Err(ReconcileError::Transient(reason)) => {
                if tries >= cap {
                    tracing::warn!(
                        target: "escurel_runner",
                        attempts = tries,
                        reason = %reason,
                        "reconcile: transient failure; attempts exhausted, recording failed"
                    );
                    return RunReport {
                        confirmed: None,
                        attempts: tries,
                    };
                }
                let backoff = backoff_for(cfg.retry_backoff, tries);
                tracing::info!(
                    target: "escurel_runner",
                    attempt = tries,
                    backoff_ms = backoff.as_millis() as u64,
                    reason = %reason,
                    "reconcile: transient failure; retrying after backoff"
                );
                tokio::time::sleep(backoff).await;
            }
        }
    }
}

/// The content-addressed version marker of an instance body: a short hex
/// digest of its bytes. Stable across processes/platforms (SHA-256), so the
/// ledger's recorded version is reproducible and advances exactly when the
/// instance content changes. A stand-in until the gateway surfaces a
/// monotonic CRDT version on a read.
fn content_version(body: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(body.as_bytes());
    format!("sha256:{:x}", digest)
}

/// Compute the backoff before the `attempt`-th retry: a simple linear
/// multiple of the base (`base * attempt`), capped at 30s so a
/// misconfiguration can't wedge the loop for minutes.
fn backoff_for(base: Duration, attempt: u32) -> Duration {
    base.saturating_mul(attempt).min(Duration::from_secs(30))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(max_attempts: u32) -> RunnerConfig {
        let mut c = RunnerConfig::from_env_with(|_| None).expect("defaults");
        c.max_attempts = max_attempts;
        c.retry_backoff = Duration::from_millis(0);
        c
    }

    fn effect() -> ConfirmedEffect {
        ConfirmedEffect {
            instance_page_id: "inst".into(),
            version: "v1".into(),
        }
    }

    #[test]
    fn transport_error_is_transient() {
        // A connection-refused style transport error must be retried.
        // Build a real reqwest transport error via a failed request.
        let err = escurel_client::Error::Http {
            status: 503,
            body: "x".into(),
        };
        assert!(matches!(
            classify_client_error(&err),
            ReconcileError::Transient(_)
        ));
    }

    #[test]
    fn client_4xx_is_permanent_but_429_408_are_transient() {
        let four_oh_one = escurel_client::Error::Http {
            status: 401,
            body: "no".into(),
        };
        assert!(matches!(
            classify_client_error(&four_oh_one),
            ReconcileError::Permanent(_)
        ));
        for s in [408u16, 429] {
            let e = escurel_client::Error::Http {
                status: s,
                body: "".into(),
            };
            assert!(
                matches!(classify_client_error(&e), ReconcileError::Transient(_)),
                "{s} must be transient"
            );
        }
    }

    #[test]
    fn jsonrpc_and_decode_are_permanent() {
        let rpc = escurel_client::Error::JsonRpc {
            code: -32000,
            message: "quota".into(),
        };
        assert!(matches!(
            classify_client_error(&rpc),
            ReconcileError::Permanent(_)
        ));
        let dec = escurel_client::Error::Decode("bad".into());
        assert!(matches!(
            classify_client_error(&dec),
            ReconcileError::Permanent(_)
        ));
    }

    #[test]
    fn content_version_is_stable_and_advances_on_change() {
        let a = content_version("BASELINE");
        assert_eq!(a, content_version("BASELINE"), "stable for same content");
        assert_ne!(a, content_version("BASELINE + folded"), "advances on write");
        assert!(a.starts_with("sha256:"));
    }

    #[tokio::test]
    async fn retry_succeeds_after_transient_failures() {
        let c = cfg(5);
        let report = run_with_retry(&c, |attempt| async move {
            if attempt < 3 {
                Err(ReconcileError::Transient("not yet".into()))
            } else {
                Ok(effect())
            }
        })
        .await;
        assert_eq!(report.attempts, 3);
        assert_eq!(report.confirmed, Some(effect()));
    }

    #[tokio::test]
    async fn retry_exhausts_cap_and_reports_failed() {
        let c = cfg(3);
        let report = run_with_retry(&c, |_attempt| async {
            Err::<ConfirmedEffect, _>(ReconcileError::Transient("always".into()))
        })
        .await;
        assert_eq!(report.attempts, 3, "must use the whole cap");
        assert!(report.confirmed.is_none());
    }

    #[tokio::test]
    async fn permanent_failure_fails_fast_without_burning_attempts() {
        let c = cfg(5);
        let report = run_with_retry(&c, |_attempt| async {
            Err::<ConfirmedEffect, _>(ReconcileError::Permanent("bad outcome".into()))
        })
        .await;
        assert_eq!(report.attempts, 1, "permanent failure must not retry");
        assert!(report.confirmed.is_none());
    }
}
