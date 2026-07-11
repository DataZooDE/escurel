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
    BudgetExceeded, ProducedInstance, RunState, StepIntent, Vote, WorkflowSkill, check_budget, key,
    reduce,
};
use escurel_types::{
    CaptureEventRequest, ExpandRequest, InstanceInfo, ListInstancesRequest, WorkflowProvenance,
};
use serde_json::json;

use crate::reconciler::ConfirmedEffect;
use crate::trigger::Trigger;

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

/// Run one reducer pass for the workflow the confirmed `trigger` belongs to,
/// emitting the next batch of step events. Returns an empty outcome when the
/// run has no more steps (the plan is complete) or the labelled skill is not
/// a workflow plan.
pub async fn drive_workflow(
    client: &Client,
    trigger: &Trigger,
    parent_run_id: &str,
    effect: &ConfirmedEffect,
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
    Ok(WorkflowDriveOutcome { emitted })
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
        let intents = reduce(&spec, &state);
        if intents.is_empty() {
            continue; // run already complete
        }
        // Recovery has no parent trigger; each re-emitted step is its own
        // root at depth 0 (a fresh lineage), which `admit` treats like any
        // webhook-origin event. §3.6 keys keep the re-emit idempotent.
        emit_intents(client, &intents, |intent| root_provenance(&wf, intent)).await?;
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
    let produces_skills: BTreeSet<&str> = spec.phases.iter().map(|p| p.produces.as_str()).collect();
    let mut produced: BTreeMap<String, Vec<ProducedInstance>> = BTreeMap::new();
    let mut votes: Vec<Vote> = Vec::new();
    for skill in produces_skills {
        let resp = client
            .list_instances(ListInstancesRequest {
                skill: skill.to_owned(),
                ..Default::default()
            })
            .await
            .map_err(WorkflowDriveError::Read)?;
        let prefix = format!("markdown/instances/{skill}/{run_slug}-");
        let run_scoped: Vec<InstanceInfo> = resp
            .instances
            .into_iter()
            .filter(|i| i.page_id.starts_with(&prefix))
            .collect();
        if skill == "verify-vote" {
            votes.extend(run_scoped.iter().filter_map(vote_from_instance));
        }
        produced.insert(
            skill.to_owned(),
            run_scoped
                .into_iter()
                .map(|i| ProducedInstance { page_id: i.page_id })
                .collect(),
        );
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
    effect: &ConfirmedEffect,
    intent: &StepIntent,
) -> serde_json::Value {
    let parent = &parent_trigger.lineage;
    let depth = parent.depth + 1;
    let mut lineage_path = parent.lineage_path.clone();
    if lineage_path.last().map(String::as_str) != Some(parent_trigger.event_id.as_str()) {
        lineage_path.push(parent_trigger.event_id.clone());
    }
    let mut instance_path = parent.instance_path.clone();
    if instance_path.last() != Some(&effect.instance_page_id) {
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
