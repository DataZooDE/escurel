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

use serde_json::Value;

use crate::barrier::{self, BarrierInput, Vote};
use crate::key;
use crate::spec::{FanOut, Phase, WorkflowSkill, WriteMode};
use crate::step::StepIntent;

/// One produced instance as the reducer observes it — the projection of a
/// `list_instances(<produces>, {workflow_run: run})` row the caller fetched.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProducedInstance {
    /// The instance's page id. A step pre-flags its target
    /// (`§3.6`), so a produced page id equals the emitting step's
    /// `StepIntent::instance_page_id` — that identity is how the reducer
    /// matches outputs to expected steps (and distinguishes two phases that
    /// share a `produces` skill, e.g. `search` and `fetch` both writing
    /// `source`).
    pub page_id: String,
    /// The instance's frontmatter, as `list_instances` returns it. Read by a
    /// downstream [`WriteMode::Existing`] phase to resolve each element's
    /// durable `target_field` (e.g. a `weave` record's `target_page`); `Null`
    /// for callers/tests that don't need it.
    pub frontmatter: Value,
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
    /// The run's `verify-vote` instances (for barrier phases), projected by
    /// the caller from `list_instances(verify-vote, {run})`. Empty for a run
    /// with no barrier phase.
    pub votes: Vec<Vote>,
    /// Per-claim count of the barrier's terminal (dead-lettered) vote steps,
    /// read from the ledger — a dead-letter writes no instance but still
    /// closes its slot (`§3.5`).
    pub deadlettered: BTreeMap<String, u32>,
    /// Frontmatter of the durable target pages a [`WriteMode::Existing`] phase
    /// weaves into, keyed by target page id. The caller resolves the target
    /// set from the upstream elements and `expand`s each page's frontmatter
    /// (compile-first-wiki G1). A weave completes when its target carries
    /// `source_event == <that step's event id>`; empty for a run with no
    /// `Existing` phase.
    pub targets: BTreeMap<String, Value>,
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
/// Two phase shapes drive the control flow:
///
/// - An **`over` phase pipelines** per-item over its upstream: `plan_phase`
///   enumerates a step only for each upstream instance that already exists,
///   so item A's next stage fires as soon as A is produced, without waiting
///   for its siblings (deep-research's `pipeline()`). Reduce emits every such
///   ready step across *all* pipeline phases in one pass.
/// - A **`Fixed` phase is a barrier point**: it may start only once every
///   earlier phase is complete. This is what makes `synthesize` (Fixed) wait
///   for the whole `verify` barrier to close, while a leading `scope` (Fixed,
///   no predecessors) runs immediately.
///
/// Re-emission is idempotent (`§3.6`), so a step already emitted (present in
/// `state.emitted`) is filtered out — an in-flight phase yields nothing new.
/// When every phase is complete the batch is empty and the run is done.
#[must_use]
pub fn reduce(spec: &WorkflowSkill, state: &RunState) -> Vec<StepIntent> {
    let mut batch = Vec::new();
    for (k, phase) in spec.phases.iter().enumerate() {
        // A Fixed barrier point waits for every earlier phase to complete.
        if matches!(phase.fan_out, FanOut::Fixed(_))
            && !spec.phases[..k]
                .iter()
                .all(|p| phase_complete(spec, p, state))
        {
            continue;
        }
        if phase_complete(spec, phase, state) {
            continue;
        }
        let expected = plan_phase(spec, phase, state, &state.run);
        batch.extend(
            expected
                .into_iter()
                .filter(|intent| !state.emitted.contains(&intent.event_id())),
        );
    }
    batch
}

/// Whether a phase has finished all the work it will ever do.
///
/// - `Fixed(n)`: all `n` pre-flagged instances are present.
/// - `Over` width 1 (pipeline stage): its upstream is complete *and* every
///   upstream item has produced this phase's instance.
/// - `Over` width > 1 (quorum barrier): its upstream is complete *and* the
///   barrier tally (`§3.5`) has closed every upstream item's claim.
fn phase_complete(spec: &WorkflowSkill, phase: &Phase, state: &RunState) -> bool {
    match &phase.fan_out {
        FanOut::Fixed(_) => {
            let expected = plan_phase(spec, phase, state, &state.run);
            let produced = state.produced_page_ids(&phase.produces);
            expected
                .iter()
                .all(|i| produced.contains(i.instance_page_id().as_str()))
        }
        FanOut::Over { over, width } => {
            if !upstream_complete(spec, over, state) {
                return false;
            }
            // Durable-target weave (compile-first-wiki G1): a weave completes
            // when each distinct target page carries this step's `source_event`
            // stamp — a durable page already exists, so "instance present"
            // cannot signal completion.
            if let WriteMode::Existing { target_field } = &phase.writes {
                return distinct_targets(over, state, target_field)
                    .iter()
                    .all(|tp| target_woven(state, &phase.id, tp));
            }
            let upstream = upstream_instances(over, state);
            if *width > 1 {
                // Barrier: every upstream item's claim must have closed.
                let outcomes = barrier::tally_barrier(&barrier_input(state, *width));
                let closed: BTreeSet<&str> = outcomes
                    .iter()
                    .filter(|o| o.closed)
                    .map(|o| o.claim.as_str())
                    .collect();
                upstream
                    .iter()
                    .all(|page_id| closed.contains(element_slug(page_id).as_str()))
            } else {
                // Pipeline stage: every upstream item has its produced instance.
                let expected = plan_phase(spec, phase, state, &state.run);
                let produced = state.produced_page_ids(&phase.produces);
                expected
                    .iter()
                    .all(|i| produced.contains(i.instance_page_id().as_str()))
            }
        }
    }
}

/// Whether the phase that produces `over` (this phase's upstream) is itself
/// complete. When no phase in the plan produces `over`, there is nothing to
/// wait for (the upstream is externally supplied) → treated as complete.
fn upstream_complete(spec: &WorkflowSkill, over: &str, state: &RunState) -> bool {
    let producers: Vec<&Phase> = spec.phases.iter().filter(|p| p.produces == over).collect();
    if producers.is_empty() {
        return true;
    }
    producers.iter().all(|p| phase_complete(spec, p, state))
}

/// Build the barrier tally input from the run's vote data + the plan's verify
/// policy (the barrier width is `votes_per_claim`).
fn barrier_input(state: &RunState, width: u32) -> BarrierInput {
    BarrierInput {
        votes: state.votes.clone(),
        deadlettered: state.deadlettered.clone(),
        votes_per_claim: width,
        // The reducer only needs closure here; refutations_required drives
        // `survivors`, which the caller computes separately when synthesizing.
        refutations_required: u32::MAX,
    }
}

/// The full set of steps a phase plans, given the run's current state. Pure:
/// a `Fixed` phase yields `n` slots `0..n`; an `over` phase yields one slot
/// per upstream produced instance (× `width` for a barrier phase).
fn plan_phase(spec: &WorkflowSkill, phase: &Phase, state: &RunState, run: &str) -> Vec<StepIntent> {
    match &phase.fan_out {
        FanOut::Fixed(n) => (0..*n)
            .map(|i| intent(spec, phase, run, i.to_string(), None, None, None, None))
            .collect(),
        FanOut::Over { over, width } => {
            // Durable-target weave: one step per distinct target page (deduped),
            // pre-flagged onto the existing page rather than a run-scoped id.
            if let WriteMode::Existing { target_field } = &phase.writes {
                return distinct_targets(over, state, target_field)
                    .into_iter()
                    .map(|tp| {
                        // `slot` is the full target page id so the event id is
                        // content-addressed per target (idempotent re-weave);
                        // `over` carries it for the harness/provenance.
                        intent(
                            spec,
                            phase,
                            run,
                            tp.clone(),
                            Some(tp.clone()),
                            None,
                            None,
                            Some(tp),
                        )
                    })
                    .collect();
            }
            let mut upstream: Vec<&str> = upstream_instances(over, state);
            upstream.sort_unstable(); // deterministic order
            let barrier = (*width > 1).then(|| phase.id.clone());
            upstream
                .into_iter()
                .flat_map(|page_id| {
                    let elem = element_slug(page_id);
                    let barrier = barrier.clone();
                    (0..*width).map(move |k| {
                        // A barrier phase (width > 1) pins the skeptic's slot
                        // both in the deterministic `slot` and — for the
                        // harness's benefit — in `vote_index`.
                        let (slot, vote_index) = if *width > 1 {
                            (format!("{elem}-v{k}"), Some(k))
                        } else {
                            (elem.clone(), None)
                        };
                        intent(
                            spec,
                            phase,
                            run,
                            slot,
                            Some(page_id.to_owned()),
                            barrier.clone(),
                            vote_index,
                            None,
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

/// Build one [`StepIntent`] for `phase` at `slot` within `run`.
#[allow(clippy::too_many_arguments)]
fn intent(
    spec: &WorkflowSkill,
    phase: &Phase,
    run: &str,
    slot: String,
    over: Option<String>,
    barrier: Option<String>,
    vote_index: Option<u32>,
    target_page: Option<String>,
) -> StepIntent {
    StepIntent {
        run: run.to_owned(),
        wf_skill: spec.id.clone(),
        phase: phase.id.clone(),
        produces: phase.produces.clone(),
        slot,
        barrier,
        over,
        vote_index,
        target_page,
    }
}

/// The distinct durable target page ids a [`WriteMode::Existing`] phase weaves
/// into: the `target_field` frontmatter value of each upstream `over` element,
/// deduped and sorted (deterministic). Multiple claims routed to one page
/// collapse to a single weave step.
fn distinct_targets(over: &str, state: &RunState, target_field: &str) -> Vec<String> {
    let mut targets: Vec<String> = state
        .produced
        .get(over)
        .into_iter()
        .flatten()
        .filter_map(|inst| {
            inst.frontmatter
                .get(target_field)
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect();
    targets.sort_unstable();
    targets.dedup();
    targets
}

/// Whether the durable `target_page` has been woven by this phase: its
/// frontmatter carries `source_event == <this step's deterministic event id>`,
/// the stamp the weave harness writes. Pure — reads only [`RunState::targets`].
fn target_woven(state: &RunState, phase_id: &str, target_page: &str) -> bool {
    let expected = key::step_event_id(&state.run, phase_id, target_page);
    state
        .targets
        .get(target_page)
        .and_then(|fm| fm.get("source_event"))
        .and_then(Value::as_str)
        == Some(expected.as_str())
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
                        ..Default::default()
                    })
                    .collect(),
            );
        }
        RunState {
            run: RUN.to_owned(),
            wf_skill: "deep-research".to_owned(),
            produced: map,
            emitted: emitted.iter().map(|s| (*s).to_owned()).collect(),
            ..Default::default()
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

    // A three-stage pipeline: seed produces 2 items, stageA processes each
    // item → a result, stageB processes each result → a final. `over` phases
    // pipeline, so a fast item can reach stageB while a slow one is still in
    // stageA.
    fn pipeline_spec() -> WorkflowSkill {
        WorkflowSkill::parse(&json!({
            "id": "pipe",
            "phases": [
                { "id": "seed", "produces": "item", "fan_out": 2 },
                { "id": "stageA", "produces": "result", "fan_out": { "over": "item" } },
                { "id": "stageB", "produces": "final", "fan_out": { "over": "result" } }
            ]
        }))
        .unwrap()
    }

    #[test]
    fn pipeline_advances_items_independently_slow_does_not_block_fast() {
        let spec = pipeline_spec();
        // seed emits two item steps.
        let seed = reduce(&spec, &state_with(&[], &[]));
        assert_eq!(seed.len(), 2);
        let (item0, item1) = (seed[0].instance_page_id(), seed[1].instance_page_id());

        // Both items produced → stageA emits a step per item.
        let after_seed = state_with(
            &[("item", &[&item0, &item1])],
            &[&seed[0].event_id(), &seed[1].event_id()],
        );
        let stage_a = reduce(&spec, &after_seed);
        assert_eq!(stage_a.len(), 2, "one stageA step per item");
        assert!(stage_a.iter().all(|s| s.phase == "stageA"));
        // Identify item0's stageA step by its `over`; item1's stays in flight.
        assert!(
            stage_a
                .iter()
                .any(|s| s.over.as_deref() == Some(item1.as_str())),
            "item1 has a stageA step too"
        );
        let a_for_item0 = stage_a
            .iter()
            .find(|s| s.over.as_deref() == Some(item0.as_str()))
            .unwrap();
        let result0 = a_for_item0.instance_page_id();

        // The FAST item (0) finished stageA (result0 exists); the SLOW item
        // (1) is still in stageA (no result). Reduce must, in one pass, both
        // advance result0 to stageB AND keep item1's stageA step alive —
        // neither blocks the other.
        let mixed = state_with(
            &[("item", &[&item0, &item1]), ("result", &[&result0])],
            &[
                &seed[0].event_id(),
                &seed[1].event_id(),
                &a_for_item0.event_id(), // item0's stageA already emitted
            ],
        );
        let batch = reduce(&spec, &mixed);
        let phases: BTreeSet<&str> = batch.iter().map(|s| s.phase.as_str()).collect();
        assert!(
            phases.contains("stageB"),
            "the fast item advanced to stageB: {batch:?}"
        );
        assert!(
            batch
                .iter()
                .any(|s| s.phase == "stageA" && s.over.as_deref() == Some(item1.as_str())),
            "the slow item's stageA step is still emitted (not blocked): {batch:?}"
        );
        // stageB only fired for the produced result, not the missing one.
        assert_eq!(
            batch.iter().filter(|s| s.phase == "stageB").count(),
            1,
            "stageB fans out only over produced results"
        );
        // Sanity: only item1's stageA step is pending (item0's was emitted).
        assert_eq!(
            batch.iter().filter(|s| s.phase == "stageA").count(),
            1,
            "item0's stageA step is already emitted, only item1 remains"
        );
    }

    fn barrier_spec() -> WorkflowSkill {
        WorkflowSkill::parse(&json!({
            "id": "verify-plan",
            "phases": [
                { "id": "extract", "produces": "claims", "fan_out": 1 },
                { "id": "verify", "produces": "verify-vote",
                  "fan_out": { "over": "claims", "width": "verify.votes_per_claim" } },
                { "id": "synthesize", "produces": "report", "fan_out": 1 }
            ],
            "verify": { "votes_per_claim": 3, "refutations_required": 2 }
        }))
        .unwrap()
    }

    #[test]
    fn synthesize_waits_for_the_verify_barrier_to_close() {
        let spec = barrier_spec();
        let extract = reduce(&spec, &state_with(&[], &[])).remove(0);
        let claims_page = extract.instance_page_id();
        let claim_key = element_slug(&claims_page);

        // Extract produced its claims instance → verify opens a width-3
        // barrier over that claims item (3 vote steps).
        let mut base = state_with(&[("claims", &[&claims_page])], &[&extract.event_id()]);
        let verify = reduce(&spec, &base);
        assert_eq!(verify.len(), 3, "3 vote steps for the one claim");
        assert!(verify.iter().all(|s| s.phase == "verify"));
        assert!(
            verify
                .iter()
                .all(|s| s.barrier.as_deref() == Some("verify"))
        );
        // Each skeptic step pins a distinct vote slot (0,1,2) so the harness
        // stamps distinct `vote_index`es and the barrier tallies three votes.
        let mut slots: Vec<u32> = verify.iter().filter_map(|s| s.vote_index).collect();
        slots.sort_unstable();
        assert_eq!(slots, vec![0, 1, 2], "distinct vote slots: {verify:?}");

        // Only 2 of 3 votes cast → barrier OPEN → synthesize must not fire.
        base.votes = vec![
            Vote {
                claim: claim_key.clone(),
                vote_index: 0,
                verdict: "valid".into(),
            },
            Vote {
                claim: claim_key.clone(),
                vote_index: 1,
                verdict: "valid".into(),
            },
        ];
        let batch = reduce(&spec, &base);
        assert!(
            !batch.iter().any(|s| s.phase == "synthesize"),
            "synthesize is gated on the open barrier: {batch:?}"
        );

        // The 3rd vote closes the barrier → synthesize (Fixed) now fires.
        base.votes.push(Vote {
            claim: claim_key,
            vote_index: 2,
            verdict: "valid".into(),
        });
        let batch = reduce(&spec, &base);
        assert!(
            batch.iter().any(|s| s.phase == "synthesize"),
            "closed barrier releases synthesize: {batch:?}"
        );
    }

    // A distill-shaped plan: match produces run-scoped `weave-plan` records
    // (each naming a durable `target_page`); weave is a `writes: existing`
    // phase that fans out over the distinct target pages; integrate is a Fixed
    // barrier gated on every weave landing.
    fn distill_spec() -> WorkflowSkill {
        WorkflowSkill::parse(&json!({
            "id": "distill",
            "phases": [
                { "id": "match", "produces": "weave-plan", "fan_out": 2 },
                { "id": "weave", "produces": "weave",
                  "fan_out": { "over": "weave-plan" },
                  "writes": "existing", "target_field": "target_page" },
                { "id": "integrate", "produces": "distill-report", "fan_out": 1 }
            ]
        }))
        .unwrap()
    }

    /// Build a `weave-plan` `ProducedInstance` carrying a `target_page`.
    fn plan_row(page_id: &str, target: &str) -> ProducedInstance {
        ProducedInstance {
            page_id: page_id.to_owned(),
            frontmatter: json!({ "target_page": target }),
        }
    }

    #[test]
    fn existing_phase_fans_out_one_step_per_distinct_target() {
        let spec = distill_spec();
        // Two match rows completed, pointing at two distinct durable pages.
        let m = reduce(&spec, &state_with(&[], &[]));
        let (m0, m1) = (m[0].event_id(), m[1].event_id());
        let mut state = state_with(&[], &[&m0, &m1]);
        state.produced.insert(
            "weave-plan".to_owned(),
            vec![
                plan_row("markdown/instances/weave-plan/r1-match-a.md", "markdown/instances/entity/acme.md"),
                plan_row("markdown/instances/weave-plan/r1-match-b.md", "markdown/instances/entity/globex.md"),
            ],
        );
        // match is Fixed(2); mark it complete so weave can plan.
        state.produced.get_mut("weave-plan").unwrap().extend([
            plan_row(&m[0].instance_page_id(), "markdown/instances/entity/acme.md"),
            plan_row(&m[1].instance_page_id(), "markdown/instances/entity/globex.md"),
        ]);
        let batch = reduce(&spec, &state);
        let weaves: Vec<&StepIntent> = batch.iter().filter(|s| s.phase == "weave").collect();
        assert_eq!(weaves.len(), 2, "one weave step per distinct target: {batch:?}");
        assert!(weaves.iter().all(|s| s.target_page.is_some()));
        // Each weave pre-flags the durable page, not a run-scoped instance.
        let ipids: Vec<String> = weaves.iter().map(|s| s.instance_page_id()).collect();
        assert!(ipids.contains(&"markdown/instances/entity/acme.md".to_owned()));
        assert!(ipids.contains(&"markdown/instances/entity/globex.md".to_owned()));
    }

    #[test]
    fn duplicate_targets_collapse_to_one_weave_step() {
        let spec = distill_spec();
        let mut state = state_with(&[], &[]);
        // Two match rows pointing at the SAME durable page ⇒ one weave step.
        state.produced.insert(
            "weave-plan".to_owned(),
            vec![
                plan_row("markdown/instances/weave-plan/r1-match-0.md", "markdown/instances/entity/acme.md"),
                plan_row("markdown/instances/weave-plan/r1-match-1.md", "markdown/instances/entity/acme.md"),
            ],
        );
        // Fixed(2) match complete (its two pre-flagged instances present).
        let m = reduce(&spec, &state_with(&[], &[]));
        state.emitted.insert(m[0].event_id());
        state.emitted.insert(m[1].event_id());
        state.produced.get_mut("weave-plan").unwrap().extend([
            plan_row(&m[0].instance_page_id(), "markdown/instances/entity/acme.md"),
            plan_row(&m[1].instance_page_id(), "markdown/instances/entity/acme.md"),
        ]);
        let weaves: Vec<StepIntent> = reduce(&spec, &state)
            .into_iter()
            .filter(|s| s.phase == "weave")
            .collect();
        assert_eq!(weaves.len(), 1, "same target ⇒ single weave step");
        assert_eq!(
            weaves[0].instance_page_id(),
            "markdown/instances/entity/acme.md"
        );
    }

    #[test]
    fn integrate_waits_until_every_target_is_source_event_stamped() {
        let spec = distill_spec();
        // match complete, two distinct targets.
        let m = reduce(&spec, &state_with(&[], &[]));
        let mut base = state_with(&[], &[&m[0].event_id(), &m[1].event_id()]);
        base.produced.insert(
            "weave-plan".to_owned(),
            vec![
                plan_row(&m[0].instance_page_id(), "markdown/instances/entity/acme.md"),
                plan_row(&m[1].instance_page_id(), "markdown/instances/entity/globex.md"),
            ],
        );
        // Weave steps emitted; only the FIRST target woven (source_event set).
        let weaves: Vec<StepIntent> = plan_phase(&spec, &spec.phases[1], &base, &base.run);
        for w in &weaves {
            base.emitted.insert(w.event_id());
        }
        let acme_ev = key::step_event_id(&base.run, "weave", "markdown/instances/entity/acme.md");
        base.targets.insert(
            "markdown/instances/entity/acme.md".to_owned(),
            json!({ "source_event": acme_ev }),
        );
        // globex not yet stamped ⇒ integrate (Fixed) must not fire.
        let batch = reduce(&spec, &base);
        assert!(
            !batch.iter().any(|s| s.phase == "integrate"),
            "integrate gated on the open weave barrier: {batch:?}"
        );

        // Stamp the second target ⇒ weave complete ⇒ integrate releases.
        let globex_ev =
            key::step_event_id(&base.run, "weave", "markdown/instances/entity/globex.md");
        base.targets.insert(
            "markdown/instances/entity/globex.md".to_owned(),
            json!({ "source_event": globex_ev }),
        );
        let batch = reduce(&spec, &base);
        assert!(
            batch.iter().any(|s| s.phase == "integrate"),
            "all targets woven ⇒ integrate fires: {batch:?}"
        );
    }

    #[test]
    fn new_phase_is_unchanged_by_the_extension() {
        // A plan with no `writes:` must behave exactly as before (regression
        // guard for deep-research): first phase emits, no target_page set.
        let spec = linear_spec();
        let batch = reduce(&spec, &state_with(&[], &[]));
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].target_page, None);
        assert!(batch[0].instance_page_id().starts_with("markdown/instances/research-angle/"));
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
