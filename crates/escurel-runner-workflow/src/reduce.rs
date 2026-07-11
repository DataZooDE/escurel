//! The pure reducer — the core new component (`§3.4`).
//!
//! `reduce` is the generalization of `emit_cascade`: where `emit_cascade`
//! is a fixed policy returning ≤1 event, `reduce` is a plan-driven policy
//! returning a *set* of steps (possibly zero). It is a **pure planner** —
//! it calls no LLM, performs no I/O, and does no write reasoning. Its only
//! job is control flow: sequence phases, fan out, and decide termination.
//! All intelligence lives inside the harness runs whose outputs it reads
//! back as [`RunState`].
//!
//! Determinism is the contract (the escurel analogue of deep-research's
//! `Date.now`/`random` ban): **no wall-clock, no randomness**. Given the
//! same `(spec, state)` it returns the same intents — the property that
//! makes replay-based resume correct.
//!
//! The dispatch loop (a later PR) does the I/O: it fetches each phase's
//! produced instances via `list_instances(<produces>, {workflow_run: run})`
//! and the emitted step ids from the ledger, packs them into a [`RunState`],
//! calls `reduce`, and `capture_event`s each returned [`StepIntent`].
//!
//! ## PR-3 scope (this commit)
//!
//! Linear (width-1) phase sequencing plus the basic `fan_out: { over }`
//! enumeration. The quorum **barrier** (verify) close semantics — the
//! `list_instances` ∪ ledger-terminal tally and the claim-ref flattening —
//! land with PR-5; `reduce` here treats a phase as complete once every
//! expected step's pre-flagged instance is present.

use std::collections::{BTreeMap, BTreeSet};

use crate::spec::{FanOut, Phase, WorkflowSkill};
use crate::step::StepIntent;

/// One produced instance as the reducer observes it — the projection of a
/// `list_instances(<produces>, {workflow_run: run})` row the caller fetched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProducedInstance {
    /// The instance's page id. A step pre-flags its target
    /// (`§3.6`), so a produced page id equals the emitting step's
    /// `StepIntent::instance_page_id` — that identity is how the reducer
    /// matches outputs to expected steps (and distinguishes two phases that
    /// share a `produces` skill, e.g. `search` and `fetch` both writing
    /// `source`).
    pub page_id: String,
}

/// The observable state of a run the pure reducer plans over.
///
/// This is everything `reduce` reads: the run identity, the produced
/// instances per `produces` **skill** (as `list_instances` returns them),
/// and the set of step ids already emitted (any status). Nothing here is
/// fetched by `reduce` itself — the caller supplies it, keeping `reduce`
/// pure and table-testable.
#[derive(Debug, Clone, Default)]
pub struct RunState {
    /// The `workflow-run` instance page id.
    pub run: String,
    /// The `kind: workflow` plan skill id.
    pub wf_skill: String,
    /// `produced[skill_id]` = instances of that skill written by this run's
    /// steps, keyed by the `produces` skill (NOT the phase id — two phases
    /// may share a skill; the reducer disambiguates by pre-flagged page id).
    pub produced: BTreeMap<String, Vec<ProducedInstance>>,
    /// Step ids already emitted for this run (any status), so a re-run of
    /// `reduce` does not re-return a step whose instance has not landed yet.
    pub emitted: BTreeSet<String>,
}

impl RunState {
    /// The produced instance page ids for `skill`.
    fn produced_page_ids(&self, skill: &str) -> BTreeSet<&str> {
        self.produced
            .get(skill)
            .into_iter()
            .flatten()
            .map(|i| i.page_id.as_str())
            .collect()
    }
}

/// Decide the next batch of steps to emit — or empty when the run is done.
///
/// Walks the phases in order and returns the first incomplete phase's
/// not-yet-emitted steps. A phase is **complete** when every step it plans
/// has its pre-flagged instance present; a phase with steps emitted but not
/// yet produced is *in flight* and yields an empty batch (the reducer waits
/// rather than re-emitting or advancing). When every phase is complete the
/// run is done and the batch is empty.
#[must_use]
pub fn reduce(spec: &WorkflowSkill, state: &RunState) -> Vec<StepIntent> {
    for phase in &spec.phases {
        let expected = plan_phase(spec, phase, state, &state.run);
        if expected.is_empty() {
            // Nothing to plan yet (an `over` phase whose upstream has not
            // produced anything): this phase cannot advance, so the run
            // waits here rather than skipping ahead.
            if phase_has_upstream_pending(spec, phase, state) {
                return Vec::new();
            }
            continue;
        }
        let produced = state.produced_page_ids(&phase.produces);
        let complete = expected
            .iter()
            .all(|intent| produced.contains(intent.instance_page_id().as_str()));
        if complete {
            continue;
        }
        // Incomplete: emit the steps not already emitted. May be empty when
        // every step is in flight (emitted, awaiting its instance).
        return expected
            .into_iter()
            .filter(|intent| !state.emitted.contains(&intent.event_id()))
            .collect();
    }
    Vec::new()
}

/// The full set of steps a phase plans, given the run's current state. Pure:
/// a `Fixed` phase yields `n` slots `0..n`; an `over` phase yields one slot
/// per upstream produced instance (× `width` for a barrier phase).
fn plan_phase(spec: &WorkflowSkill, phase: &Phase, state: &RunState, run: &str) -> Vec<StepIntent> {
    match &phase.fan_out {
        FanOut::Fixed(n) => (0..*n)
            .map(|i| intent(spec, phase, run, i.to_string(), None, None))
            .collect(),
        FanOut::Over { over, width } => {
            let mut upstream: Vec<&str> = upstream_instances(over, state);
            upstream.sort_unstable(); // deterministic order
            let barrier = (*width > 1).then(|| phase.id.clone());
            upstream
                .into_iter()
                .flat_map(|page_id| {
                    let elem = element_slug(page_id);
                    let barrier = barrier.clone();
                    (0..*width).map(move |k| {
                        let slot = if *width > 1 {
                            format!("{elem}-v{k}")
                        } else {
                            elem.clone()
                        };
                        intent(
                            spec,
                            phase,
                            run,
                            slot,
                            Some(page_id.to_owned()),
                            barrier.clone(),
                        )
                    })
                })
                .collect()
        }
    }
}

/// The instances a `fan_out: { over: <skill> }` phase fans out across: the
/// produced instances of the phase whose `produces` equals `over`.
fn upstream_instances<'a>(over: &str, state: &'a RunState) -> Vec<&'a str> {
    state
        .produced
        .get(over)
        .into_iter()
        .flatten()
        .map(|i| i.page_id.as_str())
        .collect()
}

/// True when `phase` fans out over an upstream skill that has not produced
/// anything yet AND that upstream phase is itself not yet complete — i.e.
/// the run must wait here. A `Fixed` phase never has upstream-pending.
fn phase_has_upstream_pending(spec: &WorkflowSkill, phase: &Phase, state: &RunState) -> bool {
    let FanOut::Over { over, .. } = &phase.fan_out else {
        return false;
    };
    if !upstream_instances(over, state).is_empty() {
        return false; // upstream produced something → plan_phase is non-empty
    }
    // Upstream empty: wait only if some earlier phase is the producer of
    // `over` (i.e. it is expected to fill in). If nothing in the plan
    // produces `over`, there is genuinely nothing to wait for.
    spec.phases.iter().any(|p| &p.produces == over)
}

/// Build one [`StepIntent`] for `phase` at `slot` within `run`.
fn intent(
    spec: &WorkflowSkill,
    phase: &Phase,
    run: &str,
    slot: String,
    over: Option<String>,
    barrier: Option<String>,
) -> StepIntent {
    StepIntent {
        run: run.to_owned(),
        wf_skill: spec.id.clone(),
        phase: phase.id.clone(),
        produces: phase.produces.clone(),
        slot,
        barrier,
        over,
    }
}

/// A short, filename-safe element token for a fan-out slot: the last path
/// segment of a page id, sans `.md`.
fn element_slug(page_id: &str) -> String {
    page_id
        .rsplit('/')
        .next()
        .unwrap_or(page_id)
        .strip_suffix(".md")
        .unwrap_or_else(|| page_id.rsplit('/').next().unwrap_or(page_id))
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const RUN: &str = "markdown/instances/workflow-run/r1.md";

    /// A linear two-phase plan: scope (fan_out 1) → synthesize (fan_out 1).
    fn linear_spec() -> WorkflowSkill {
        WorkflowSkill::parse(&json!({
            "id": "deep-research",
            "phases": [
                { "id": "scope", "produces": "research-angle", "fan_out": 1 },
                { "id": "synthesize", "produces": "research-report", "fan_out": 1 }
            ]
        }))
        .unwrap()
    }

    fn state_with(produced: &[(&str, &[&str])], emitted: &[&str]) -> RunState {
        let mut map: BTreeMap<String, Vec<ProducedInstance>> = BTreeMap::new();
        for (skill, pages) in produced {
            map.insert(
                (*skill).to_owned(),
                pages
                    .iter()
                    .map(|p| ProducedInstance {
                        page_id: (*p).to_owned(),
                    })
                    .collect(),
            );
        }
        RunState {
            run: RUN.to_owned(),
            wf_skill: "deep-research".to_owned(),
            produced: map,
            emitted: emitted.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    #[test]
    fn empty_run_emits_the_first_phase() {
        let spec = linear_spec();
        let batch = reduce(&spec, &state_with(&[], &[]));
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].phase, "scope");
        assert_eq!(batch[0].produces, "research-angle");
        assert_eq!(batch[0].run, RUN);
        assert_eq!(batch[0].over, None);
        assert_eq!(batch[0].barrier, None);
    }

    #[test]
    fn reduce_is_deterministic() {
        let spec = linear_spec();
        let state = state_with(&[], &[]);
        assert_eq!(reduce(&spec, &state), reduce(&spec, &state));
    }

    #[test]
    fn emitted_but_not_produced_phase_is_in_flight_and_waits() {
        let spec = linear_spec();
        let scope_step = reduce(&spec, &state_with(&[], &[])).remove(0);
        // The scope step was emitted but its instance hasn't landed yet.
        let batch = reduce(&spec, &state_with(&[], &[&scope_step.event_id()]));
        assert!(
            batch.is_empty(),
            "an in-flight phase re-emits nothing and does not advance"
        );
    }

    #[test]
    fn produced_first_phase_advances_to_the_second() {
        let spec = linear_spec();
        let scope_step = reduce(&spec, &state_with(&[], &[])).remove(0);
        // Scope produced its instance at the pre-flagged page id.
        let state = state_with(
            &[("research-angle", &[&scope_step.instance_page_id()])],
            &[&scope_step.event_id()],
        );
        let batch = reduce(&spec, &state);
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].phase, "synthesize");
        assert_eq!(batch[0].produces, "research-report");
    }

    #[test]
    fn all_phases_complete_terminates() {
        let spec = linear_spec();
        let scope = reduce(&spec, &state_with(&[], &[])).remove(0);
        let mid = state_with(
            &[("research-angle", &[&scope.instance_page_id()])],
            &[&scope.event_id()],
        );
        let synth = reduce(&spec, &mid).remove(0);
        let done = state_with(
            &[
                ("research-angle", &[&scope.instance_page_id()]),
                ("research-report", &[&synth.instance_page_id()]),
            ],
            &[&scope.event_id(), &synth.event_id()],
        );
        assert!(reduce(&spec, &done).is_empty(), "run is done");
    }

    #[test]
    fn fan_out_over_emits_one_step_per_upstream_instance() {
        // scope → search (fan_out over research-angle). Two angles produced ⇒
        // two search steps, each routed `over` its angle, no barrier (width 1).
        let spec = WorkflowSkill::parse(&json!({
            "id": "deep-research",
            "phases": [
                { "id": "scope", "produces": "research-angle", "fan_out": 1 },
                { "id": "search", "produces": "source",
                  "fan_out": { "over": "research-angle" } }
            ]
        }))
        .unwrap();
        let scope = reduce(&spec, &state_with(&[], &[])).remove(0);
        let angle_a = "markdown/instances/research-angle/a.md";
        let angle_b = "markdown/instances/research-angle/b.md";
        // Scope is complete (its pre-flagged instance is present); two angle
        // instances exist for search to fan out over.
        let state = state_with(
            &[(
                "research-angle",
                &[&scope.instance_page_id(), angle_a, angle_b],
            )],
            &[&scope.event_id()],
        );
        let batch = reduce(&spec, &state);
        // One search step per research-angle instance (3: the pre-flagged
        // scope target + the two extra angle pages).
        assert_eq!(batch.len(), 3);
        assert!(batch.iter().all(|s| s.phase == "search"));
        assert!(batch.iter().all(|s| s.over.is_some()));
        assert!(batch.iter().all(|s| s.barrier.is_none()));
        // Deterministic order: sorted by upstream page id.
        let overs: Vec<&str> = batch.iter().map(|s| s.over.as_deref().unwrap()).collect();
        let mut sorted = overs.clone();
        sorted.sort_unstable();
        assert_eq!(overs, sorted);
    }

    #[test]
    fn over_phase_waits_when_upstream_not_yet_produced() {
        // search fans out over research-angle, but scope hasn't produced yet:
        // reduce must emit scope (the producer), never skip to a dead search.
        let spec = WorkflowSkill::parse(&json!({
            "id": "deep-research",
            "phases": [
                { "id": "scope", "produces": "research-angle", "fan_out": 1 },
                { "id": "search", "produces": "source",
                  "fan_out": { "over": "research-angle" } }
            ]
        }))
        .unwrap();
        let batch = reduce(&spec, &state_with(&[], &[]));
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].phase, "scope");
    }

    #[test]
    fn no_wall_clock_or_random_ids_are_pure_functions_of_state() {
        // Two independent reduces of the same state yield byte-identical
        // event ids — the determinism the resume path depends on.
        let spec = linear_spec();
        let a = reduce(&spec, &state_with(&[], &[]));
        let b = reduce(&spec, &state_with(&[], &[]));
        assert_eq!(a[0].event_id(), b[0].event_id());
        assert_eq!(a[0].instance_page_id(), b[0].instance_page_id());
    }
}
