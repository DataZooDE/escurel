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
use escurel_runner_workflow::{ProducedInstance, RunState, StepIntent, WorkflowSkill, key, reduce};
use escurel_types::{CaptureEventRequest, ExpandRequest, ListInstancesRequest};
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
}

/// Skill page id for a plan skill id (`markdown/skills/<id>.md`).
fn skill_page_id(skill: &str) -> String {
    format!("markdown/skills/{skill}.md")
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

    // 2. Build the run state: each phase's produced instances, run-scoped by
    //    the deterministic pre-flagged page-id prefix (`§3.6`).
    let run_slug = key::run_slug(&wf.run);
    let produces_skills: BTreeSet<&str> = spec.phases.iter().map(|p| p.produces.as_str()).collect();
    let mut produced: BTreeMap<String, Vec<ProducedInstance>> = BTreeMap::new();
    for skill in produces_skills {
        let resp = client
            .list_instances(ListInstancesRequest {
                skill: skill.to_owned(),
                ..Default::default()
            })
            .await
            .map_err(WorkflowDriveError::Read)?;
        let prefix = format!("markdown/instances/{skill}/{run_slug}-");
        let insts = resp
            .instances
            .into_iter()
            .filter(|i| i.page_id.starts_with(&prefix))
            .map(|i| ProducedInstance { page_id: i.page_id })
            .collect();
        produced.insert(skill.to_owned(), insts);
    }
    let state = RunState {
        run: wf.run.clone(),
        wf_skill: wf.wf_skill.clone(),
        produced,
        emitted: BTreeSet::new(),
    };

    // 3. Plan the next batch (pure, deterministic).
    let intents = reduce(&spec, &state);

    // 4. Emit each step as an idempotent, lineage-tagged event.
    let mut emitted = Vec::with_capacity(intents.len());
    for intent in intents {
        let event_id = intent.event_id();
        let provenance = build_step_provenance(trigger, parent_run_id, effect, &intent);
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
                provenance,
                ..Default::default()
            })
            .await
            .map_err(WorkflowDriveError::Capture)?;
        emitted.push(event_id);
    }
    Ok(WorkflowDriveOutcome { emitted })
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
