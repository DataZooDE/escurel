//! The step vocabulary the reducer emits.
//!
//! A [`StepIntent`] is the reducer's decision to run one more harness step:
//! which phase, over which item, producing which skill's instance. It is a
//! *pure* value — no ids minted from wall-clock or randomness. The dispatch
//! loop turns each intent into a `capture_event` call, using the
//! deterministic event id + pre-flagged instance page id from [`crate::key`]
//! and stamping the [`WorkflowProvenance`] block returned by
//! [`StepIntent::provenance`].

use escurel_types::WorkflowProvenance;

use crate::key;

/// One step the reducer wants to run, addressed by its deterministic slot.
///
/// The `slot` is the `§3.6` fan-out index — an angle ordinal, a source page
/// id, a claim ref, or a `claim-vN` vote slot — and together with `(run,
/// phase)` it content-addresses the step: same `(run, phase, slot)` ⇒ same
/// event id and same pre-flagged instance, which is what makes re-emission
/// idempotent and recovery a no-op on already-landed steps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepIntent {
    /// The `workflow-run` instance page id this step belongs to.
    pub run: String,
    /// The `kind: workflow` plan skill id.
    pub wf_skill: String,
    /// The phase id this step executes.
    pub phase: String,
    /// The skill id this step's harness run writes an instance of.
    pub produces: String,
    /// The deterministic fan-out slot (`§3.6`): angle ordinal, source page
    /// id, claim ref, or `claim-vN` vote slot.
    pub slot: String,
    /// The barrier this step participates in, if any (e.g. `verify`).
    pub barrier: Option<String>,
    /// The per-item routing target this step fans out over (empty for a
    /// width-1 phase).
    pub over: Option<String>,
    /// The vote slot this step occupies within its barrier (`Some(k)` for a
    /// width-`k` verify barrier, `None` otherwise). Carried into the
    /// `provenance.workflow.vote_index` so the harness stamps the matching
    /// `verify-vote` — the reducer already encodes it into `slot` (`elem-vN`),
    /// but the harness cannot recover it from the hashed step id, so it must
    /// travel in the provenance.
    pub vote_index: Option<u32>,
    /// The durable existing page this step writes, for a
    /// [`crate::WriteMode::Existing`] phase (`None` for a normal run-scoped
    /// phase). When set, [`StepIntent::instance_page_id`] returns it verbatim
    /// so the event is pre-flagged onto — and reconciliation confirms on — the
    /// durable target rather than a fresh run-scoped instance.
    pub target_page: Option<String>,
}

impl StepIntent {
    /// The deterministic event id for this step (`§3.6` point 1). Emitting
    /// the same intent twice produces the same id, so the events insert's
    /// `ON CONFLICT DO NOTHING` + the ledger's `(tenant, event_id)` unique
    /// index collapse the duplicate.
    #[must_use]
    pub fn event_id(&self) -> String {
        key::step_event_id(&self.run, &self.phase, &self.slot)
    }

    /// The instance page id this step writes. For a
    /// [`crate::WriteMode::Existing`] phase it is the durable `target_page`
    /// verbatim (the event is pre-flagged onto the existing page); otherwise
    /// it is the deterministic run-scoped pre-flag (`§3.6` point 2) — a
    /// re-driven step overwrites rather than forks either way.
    #[must_use]
    pub fn instance_page_id(&self) -> String {
        match &self.target_page {
            Some(page) => page.clone(),
            None => key::step_instance_page_id(&self.produces, &self.run, &self.phase, &self.slot),
        }
    }

    /// The `provenance.workflow` block to stamp on this step's event. Its
    /// `step` is the event id, so a consumer can recover the step identity
    /// from the event alone.
    #[must_use]
    pub fn provenance(&self) -> WorkflowProvenance {
        WorkflowProvenance {
            run: self.run.clone(),
            wf_skill: self.wf_skill.clone(),
            phase: self.phase.clone(),
            step: self.event_id(),
            barrier: self.barrier.clone().unwrap_or_default(),
            over: self.over.clone().unwrap_or_default(),
            vote_index: self.vote_index,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn intent() -> StepIntent {
        StepIntent {
            run: "markdown/instances/workflow-run/r1.md".to_owned(),
            wf_skill: "deep-research".to_owned(),
            phase: "verify".to_owned(),
            produces: "verify-vote".to_owned(),
            slot: "c12-v0".to_owned(),
            barrier: Some("verify".to_owned()),
            over: Some("[[claim::c12]]".to_owned()),
            vote_index: Some(0),
            target_page: None,
        }
    }

    #[test]
    fn identical_intents_share_id_and_instance() {
        let a = intent();
        let b = intent();
        assert_eq!(a.event_id(), b.event_id());
        assert_eq!(a.instance_page_id(), b.instance_page_id());
    }

    #[test]
    fn provenance_step_is_the_event_id() {
        let i = intent();
        let p = i.provenance();
        assert_eq!(p.step, i.event_id());
        assert_eq!(p.run, i.run);
        assert_eq!(p.phase, "verify");
        assert_eq!(p.barrier, "verify");
        assert_eq!(p.over, "[[claim::c12]]");
        assert_eq!(p.vote_index, Some(0));
    }

    #[test]
    fn provenance_round_trips_via_event_json() {
        let p = intent().provenance();
        let event_provenance = serde_json::json!({ "workflow": p });
        assert_eq!(
            WorkflowProvenance::from_provenance(&event_provenance),
            Some(p)
        );
    }

    #[test]
    fn durable_target_step_pre_flags_the_existing_page() {
        let i = StepIntent {
            phase: "weave".to_owned(),
            produces: "weave".to_owned(),
            slot: "markdown/instances/entity/acme.md".to_owned(),
            barrier: None,
            over: Some("markdown/instances/entity/acme.md".to_owned()),
            vote_index: None,
            target_page: Some("markdown/instances/entity/acme.md".to_owned()),
            ..intent()
        };
        // The event is pre-flagged onto the durable page, not a run-scoped id.
        assert_eq!(i.instance_page_id(), "markdown/instances/entity/acme.md");
        // Two weaves of the same target share an id (idempotent overwrite);
        // distinct targets get distinct ids (content-addressed on the slot).
        assert_eq!(i.event_id(), i.event_id());
        let other = StepIntent {
            slot: "markdown/instances/entity/globex.md".to_owned(),
            target_page: Some("markdown/instances/entity/globex.md".to_owned()),
            ..i.clone()
        };
        assert_ne!(i.event_id(), other.event_id());
    }

    #[test]
    fn width_one_step_has_empty_barrier_and_over() {
        let i = StepIntent {
            barrier: None,
            over: None,
            vote_index: None,
            ..intent()
        };
        let p = i.provenance();
        assert_eq!(p.barrier, "");
        assert_eq!(p.over, "");
        assert_eq!(p.vote_index, None);
    }
}
