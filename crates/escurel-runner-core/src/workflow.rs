//! The workflow driver — the I/O half of the reducer (`§3.4`).
//!
//! The pure [`reduce`](escurel_runner_workflow::reduce) planner does no I/O;
//! this module is the runner-side half that feeds it. On a confirmed write
//! whose trigger carries a `provenance.workflow` block, the dispatch loop
//! calls [`drive_workflow`] **instead of** `emit_cascade` — the cascade
//! emitter is the width-≤1 special case; this is the general one. It:
//!
//! 1. loads the immutable plan (`expand` the `kind: workflow` skill page);
//! 2. builds a [`RunState`] by reading each phase's produced instances
//!    (`list_instances(<produces>)`, run-scoped by the deterministic
//!    pre-flagged page-id convention `§3.6` — harness-agnostic, no reliance
//!    on the harness stamping a `workflow_run` field);
//! 3. calls `reduce` to get the next batch of [`StepIntent`]s;
//! 4. `capture_event`s each with its content-addressed id + pre-flagged
//!    instance id + a `provenance.workflow` block alongside the runner
//!    lineage — so the emitted step re-enters the exact same
//!    poll → trigger → package → harness → reconcile pipeline, guarded by
//!    the same `admit` loop controls as any cascade.
//!
//! Re-emission is idempotent by construction (`§3.6`): the deterministic
//! event id + `capture_event`'s `ON CONFLICT DO NOTHING` collapse a step
//! decided twice, so the driver passes an empty `emitted` set and relies on
//! the id — it is edge-triggered on confirmations, never a busy loop.

use std::collections::{BTreeMap, BTreeSet};

use escurel_client::Client;
use escurel_runner_workflow::{
    BudgetExceeded, Fallback, FanOut, OperationStatus, OutcomePolicy, ProducedInstance, RunState,
    StepIntent, Vote, WorkflowSkill, WriteMode, check_budget, is_complete, key, reduce,
};
use escurel_types::{
    AssignEventRequest, CaptureEventRequest, ExpandRequest, InstanceInfo, ListEventsRequest,
    ListInstancesRequest, WorkflowProvenance,
};
use serde_json::json;

use crate::reconciler::ConfirmedEffect;
use crate::trigger::Trigger;

/// The reserved `label_skill` under which an operation's status is recorded on
/// its run board (async-ops Phase 0.2). Defined in `escurel-types` as shared
/// wire vocabulary (the runner writes it, the gateway's `get_operation` reads
/// it); re-exported so `escurel_runner_core::OPERATION_STATUS_LABEL` resolves.
///
/// It is a KB-visible record, **never a dispatchable run**: the runner's enqueue
/// chokepoint (`gate_and_enqueue`) drops any trigger carrying this label before
/// a ledger row is created. The `escurel:` prefix is a **reserved namespace** a
/// tenant cannot author (crew F-7). Phase 1 adds the capture-layer backstop that
/// rejects a *caller* who tries to write it; until then the single enqueue
/// chokepoint is the guard.
pub use escurel_types::OPERATION_STATUS_LABEL;

/// Outcome of driving one reducer pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkflowDriveOutcome {
    /// The event ids emitted this pass (empty when the run is complete or the
    /// trigger's skill is not a workflow plan).
    pub emitted: Vec<String>,
    /// A channel delivery to make when this pass drove the operation to a
    /// TERMINAL state and the run board carries a `conversation_ref` (async-ops
    /// Phase 3). The driver only surfaces the delivery; the runner performs the
    /// outbound POST. `None` for a non-terminal pass or an operation with no
    /// stored conversation reference (e.g. an A2A/pull operation).
    pub delivery: Option<TerminalDelivery>,
}

/// A terminal result to deliver back to the channel that started the operation
/// (async-ops Phase 3). The runner POSTs it to the channel's proactive seam
/// (`/v1/outbound`), keyed on the stored `conversation_ref`. Delivery is
/// at-least-once — the courier dedups on `operation_id` — so a re-driven
/// terminal never doubles a user-visible message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalDelivery {
    /// The operation's run board page id (the dedup key for the courier).
    pub operation_id: String,
    /// The status being delivered — a terminal
    /// (`succeeded`/`failed`/`awaiting_human`) OR, for a selective PROGRESS
    /// delivery (async-ops Phase 3b), `running`.
    pub status: String,
    /// The opaque channel reference the caller supplied at `start_operation`.
    pub conversation_ref: serde_json::Value,
    /// A human progress phrase for a NON-terminal (`running`) delivery — the
    /// message the courier renders back into the conversation (e.g. "⏳ Working
    /// on *signals* (2 of 4)"). `None` for a terminal delivery, where the courier
    /// renders the operation's own result (or a terse status fallback).
    pub note: Option<String>,
    /// The CHANNEL's tenant as recorded when the operation STARTED —
    /// deliberately NOT read out of `conversation_ref`.
    ///
    /// The delivery side needs a tenant the redeemer did not write. Without
    /// it a courier can only check the reference's tenant against the
    /// caller's own, and the caller supplies both, so the check binds
    /// nothing (DataZooDE/triton#332). `None` for an operation started
    /// before this field existed, or off-chat.
    pub channel_tenant: Option<String>,
}

/// Errors driving a workflow reducer pass.
#[derive(Debug, thiserror::Error)]
pub enum WorkflowDriveError {
    #[error("workflow: gateway read failed: {0}")]
    Read(escurel_client::Error),
    #[error("workflow: emitting a step event failed: {0}")]
    Capture(escurel_client::Error),
    #[error(transparent)]
    Budget(#[from] BudgetExceeded),
}

/// Skill page id for a plan skill id (`markdown/skills/<id>.md`).
fn skill_page_id(skill: &str) -> String {
    format!("markdown/skills/{skill}.md")
}

/// Project a `verify-vote` instance's frontmatter into a barrier [`Vote`].
/// Returns `None` when the required fields are absent (a malformed vote is
/// ignored rather than skewing the tally).
fn vote_from_instance(inst: &InstanceInfo) -> Option<Vote> {
    let fm = &inst.frontmatter;
    let claim = fm.get("claim")?.as_str()?.to_owned();
    let vote_index = u32::try_from(fm.get("vote_index")?.as_u64()?).ok()?;
    let verdict = fm
        .get("verdict")
        .and_then(|v| v.as_str())
        .unwrap_or("unverified")
        .to_owned();
    Some(Vote {
        claim,
        vote_index,
        verdict,
    })
}

/// How the step whose terminal transition is driving this reducer pass ended
/// (async-ops Phase 0.3). The pure reducer plans from the run's produced
/// instances alone; this tells the *driver* whether the terminating step made
/// progress or failed, so a failure can be turned into the operation's terminal
/// status per the failed phase's authored [`OutcomePolicy`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepTerminal {
    /// A confirmed write or a converged no-op — the step made progress;
    /// advance the plan.
    Advanced,
    /// The step produced a **held draft** awaiting human approval; the plan
    /// must not advance past it — the operation pauses at `awaiting_human`.
    Held,
    /// The step reached a **failure terminal** — dead-lettered (retries
    /// exhausted / bad output), a fail-fast `Permanent` error, or a reducer
    /// failure. The `&'static str` is a stable reason slug recorded in the
    /// status provenance (crew F-5). Construct this only from a run that has
    /// actually hit a ledger terminal: a genuinely *transient* error is retried
    /// within the run by the reconciler (→ a confirmed write, or
    /// `RetriesExhausted`) and never arrives here mid-retry (crew F-2). The
    /// failed phase's `on_exhausted` policy decides the operation's fate.
    Failed(&'static str),
}

/// Run one reducer pass for the workflow the `trigger` belongs to on a terminal
/// ledger transition (async-ops Phase 0.3 — Bug A: previously the reducer ran
/// only on a confirmed, non-held write, so a converged no-op, a held draft, or
/// a failed step left the parent operation wedged at `running` forever).
///
/// - [`StepTerminal::Advanced`]: build state, emit the next batch, record
///   `running`/`succeeded`. `effect` is `Some` for a confirmed write (the
///   emitted steps' provenance extends its instance path) and `None` for a
///   converged/held terminal.
/// - [`StepTerminal::Failed`]: the failed phase's `on_exhausted` decides —
///   `Stop` ⇒ operation `failed`; `AskHuman` ⇒ `awaiting_human`; `Skip` is not
///   yet honoured (it needs the reducer to advance past a dead-lettered phase,
///   Phase 0.3b) so it **fails closed** as `failed` rather than silently
///   dropping the step's output.
///
/// Returns an empty outcome when the run has no more steps (complete) or the
/// labelled skill is not a workflow plan.
pub async fn drive_workflow(
    client: &Client,
    trigger: &Trigger,
    parent_run_id: &str,
    effect: Option<&ConfirmedEffect>,
    terminal: StepTerminal,
    max_runs_per_root: u64,
) -> Result<WorkflowDriveOutcome, WorkflowDriveError> {
    let Some(wf) = &trigger.workflow else {
        return Ok(WorkflowDriveOutcome::default());
    };

    // 1. Load the immutable plan — YAML `phases:` frontmatter or the prose
    //    dialect in the page body (crew F-1: the dialect is now wired in, so an
    //    authored `on failure: … ask a human` reaches the driver).
    let expanded = client
        .expand(ExpandRequest {
            page_id: skill_page_id(&wf.wf_skill),
            ..Default::default()
        })
        .await
        .map_err(WorkflowDriveError::Read)?;
    // The event that triggered this drive — the transition key that makes the
    // status history append-only (crew F-3): a status event id is a function of
    // (operation, triggering event, status), so `running → awaiting_human →
    // running` records three ordered rows rather than collapsing onto one.
    let transition = trigger.event_id.as_str();

    let spec = match WorkflowSkill::parse_page(&wf.wf_skill, &expanded.frontmatter, &expanded.body)
    {
        Ok(Some(spec)) => spec,
        // Not a workflow plan (no phases, empty prose) — nothing to drive.
        Ok(None) => return Ok(WorkflowDriveOutcome::default()),
        // A plan that FAILED to parse (crew final-review F3): record a terminal
        // `failed` with the reason so the operation reaches a terminal status —
        // never the silent `pending` wedge (Bug-A) that a swallowed error caused.
        Err(e) => {
            tracing::warn!(
                target: "escurel_runner",
                operation = %wf.run,
                wf_skill = %wf.wf_skill,
                error = %e,
                "workflow: plan is unparseable; recording terminal failed"
            );
            record_status_best_effort(
                client,
                &wf.run,
                transition,
                OperationStatus::Failed,
                &wf.phase,
                "plan_unparseable",
            )
            .await;
            return Ok(WorkflowDriveOutcome::default());
        }
    };

    // A held draft pauses the operation for human approval; the plan must not
    // advance past it, so record `awaiting_human` and emit nothing.
    if terminal == StepTerminal::Held {
        record_status_best_effort(
            client,
            &wf.run,
            transition,
            OperationStatus::AwaitingHuman,
            &wf.phase,
            "held_draft",
        )
        .await;
        let delivery = maybe_delivery(client, &wf.run, OperationStatus::AwaitingHuman).await;
        return Ok(WorkflowDriveOutcome {
            delivery,
            ..Default::default()
        });
    }

    // A terminal FAILURE is resolved from the failed phase's authored policy
    // BEFORE any emit — a Stop/AskHuman step must not re-emit itself into a
    // retry loop. `is_complete` guards the rare race where the failing step's
    // output nonetheless landed and every phase is already done.
    if let StepTerminal::Failed(fail_reason) = terminal {
        let state = build_run_state(client, wf, &spec).await?;
        if is_complete(&spec, &state) {
            record_status_best_effort(
                client,
                &wf.run,
                transition,
                OperationStatus::Succeeded,
                &wf.phase,
                "",
            )
            .await;
            let delivery = maybe_delivery(client, &wf.run, OperationStatus::Succeeded).await;
            return Ok(WorkflowDriveOutcome {
                delivery,
                ..Default::default()
            });
        }
        let policy = phase_outcome(&spec, &wf.phase);
        let (status, reason) = match policy.on_exhausted {
            Fallback::AskHuman => (OperationStatus::AwaitingHuman, fail_reason),
            // Skip is not yet honoured (Phase 0.3b); fail closed rather than
            // silently advance past a dropped step — recorded so the operator
            // sees the substitution in the KB, not only in a runner log.
            Fallback::Skip => {
                tracing::warn!(
                    target: "escurel_runner",
                    operation = %wf.run,
                    phase = %wf.phase,
                    "workflow: authored `skip` fallback not yet honoured (Phase 0.3b); failing closed"
                );
                (OperationStatus::Failed, "skip_unsupported")
            }
            Fallback::Stop => (OperationStatus::Failed, fail_reason),
        };
        record_status_best_effort(client, &wf.run, transition, status, &wf.phase, reason).await;
        let delivery = maybe_delivery(client, &wf.run, status).await;
        return Ok(WorkflowDriveOutcome {
            delivery,
            ..Default::default()
        });
    }

    // Reserve the plan's projected fan-out against the budget BEFORE emitting
    // anything (`§7`). Checked every pass (the projection is constant), but it
    // is the invocation pass — before any step exists — that fails fast, so an
    // over-budget plan never starts and can never starve a barrier mid-flight.
    check_budget(&spec, max_runs_per_root)?;

    // 2. Build the run state, 3. plan the next batch, 4. emit — each step's
    //    provenance carries the runner lineage extended from this trigger.
    let state = build_run_state(client, wf, &spec).await?;
    let intents = reduce(&spec, &state);
    let emitted = emit_intents(client, &intents, |intent| {
        build_step_provenance(trigger, parent_run_id, effect, intent)
    })
    .await?;
    // Operation status (async-ops Phase 0.2), event-sourced: record the
    // operation's status as an assigned event on the run board — `succeeded`
    // once every phase is complete, else `running`. `get_operation` (Phase 2)
    // derives the current status from these append-only events.
    let status = if is_complete(&spec, &state) {
        OperationStatus::Succeeded
    } else {
        OperationStatus::Running
    };
    record_status_best_effort(client, &wf.run, transition, status, &wf.phase, "").await;
    // A completed plan (Succeeded) delivers its terminal; a `Running` pass
    // delivers a SELECTIVE progress note, but only when it opens a NEW phase
    // (async-ops Phase 3b) — one push per phase boundary, never per step.
    let delivery = if matches!(status, OperationStatus::Running) {
        maybe_progress_delivery(client, wf, &spec, &intents).await
    } else {
        maybe_delivery(client, &wf.run, status).await
    };
    Ok(WorkflowDriveOutcome { emitted, delivery })
}

/// Build the channel delivery for a TERMINAL operation status (async-ops Phase
/// 3), or `None` when there is nothing to deliver: a non-terminal status
/// (`running`), an operation whose board carries no `conversation_ref` (an
/// A2A/pull operation, delivered by polling `tasks/get`), or a best-effort read
/// failure (delivery must never derail the run — logged and skipped).
async fn maybe_delivery(
    client: &Client,
    operation: &str,
    status: OperationStatus,
) -> Option<TerminalDelivery> {
    let is_terminal = matches!(
        status,
        OperationStatus::Succeeded | OperationStatus::Failed | OperationStatus::AwaitingHuman
    );
    if !is_terminal {
        return None;
    }
    let board = match client
        .expand(ExpandRequest {
            page_id: operation.to_owned(),
            ..Default::default()
        })
        .await
    {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(
                target: "escurel_runner",
                operation = %operation,
                error = %e,
                "workflow: could not read board for channel delivery (best-effort); skipping"
            );
            return None;
        }
    };
    let conversation_ref = board.frontmatter.get("conversation_ref").cloned()?;
    // From the BOARD, not from the reference beside it: the board was written
    // when the operation started, by the server, and whoever redeems the
    // delivery cannot rewrite it.
    let channel_tenant = board
        .frontmatter
        .get("channel_tenant")
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    Some(TerminalDelivery {
        operation_id: operation.to_owned(),
        status: status.as_str().to_owned(),
        conversation_ref,
        note: None,
        channel_tenant,
    })
}

/// Build a selective PROGRESS delivery (async-ops Phase 3b) when this reducer
/// pass OPENS A NEW PHASE — the operation just advanced from `wf.phase` into the
/// phase the freshly-emitted `intents` belong to. Returns `None` otherwise: a
/// pass that emitted nothing new, a pass still inside the same phase (a fan-out
/// step landing), or an operation with no stored `conversation_ref` (pull-only).
///
/// This is deliberately "not every transition" — one push per phase BOUNDARY,
/// not per step — so a wide fan-out does not spam the conversation. The `invoke`
/// pass counts as a boundary (it opens phase 1), so the first push is "now
/// running <first phase>".
async fn maybe_progress_delivery(
    client: &Client,
    wf: &WorkflowProvenance,
    spec: &WorkflowSkill,
    intents: &[StepIntent],
) -> Option<TerminalDelivery> {
    // The phase the newly-emitted steps belong to. Empty ⇒ nothing emitted.
    let next_phase = intents.first().map(|i| i.phase.as_str())?;
    // Only a BOUNDARY: the emitted steps open a phase different from the one that
    // just completed (`wf.phase`). Within-phase passes re-emit nothing new here.
    if next_phase == wf.phase {
        return None;
    }
    let board = client
        .expand(ExpandRequest {
            page_id: wf.run.clone(),
            ..Default::default()
        })
        .await
        .ok()?;
    let conversation_ref = board.frontmatter.get("conversation_ref").cloned()?;
    // From the BOARD, not from the reference beside it: the board was written
    // when the operation started, by the server, and whoever redeems the
    // delivery cannot rewrite it.
    let channel_tenant = board
        .frontmatter
        .get("channel_tenant")
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    // 1-based position of the phase in the plan, for a "(n of m)" hint.
    let total = spec.phases.len();
    let idx = spec
        .phases
        .iter()
        .position(|p| p.id == next_phase)
        .map(|i| i + 1)
        .unwrap_or(0);
    let note = if idx > 0 && total > 0 {
        format!("⏳ Working on *{next_phase}* ({idx} of {total})…")
    } else {
        format!("⏳ Working on *{next_phase}*…")
    };
    Some(TerminalDelivery {
        operation_id: wf.run.clone(),
        status: OperationStatus::Running.as_str().to_owned(),
        conversation_ref,
        note: Some(note),
        channel_tenant,
    })
}

/// The [`OutcomePolicy`] of the plan phase named `phase_id`; the default
/// (`retries: 0, on_exhausted: Stop`) when the id is not a plan phase (e.g. the
/// synthetic `invoke`/`recover` phases) — a conservative fail-closed default.
fn phase_outcome(spec: &WorkflowSkill, phase_id: &str) -> OutcomePolicy {
    spec.phases
        .iter()
        .find(|p| p.id == phase_id)
        .map(|p| p.outcome)
        .unwrap_or_default()
}

/// Record the operation status, best-effort (F6): the status record is an
/// observability side-channel for `get_operation`, not the control path. A
/// gateway hiccup writing it must not derail a run that made progress — log and
/// carry on. `transition` is the triggering event id (or a synthetic marker
/// like `recover`); `phase`/`reason` are empty to omit.
pub async fn record_status_best_effort(
    client: &Client,
    operation: &str,
    transition: &str,
    status: OperationStatus,
    phase: &str,
    reason: &str,
) {
    if let Err(e) = record_status(client, operation, transition, status, phase, reason).await {
        tracing::warn!(
            target: "escurel_runner",
            operation = %operation,
            status = status.as_str(),
            error = %e,
            "workflow: recording operation status failed (best-effort); run unaffected"
        );
    }
}

/// Whether the operation's LATEST recorded status event is a terminal
/// failure/hold (`failed` / `awaiting_human`). Used to leave such an operation
/// alone — its terminal is authoritative and not re-derivable from the run
/// state: [`recover_workflows`] must not re-emit a failed step, and the runner's
/// invocation-pass driver must not re-record `running` over a terminal that a
/// concurrent driver (crash recovery) already reached. Best-effort: an
/// unreadable event log returns `false` (the caller then proceeds as before).
/// Matches the wire string directly so it does not depend on the status parser.
pub async fn operation_has_terminal_status(client: &Client, run: &str) -> bool {
    let Ok(resp) = client
        .list_events(ListEventsRequest {
            instance_page_id: run.to_owned(),
            limit: 200,
            ..Default::default()
        })
        .await
    else {
        return false;
    };
    let latest = resp
        .events
        .iter()
        .filter(|e| e.label_skill == OPERATION_STATUS_LABEL)
        .filter_map(|e| {
            e.provenance
                .get("run_status")
                .and_then(|v| v.as_str())
                .map(|s| (e.at.as_str(), s))
        })
        .max_by(|a, b| a.0.cmp(b.0))
        .map(|(_, s)| s.to_owned());
    matches!(latest.as_deref(), Some("failed") | Some("awaiting_human"))
}

/// Record the operation's status as a **processed, assigned** event on the run
/// board — the append-only record `get_operation` reads.
///
/// Three properties make this safe, and none may be silently dropped:
///
/// - **Fail-closed against dispatch (F1).** The event is captured under the
///   reserved [`OPERATION_STATUS_LABEL`], which the runner's enqueue chokepoint
///   (`gate_and_enqueue`) refuses before creating a ledger row. The prior code
///   relied on `assign_event` racing ahead of the poller/webhook to move the
///   event out of the inbox; it does not — `capture_event` lands it `inbox`,
///   and the webhook's synchronous notify (and the next poll tick) enqueue it
///   before `assign_event` runs, spawning a dead-lettered run per transition.
///   The label guard closes that window regardless of timing.
/// - **Append-only (F3).** The event id is a function of `(operation,
///   transition, status)` — `transition` being the triggering event id — so a
///   later `running` after an `awaiting_human` does NOT collapse onto the first
///   `running`'s row. `capture_event`'s `ON CONFLICT DO NOTHING` still makes a
///   *re-drive of the same transition* idempotent.
/// - **Time-ordered (F2).** `at` is stamped so the board's history orders by
///   wall-clock, not by the status event's content-addressed id (which is not
///   monotonic). `get_operation` derives the current status as the LATEST such
///   event by `at` — so a re-driven operation reports its current state, not a
///   stale terminal.
async fn record_status(
    client: &Client,
    operation: &str,
    transition: &str,
    status: OperationStatus,
    phase: &str,
    reason: &str,
) -> Result<(), WorkflowDriveError> {
    let status_str = status.as_str();
    let event_id = key::step_event_id(
        operation,
        OPERATION_STATUS_LABEL,
        &format!("{transition}:{status_str}"),
    );
    // DuckDB casts this via `TRY_CAST(? AS TIMESTAMP)`; match the space-separated
    // microsecond format `paged_events` reads back so the round-trip is exact.
    let at = chrono::Utc::now()
        .format("%Y-%m-%d %H:%M:%S%.6f")
        .to_string();
    // Provenance carries the cause so a status is diagnosable from the KB alone
    // (crew F-5): phase, reason and the triggering event id.
    let mut prov = serde_json::Map::new();
    prov.insert("run_status".to_owned(), json!(status_str));
    prov.insert("transition_event".to_owned(), json!(transition));
    if !phase.is_empty() {
        prov.insert("phase".to_owned(), json!(phase));
    }
    if !reason.is_empty() {
        prov.insert("reason".to_owned(), json!(reason));
    }
    let title = match (phase.is_empty(), reason.is_empty()) {
        (false, false) => format!("status: {status_str} (phase {phase}, {reason})"),
        (false, true) => format!("status: {status_str} (phase {phase})"),
        _ => format!("status: {status_str}"),
    };
    client
        .capture_event(CaptureEventRequest {
            event_id: event_id.clone(),
            at,
            source: "escurel-runner".to_owned(),
            mime: "text/plain".to_owned(),
            label_skill: OPERATION_STATUS_LABEL.to_owned(),
            instance_page_id: operation.to_owned(),
            title,
            body: String::new(),
            provenance: serde_json::Value::Object(prov),
        })
        .await
        .map_err(WorkflowDriveError::Capture)?;
    client
        .assign_event(AssignEventRequest {
            event_id,
            instance_page_id: operation.to_owned(),
        })
        .await
        .map_err(WorkflowDriveError::Capture)?;
    Ok(())
}

/// The workflow-run instance skill — its instances are the run boards the
/// recovery pass enumerates.
const RUN_SKILL: &str = "workflow-run";

/// Workflow-aware crash recovery (`§7`). `recover_pending` reconciles
/// individual pending ledger rows but never re-invokes the reducer, so a
/// crash after emitting 2 of 3 barrier children would wedge the barrier. This
/// pass enumerates every `workflow-run` instance and re-drives its reducer:
/// §3.6 keys make re-emitting a missing step idempotent (the landed ones
/// collapse), so a non-terminal run continues from exactly where it stopped,
/// and a complete run emits nothing. Because the state lives in the tenant KB,
/// resume survives process death.
///
/// Returns the number of runs that still had steps to emit (0 ⇒ everything
/// was already complete). A run whose board frontmatter lacks `wf_skill` is
/// skipped (nothing ties it to a plan).
pub async fn recover_workflows(
    client: &Client,
    max_runs_per_root: u64,
) -> Result<usize, WorkflowDriveError> {
    let runs = client
        .list_instances(ListInstancesRequest {
            skill: RUN_SKILL.to_owned(),
            ..Default::default()
        })
        .await
        .map_err(WorkflowDriveError::Read)?;

    let mut resumed = 0;
    for run in runs.instances {
        let Some(wf_skill) = run.frontmatter.get("wf_skill").and_then(|v| v.as_str()) else {
            continue;
        };
        // A stopped run (via `escurel workflow stop`) is left alone.
        if run.frontmatter.get("status").and_then(|v| v.as_str()) == Some("stopped") {
            continue;
        }
        let wf = WorkflowProvenance {
            run: run.page_id.clone(),
            wf_skill: wf_skill.to_owned(),
            phase: "recover".to_owned(),
            ..Default::default()
        };
        let expanded = client
            .expand(ExpandRequest {
                page_id: skill_page_id(&wf.wf_skill),
                ..Default::default()
            })
            .await
            .map_err(WorkflowDriveError::Read)?;
        // Recovery skips a run whose plan is missing or unparseable — the
        // failing-plan status is recorded on the live drive (F3), not here.
        let Ok(Some(spec)) =
            WorkflowSkill::parse_page(&wf.wf_skill, &expanded.frontmatter, &expanded.body)
        else {
            continue;
        };
        check_budget(&spec, max_runs_per_root)?;
        // Never re-drive an operation that already recorded a TERMINAL failure or
        // human hold. Its failing step's authored Stop/AskHuman transition is the
        // truth, and recovery cannot see that from the run STATE alone — an
        // incomplete state (a step that failed and stopped) looks identical to
        // "not started yet". Without this guard, recovery re-emits the failing
        // step and records `running`, overwriting the real terminal — so a
        // `failed`/`awaiting_human` operation silently flips back to `running`.
        if operation_has_terminal_status(client, &wf.run).await {
            continue;
        }
        let state = build_run_state(client, &wf, &spec).await?;
        // F7: re-establish the operation status on recovery — a run that
        // completed (or advanced) before a crash may never have recorded it.
        // A complete run records `succeeded`; a run with more steps `running`.
        // (A terminal `failed` is only known from the failing step's own
        // transition, not derivable from the run board here, so recovery never
        // overwrites a real terminal with `running`: it emits `running` only
        // when there is genuinely more to do.)
        if is_complete(&spec, &state) {
            record_status_best_effort(
                client,
                &wf.run,
                "recover",
                OperationStatus::Succeeded,
                "",
                "",
            )
            .await;
            continue; // run already complete — nothing to re-emit
        }
        let intents = reduce(&spec, &state);
        if intents.is_empty() {
            continue; // no steps to emit this pass (e.g. a barrier awaiting votes)
        }
        // Recovery has no parent trigger; each re-emitted step is its own
        // root at depth 0 (a fresh lineage), which `admit` treats like any
        // webhook-origin event. §3.6 keys keep the re-emit idempotent.
        emit_intents(client, &intents, |intent| root_provenance(&wf, intent)).await?;
        record_status_best_effort(client, &wf.run, "recover", OperationStatus::Running, "", "")
            .await;
        resumed += 1;
    }
    Ok(resumed)
}

/// Build a [`RunState`] for `wf` by reading each phase's produced instances
/// (run-scoped by the deterministic pre-flagged page-id prefix, `§3.6`) and
/// projecting any `verify-vote` frontmatter into barrier votes.
async fn build_run_state(
    client: &Client,
    wf: &WorkflowProvenance,
    spec: &WorkflowSkill,
) -> Result<RunState, WorkflowDriveError> {
    let run_slug = key::run_slug(&wf.run);
    // Load the instances of every skill the plan reads. A skill a phase
    // `produces` is **run-scoped** — filtered to this run by the deterministic
    // `<run_slug>-` pre-flagged page-id prefix. A skill that is only fanned
    // `over` (produced by no phase — e.g. `eval`'s `eval-task` benchmark set) is
    // **externally supplied** and shared across runs, so it is loaded whole
    // (no prefix filter); without this a leading `over` phase over a persistent
    // input set would see an empty upstream and vacuously "complete".
    let produces_skills: BTreeSet<&str> = spec.phases.iter().map(|p| p.produces.as_str()).collect();
    let mut load_skills: BTreeSet<&str> = produces_skills.clone();
    for phase in &spec.phases {
        if let FanOut::Over { over, .. } = &phase.fan_out {
            load_skills.insert(over.as_str());
        }
    }
    let mut produced: BTreeMap<String, Vec<ProducedInstance>> = BTreeMap::new();
    let mut votes: Vec<Vote> = Vec::new();
    for skill in load_skills {
        let resp = client
            .list_instances(ListInstancesRequest {
                skill: skill.to_owned(),
                ..Default::default()
            })
            .await
            .map_err(WorkflowDriveError::Read)?;
        let prefix = format!("markdown/instances/{skill}/{run_slug}-");
        let external = !produces_skills.contains(skill);
        let scoped: Vec<InstanceInfo> = resp
            .instances
            .into_iter()
            .filter(|i| external || i.page_id.starts_with(&prefix))
            .collect();
        if skill == "verify-vote" {
            votes.extend(scoped.iter().filter_map(vote_from_instance));
        }
        produced.insert(
            skill.to_owned(),
            scoped
                .into_iter()
                .map(|i| ProducedInstance {
                    page_id: i.page_id,
                    frontmatter: i.frontmatter,
                })
                .collect(),
        );
    }

    // Durable-target weave (compile-first-wiki G1): for each `writes: existing`
    // phase, resolve its distinct target pages from the upstream elements'
    // `target_field` and `expand` each so the reducer can read the
    // `source_event` completion stamp. A target that does not exist yet (an
    // `action: create` weave not landed) simply stays absent → not-yet-woven.
    let mut targets: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    for phase in &spec.phases {
        let (FanOut::Over { over, .. }, WriteMode::Existing { target_field }) =
            (&phase.fan_out, &phase.writes)
        else {
            continue;
        };
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for inst in produced.get(over).into_iter().flatten() {
            let Some(tp) = inst
                .frontmatter
                .get(target_field)
                .and_then(|v| v.as_str())
                .map(str::to_owned)
            else {
                continue;
            };
            if !seen.insert(tp.clone()) {
                continue;
            }
            if let Ok(expanded) = client
                .expand(ExpandRequest {
                    page_id: tp.clone(),
                    ..Default::default()
                })
                .await
            {
                targets.insert(tp, expanded.frontmatter);
            }
        }
    }

    Ok(RunState {
        run: wf.run.clone(),
        wf_skill: wf.wf_skill.clone(),
        produced,
        emitted: BTreeSet::new(),
        votes,
        // The ledger read of the barrier's terminal (dead-lettered) vote steps
        // is layered on when the verify phase runs against a real harness; the
        // happy path (every vote cast) closes on the vote instances alone.
        deadlettered: BTreeMap::new(),
        targets,
    })
}

/// Emit each intent as an idempotent, lineage-tagged step event. `provenance`
/// builds the `provenance` object for each step (a driver hop extends the
/// trigger's lineage; recovery mints a fresh root).
async fn emit_intents(
    client: &Client,
    intents: &[StepIntent],
    provenance: impl Fn(&StepIntent) -> serde_json::Value,
) -> Result<Vec<String>, WorkflowDriveError> {
    let mut emitted = Vec::with_capacity(intents.len());
    for intent in intents {
        let event_id = intent.event_id();
        client
            .capture_event(CaptureEventRequest {
                event_id: event_id.clone(),
                source: "escurel-runner".to_owned(),
                mime: "text/plain".to_owned(),
                label_skill: intent.produces.clone(),
                instance_page_id: intent.instance_page_id(),
                title: format!("workflow {} · {} step", intent.wf_skill, intent.phase),
                body: format!(
                    "Workflow {} run {} phase {} slot {}.",
                    intent.wf_skill, intent.run, intent.phase, intent.slot
                ),
                provenance: provenance(intent),
                ..Default::default()
            })
            .await
            .map_err(WorkflowDriveError::Capture)?;
        emitted.push(event_id);
    }
    Ok(emitted)
}

/// A fresh root `provenance` for a recovery-emitted step: its own event id is
/// the root at depth 0, with the `workflow` block carrying the step identity.
fn root_provenance(wf: &WorkflowProvenance, intent: &StepIntent) -> serde_json::Value {
    let event_id = intent.event_id();
    json!({
        "runner": {
            "root_event_id": event_id,
            "depth": 0,
            "lineage_path": [event_id],
            "instance_path": [],
            "cause": format!("workflow-recover:{}", wf.wf_skill),
        },
        "workflow": intent.provenance(),
    })
}

/// Build the emitted step's `provenance` — the `runner` lineage (so `admit`'s
/// depth/cycle guards apply exactly as for a cascade) plus the
/// `workflow` block carrying the step identity.
fn build_step_provenance(
    parent_trigger: &Trigger,
    parent_run_id: &str,
    effect: Option<&ConfirmedEffect>,
    intent: &StepIntent,
) -> serde_json::Value {
    let parent = &parent_trigger.lineage;
    let depth = parent.depth + 1;
    let mut lineage_path = parent.lineage_path.clone();
    if lineage_path.last().map(String::as_str) != Some(parent_trigger.event_id.as_str()) {
        lineage_path.push(parent_trigger.event_id.clone());
    }
    // A confirmed write extends the instance path with the page it produced; a
    // converged/held terminal has no such page, so the path is inherited as-is.
    let mut instance_path = parent.instance_path.clone();
    if let Some(effect) = effect
        && instance_path.last() != Some(&effect.instance_page_id)
    {
        instance_path.push(effect.instance_page_id.clone());
    }
    let mut runner = serde_json::Map::new();
    runner.insert("root_event_id".into(), json!(parent.root_event_id));
    runner.insert("parent_event_id".into(), json!(parent_trigger.event_id));
    runner.insert("parent_run_id".into(), json!(parent_run_id));
    runner.insert("depth".into(), json!(depth));
    runner.insert("lineage_path".into(), json!(lineage_path));
    runner.insert("instance_path".into(), json!(instance_path));
    if let Some(trace_id) = &parent.trace_id {
        runner.insert("trace_id".into(), json!(trace_id));
    }
    json!({ "runner": runner, "workflow": intent.provenance() })
}
