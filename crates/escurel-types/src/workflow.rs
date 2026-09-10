//! The `provenance.workflow` block — shared vocabulary for dynamic
//! workflows (`docs/contract/dynamic-workflows.md` §3.3).
//!
//! A workflow *step* is an ordinary `capture_event` whose `provenance`
//! carries this block alongside the existing `provenance.runner` lineage.
//! It is the wire contract read by both the runner's `Trigger::from_event`
//! (to recognise a workflow-driven hop) and the `escurel-runner-workflow`
//! reducer (to decide the next batch of steps). Because it is pure
//! vocabulary — no logic — it lives here in `escurel-types`, shared by the
//! runner engine and the reducer without either depending on the other.

use serde::{Deserialize, Serialize};

/// The `provenance.workflow` object stamped onto a workflow step event.
///
/// Every field except `barrier`/`over` is always present on a step event;
/// `barrier` is set only for steps that participate in a quorum barrier
/// (the verify phase), and `over` records the per-item routing target for a
/// fan-out / pipeline step (`§3.3`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct WorkflowProvenance {
    /// The `workflow-run` instance page id this step belongs to.
    pub run: String,
    /// The `kind: workflow` skill id that is the run's plan.
    pub wf_skill: String,
    /// The phase id this step executes (e.g. `scope`, `verify`).
    pub phase: String,
    /// The deterministic step id — the `§3.6` `step_key` (also the event id).
    pub step: String,
    /// The barrier id this step participates in, if any (e.g. `verify`).
    /// Empty when the step is not part of a barrier.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub barrier: String,
    /// The per-item routing target of a fan-out / pipeline step — the
    /// element the step is executing "over" (e.g. `[[claim::c12]]` or a
    /// source page id). Empty for a width-1 phase.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub over: String,
    /// The vote slot `0..votes_per_claim` a barrier (verify) step occupies —
    /// the index a `verify-vote` harness must stamp into its instance so that
    /// distinct skeptics tally as distinct votes (`§3.5`). Without it a harness
    /// cannot recover its slot from the content-addressed step id, and every
    /// skeptic would collide on the same `vote_index`, wedging the barrier at
    /// one vote. `None` for any non-barrier step.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vote_index: Option<u32>,
    /// The harness this step declared (`harness:` on the phase, else on the
    /// plan). Empty when the plan declares none, which means the runner's own
    /// `ESCUREL_RUNNER_HARNESS` decides — the ordinary case.
    ///
    /// Carried on the EVENT rather than looked up at dispatch because the plan
    /// that chose it is read once, when the step is emitted: a plan edited
    /// mid-run must not re-route steps already in flight.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub harness: String,
}

impl WorkflowProvenance {
    /// Read the `workflow` block back out of an event's `provenance` JSON.
    /// Returns `None` when the event carries no `provenance.workflow`
    /// (every non-workflow event — a webhook-origin or a plain cascade hop).
    #[must_use]
    pub fn from_provenance(provenance: &serde_json::Value) -> Option<Self> {
        let block = provenance.get("workflow")?;
        // A malformed block is treated as absent rather than panicking —
        // the read path stays lenient, like `provenance.runner`.
        serde_json::from_value(block.clone()).ok()
    }
}

/// The reserved `label_skill` an operation's status events are recorded under.
/// A KB-visible record, never a dispatchable run — the runner's enqueue
/// chokepoint drops any trigger carrying it, and the `escurel:` prefix is a
/// reserved namespace a tenant cannot author. Shared vocabulary: the runner
/// writes it, the gateway's `get_operation` reads it.
pub const OPERATION_STATUS_LABEL: &str = "escurel:run-status";

/// The status of an async operation, as recorded on its run board (as
/// `provenance.run_status` on a reserved status event) and read back by
/// `get_operation`. One shared vocabulary for the writer (the runner's driver)
/// and every reader (the gateway's `get_operation`), living here in
/// `escurel-types` so neither depends on the other (crew F-10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationStatus {
    /// Accepted, not yet running.
    Pending,
    /// At least one phase is in flight (or awaiting re-drive).
    Running,
    /// Every phase is complete.
    Succeeded,
    /// A step failed terminally and its phase's policy is to stop.
    Failed,
    /// Paused for a human decision (a held draft, or an `AskHuman` fallback).
    AwaitingHuman,
}

impl OperationStatus {
    /// The stable wire/KB string (the `provenance.run_status` value + title).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            OperationStatus::Pending => "pending",
            OperationStatus::Running => "running",
            OperationStatus::Succeeded => "succeeded",
            OperationStatus::Failed => "failed",
            OperationStatus::AwaitingHuman => "awaiting_human",
        }
    }

    /// Parse the wire string back (the inverse of [`Self::as_str`]); `None`
    /// for an unrecognised value, so a reader ignores a status it does not know.
    #[must_use]
    pub fn from_wire(s: &str) -> Option<Self> {
        Some(match s {
            "pending" => OperationStatus::Pending,
            "running" => OperationStatus::Running,
            "succeeded" => OperationStatus::Succeeded,
            "failed" => OperationStatus::Failed,
            "awaiting_human" => OperationStatus::AwaitingHuman,
            _ => return None,
        })
    }

    /// Terminal precedence for `get_operation`'s derivation: when an operation's
    /// append-only history carries several statuses, the highest rank is the
    /// current one. `Failed` outranks `Succeeded` (a failed phase means the plan
    /// did not wholly succeed); `AwaitingHuman` outranks `Running` (a pause is
    /// more specific than "in flight"); `Running` outranks `Pending`.
    #[must_use]
    pub fn precedence(self) -> u8 {
        match self {
            OperationStatus::Pending => 0,
            OperationStatus::Running => 1,
            OperationStatus::AwaitingHuman => 2,
            OperationStatus::Succeeded => 3,
            OperationStatus::Failed => 4,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn round_trips_through_provenance() {
        let wf = WorkflowProvenance {
            run: "markdown/instances/workflow-run/r1.md".to_owned(),
            wf_skill: "deep-research".to_owned(),
            phase: "verify".to_owned(),
            step: "01HSTEPKEY".to_owned(),
            barrier: "verify".to_owned(),
            over: "[[claim::c12]]".to_owned(),
            vote_index: Some(2),
            harness: "claude".to_owned(),
        };
        let provenance = json!({ "workflow": wf });
        assert_eq!(
            WorkflowProvenance::from_provenance(&provenance),
            Some(wf.clone())
        );
    }

    #[test]
    fn absent_workflow_block_is_none() {
        assert_eq!(
            WorkflowProvenance::from_provenance(&json!({ "runner": { "depth": 0 } })),
            None
        );
        assert_eq!(WorkflowProvenance::from_provenance(&json!(null)), None);
    }

    #[test]
    fn width_one_step_omits_barrier_and_over() {
        // A width-1 phase step carries no barrier/over; those keys are
        // skipped on the wire but round-trip to empty strings.
        let wf = WorkflowProvenance {
            run: "r1".to_owned(),
            wf_skill: "deep-research".to_owned(),
            phase: "scope".to_owned(),
            step: "01HSTEP".to_owned(),
            ..Default::default()
        };
        let v = serde_json::to_value(&wf).unwrap();
        assert!(v.get("barrier").is_none(), "empty barrier is skipped");
        assert!(v.get("over").is_none(), "empty over is skipped");
        assert!(v.get("vote_index").is_none(), "None vote_index is skipped");
        assert_eq!(
            WorkflowProvenance::from_provenance(&json!({ "workflow": v })),
            Some(wf)
        );
    }

    #[test]
    fn barrier_step_carries_its_vote_index() {
        // A verify (barrier) step's provenance pins the skeptic's slot so the
        // harness stamps a distinct `vote_index` per vote.
        let wf = WorkflowProvenance {
            run: "r1".to_owned(),
            wf_skill: "deep-research".to_owned(),
            phase: "verify".to_owned(),
            step: "01HVOTE".to_owned(),
            barrier: "verify".to_owned(),
            over: "markdown/instances/claims/r1-extract-abc.md".to_owned(),
            vote_index: Some(1),
            harness: String::new(),
        };
        let v = serde_json::to_value(&wf).unwrap();
        assert_eq!(v.get("vote_index").and_then(|x| x.as_u64()), Some(1));
        assert_eq!(
            WorkflowProvenance::from_provenance(&json!({ "workflow": v })),
            Some(wf)
        );
    }
}
