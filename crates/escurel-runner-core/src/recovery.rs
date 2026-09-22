//! Crash recovery: reconcile orphaned `pending` ledger rows on restart (#158).
//!
//! A run is written `pending` the moment it is created and only moves to a
//! terminal status once the dispatch loop reconciles it. If the process is
//! killed (crash, OOM, an un-drained `SIGKILL`) mid-run, that row is left
//! `pending` forever — an orphan that the idempotency gate would treat as
//! in-flight, wedging the event.
//!
//! On startup the runner sweeps every `pending` row and **re-confirms it by
//! read-back** against the gateway (the same `confirm_effect` the reconciler
//! uses, #155):
//!
//! - the effect actually landed (the event is `processed` + bound, the
//!   instance materialised) → mark the run `processed` (with the read-back
//!   instance/version). The work finished; we just never recorded it.
//! - it did not land (or the read-back errors) → reset the row to **retriable
//!   `failed`**. A `failed` row is not idempotency-terminal (#157), so the
//!   poller re-pulls the still-`inbox` event and `begin_run` re-claims it. The
//!   poller is the ultimate backstop.
//!
//! This leaves the ledger with **no orphaned `pending` rows** after a restart:
//! every prior in-flight run is either confirmed `processed` or cleanly
//! re-drivable. The sweep is best-effort and bounded — a row whose read-back
//! errors is reset to `failed` (re-drivable) rather than left pending.

use std::sync::Arc;

use escurel_client::Client;

use crate::{Ledger, RunId, RunStatus, Trigger, confirm_effect};

/// Outcome tally of a [`recover_pending`] sweep, for logging/tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RecoveryReport {
    /// How many orphaned `pending` rows were swept.
    pub swept: usize,
    /// How many were confirmed and marked `processed`.
    pub confirmed: usize,
    /// How many were reset to retriable `failed` for the poller to re-drive.
    pub reset: usize,
}

/// Sweep every `pending` ledger row and reconcile it by read-back against the
/// gateway `client`. See the module docs for the policy. Returns a tally; the
/// sweep never errors fatally (a per-row failure resets that row to `failed`).
///
/// `emit_events`: also write the `run-finished` the crashed process never
/// got to (workbench backend P1 PR7b) for every row confirmed here, so the
/// gateway's projection of the ledger never misses a terminal the ledger
/// reached. Best-effort like every run event; idempotent by event id, so a
/// run that had already written its own is a no-op. A row reset to `failed`
/// gets none — it is re-driven, and its terminal is still to come.
pub async fn recover_pending(
    ledger: &Arc<Ledger>,
    client: &Client,
    emit_events: bool,
) -> RecoveryReport {
    let mut report = RecoveryReport::default();
    let pending = match ledger.list_pending() {
        Ok(rows) => rows,
        Err(e) => {
            tracing::error!(
                target: "escurel_runner",
                error = %e,
                "recovery: could not list pending rows; skipping sweep"
            );
            return report;
        }
    };
    report.swept = pending.len();
    if pending.is_empty() {
        return report;
    }
    tracing::info!(
        target: "escurel_runner",
        pending = pending.len(),
        "recovery: reconciling orphaned pending runs by read-back"
    );

    for rec in pending {
        let run_id = RunId(rec.run_id.clone());
        // Reconstruct the minimal Trigger confirm_effect needs (tenant,
        // event_id, the pre-flagged target instance).
        let trigger = Trigger {
            is_system: false,
            tenant: rec.tenant.clone(),
            event_id: rec.event_id.clone(),
            label_skill: String::new(),
            instance_page_id: rec.instance_page_id.clone(),
            lineage: crate::Lineage::root(rec.event_id.clone()),
            workflow: None,
            // Recovery only READS this run back; it never claims a new one, so
            // there is nothing to dedup against.
            content_hash: None,
        };
        match confirm_effect(client, &trigger).await {
            Ok(effect) => {
                let _ = ledger.complete(
                    &run_id,
                    RunStatus::Processed,
                    Some((effect.instance_page_id.as_str(), effect.version.as_str())),
                );
                report.confirmed += 1;
                tracing::info!(
                    target: "escurel_runner",
                    event_id = %rec.event_id,
                    run_id = %run_id,
                    instance = %effect.instance_page_id,
                    "recovery: orphaned run confirmed landed; marked processed"
                );
                if emit_events {
                    // What the ledger row still knows; the crashed process's
                    // harness, attempts and summary are gone with it.
                    let ctx = crate::RunEventCtx {
                        run_id: run_id.0.clone(),
                        root_event_id: rec.root_event_id.clone(),
                        trigger_event_id: rec.event_id.clone(),
                        parent_run_id: None,
                        depth: rec.depth,
                        lineage_path: vec![rec.event_id.clone()],
                        trace_id: None,
                        harness: "recovery".to_owned(),
                        model: None,
                        max_attempts: 0,
                        target_page_id: Some(effect.instance_page_id.clone()),
                    };
                    let finish = crate::RunFinish::Processed {
                        produced: Some((effect.instance_page_id.clone(), effect.version.clone())),
                        held: effect.held,
                    };
                    if let Err(e) = ctx
                        .emit_finished(client, 0, &finish, "reconciled on restart", 0, None)
                        .await
                    {
                        tracing::warn!(
                            target: "escurel_runner",
                            run_id = %run_id,
                            error = %e,
                            "recovery: run-finished refused by the gateway (best-effort)"
                        );
                    }
                }
            }
            Err(_) => {
                // Not confirmed (or read-back failed): reset to retriable so the
                // poller re-drives the still-inbox event. `failed` is not
                // idempotency-terminal (#157).
                let _ = ledger.complete(&run_id, RunStatus::Failed, None);
                report.reset += 1;
                tracing::info!(
                    target: "escurel_runner",
                    event_id = %rec.event_id,
                    run_id = %run_id,
                    "recovery: orphaned run not confirmed; reset to retriable (poller backstops)"
                );
            }
        }
    }
    report
}
