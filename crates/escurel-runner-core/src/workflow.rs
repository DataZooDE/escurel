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
    BudgetExceeded, Fallback, FanOut, OutcomePolicy, ProducedInstance, RunState, StepIntent, Vote,
    WorkflowSkill, WriteMode, check_budget, is_complete, key, reduce,
};
use escurel_types::{
    AssignEventRequest, CaptureEventRequest, ExpandRequest, InstanceInfo, ListInstancesRequest,
    WorkflowProvenance,
};
use serde_json::json;

use crate::reconciler::ConfirmedEffect;
use crate::trigger::Trigger;

/// The reserved `label_skill` under which an operation's status is recorded on
/// its run board (async-ops Phase 0.2). It is a KB-visible record, **never a
/// dispatchable run**: the runner's enqueue chokepoint (`gate_and_enqueue`)
/// drops any trigger carrying this label before a ledger row is created, so a
/// status event can neither spawn a run nor re-enter the reducer. No authored
/// skill may use this label (Phase 1 sanitisation will reject a caller who
/// tries to capture one).
pub const OPERATION_STATUS_LABEL: &str = "run-status";

/// Outcome of driving one reducer pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkflowDriveOutcome {
    /// The event ids emitted this pass (empty when the run is complete or the
    /// trigger's skill is not a workflow plan).
    pub emitted: Vec<String>,
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
    /// The step failed terminally (retries exhausted, bad output, or a
    /// permanent failure). The failed phase's `on_exhausted` policy decides
    /// the operation's fate.
    Failed,
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

    // 1. Load the immutable plan from the workflow skill page's frontmatter.
    let expanded = client
        .expand(ExpandRequest {
            page_id: skill_page_id(&wf.wf_skill),
            ..Default::default()
        })
        .await
        .map_err(WorkflowDriveError::Read)?;
    let Some(spec) = WorkflowSkill::parse(&expanded.frontmatter) else {
        return Ok(WorkflowDriveOutcome::default());
    };

    // A held draft pauses the operation for human approval; the plan must not
    // advance past it, so record `awaiting_human` and emit nothing.
    if terminal == StepTerminal::Held {
        record_status_best_effort(client, &wf.run, "awaiting_human").await;
        return Ok(WorkflowDriveOutcome::default());
    }

    // A terminal FAILURE is resolved from the failed phase's authored policy
    // BEFORE any emit — a Stop/AskHuman step must not re-emit itself into a
    // retry loop. `is_complete` guards the rare race where the failing step's
    // output nonetheless landed and every phase is already done.
    if terminal == StepTerminal::Failed {
        let state = build_run_state(client, wf, &spec).await?;
        if is_complete(&spec, &state) {
            record_status_best_effort(client, &wf.run, "succeeded").await;
            return Ok(WorkflowDriveOutcome::default());
        }
        let policy = phase_outcome(&spec, &wf.phase);
        let status = match policy.on_exhausted {
            Fallback::AskHuman => "awaiting_human",
            // Skip is not yet honoured (Phase 0.3b); fail closed rather than
            // silently advance past a dropped step.
            Fallback::Stop | Fallback::Skip => "failed",
        };
        if policy.on_exhausted == Fallback::Skip {
            tracing::warn!(
                target: "escurel_runner",
                operation = %wf.run,
                phase = %wf.phase,
                "workflow: authored `skip` fallback not yet honoured (Phase 0.3b); failing closed"
            );
        }
        record_status_best_effort(client, &wf.run, status).await;
        return Ok(WorkflowDriveOutcome::default());
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
    // derives the current status as the latest such event.
    let status = if is_complete(&spec, &state) {
        "succeeded"
    } else {
        "running"
    };
    record_status_best_effort(client, &wf.run, status).await;
    Ok(WorkflowDriveOutcome { emitted })
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
/// carry on.
async fn record_status_best_effort(client: &Client, operation: &str, status: &str) {
    if let Err(e) = record_status(client, operation, status).await {
        tracing::warn!(
            target: "escurel_runner",
            operation = %operation,
            status,
            error = %e,
            "workflow: recording operation status failed (best-effort); run unaffected"
        );
    }
}

/// Record the operation's status as a **processed, assigned** event on the run
/// board — the record `get_operation` reads.
///
/// Two properties make this safe, and neither may be silently dropped:
///
/// - **Fail-closed against dispatch (F1).** The event is captured under the
///   reserved [`OPERATION_STATUS_LABEL`], which the runner's enqueue chokepoint
///   (`gate_and_enqueue`) refuses before creating a ledger row. The prior code
///   relied on `assign_event` racing ahead of the poller/webhook to move the
///   event out of the inbox; it does not — `capture_event` lands it `inbox`,
///   and the webhook's synchronous notify (and the next poll tick) enqueue it
///   before `assign_event` runs, spawning a dead-lettered run per transition.
///   The label guard closes that window regardless of timing.
/// - **Time-ordered (F2).** `at` is stamped so the board's history orders by
///   wall-clock, not by the status event's content-addressed id (which is not
///   monotonic). `get_operation` still resolves ties by status precedence
///   rather than trusting last-write ordering alone.
///
/// The id is a deterministic function of `(operation, status)` so
/// `capture_event`'s `ON CONFLICT DO NOTHING` makes it emit-once per status.
async fn record_status(
    client: &Client,
    operation: &str,
    status: &str,
) -> Result<(), WorkflowDriveError> {
    let event_id = key::step_event_id(operation, OPERATION_STATUS_LABEL, status);
    // DuckDB casts this via `TRY_CAST(? AS TIMESTAMP)`; match the space-separated
    // microsecond format `paged_events` reads back so the round-trip is exact.
    let at = chrono::Utc::now()
        .format("%Y-%m-%d %H:%M:%S%.6f")
        .to_string();
    client
        .capture_event(CaptureEventRequest {
            event_id: event_id.clone(),
            at,
            source: "escurel-runner".to_owned(),
            mime: "text/plain".to_owned(),
            label_skill: OPERATION_STATUS_LABEL.to_owned(),
            instance_page_id: operation.to_owned(),
            title: format!("status: {status}"),
            body: String::new(),
            provenance: json!({ "run_status": status }),
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
        let Some(spec) = WorkflowSkill::parse(&expanded.frontmatter) else {
            continue;
        };
        check_budget(&spec, max_runs_per_root)?;
        let state = build_run_state(client, &wf, &spec).await?;
        // F7: re-establish the operation status on recovery — a run that
        // completed (or advanced) before a crash may never have recorded it.
        // A complete run records `succeeded`; a run with more steps `running`.
        // (A terminal `failed` is only known from the failing step's own
        // transition, not derivable from the run board here, so recovery never
        // overwrites a real terminal with `running`: it emits `running` only
        // when there is genuinely more to do.)
        if is_complete(&spec, &state) {
            record_status_best_effort(client, &wf.run, "succeeded").await;
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
        record_status_best_effort(client, &wf.run, "running").await;
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
