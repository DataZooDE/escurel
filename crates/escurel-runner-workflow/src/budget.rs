//! Up-front fan-out budgeting (`§7`).
//!
//! `admit` caps `max_runs_per_root` and dead-letters the *remaining*
//! children once the cap is crossed — so a barrier discovered mid-flight can
//! be left permanently short of quorum (a 25-claim × 3-vote verify phase
//! emits 75 sibling steps under one root). The fix is to **reserve the whole
//! plan's projected fan-out against the budget before the first emit** and
//! fail fast, rather than discover the ceiling mid-barrier.
//!
//! The projection is a conservative *upper bound*: every phase contributes
//! its widest possible step count. `Over` phases are data-dependent (how many
//! angles/sources/claims appear at runtime is unknown up front), so each is
//! bounded by its declared cap (`max` / `max_targets`) times its per-item
//! width, or [`DEFAULT_PHASE_FANOUT_CAP`] when it declares none. The bound is
//! never an under-estimate, so passing the gate guarantees the run cannot
//! starve a barrier.

use crate::spec::{FanOut, Phase, WorkflowSkill};

/// The assumed per-item cap for an `over` phase that declares neither `max`
/// nor `max_targets`. Deliberately generous but finite, so an unbounded
/// fan-out still gets a real projected cost rather than escaping the gate.
pub const DEFAULT_PHASE_FANOUT_CAP: u64 = 32;

/// A conservative upper bound on the total number of harness runs a plan can
/// fan out under one root — the sum of every phase's widest step count.
#[must_use]
pub fn projected_fan_out(spec: &WorkflowSkill) -> u64 {
    spec.phases.iter().map(phase_bound).sum()
}

fn phase_bound(phase: &Phase) -> u64 {
    match &phase.fan_out {
        FanOut::Fixed(n) => u64::from(*n),
        FanOut::Over { width, .. } => {
            let cap = phase
                .max_targets
                .or(phase.max)
                .map(|c| c as u64)
                .unwrap_or(DEFAULT_PHASE_FANOUT_CAP);
            cap.saturating_mul(u64::from(*width))
        }
    }
}

/// Raised when a plan's projected fan-out exceeds the run budget.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "workflow plan projects {projected} runs, over the max_runs_per_root budget of {budget} — \
     refusing to start (would starve a barrier mid-flight)"
)]
pub struct BudgetExceeded {
    pub projected: u64,
    pub budget: u64,
}

/// Fail fast when the plan's projected fan-out (`§7`) cannot fit under
/// `max_runs_per_root`. Called once, at invocation, before the first emit.
pub fn check_budget(spec: &WorkflowSkill, max_runs_per_root: u64) -> Result<(), BudgetExceeded> {
    let projected = projected_fan_out(spec);
    if projected > max_runs_per_root {
        Err(BudgetExceeded {
            projected,
            budget: max_runs_per_root,
        })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn deep_research() -> WorkflowSkill {
        WorkflowSkill::parse(&json!({
            "id": "deep-research",
            "phases": [
                { "id": "scope", "produces": "research-angle", "fan_out": 1 },
                { "id": "search", "produces": "source", "fan_out": { "over": "research-angle" } },
                { "id": "fetch", "produces": "source", "max": 15 },
                { "id": "extract", "produces": "claims", "fan_out": { "over": "source" }, "max": 15 },
                { "id": "verify", "produces": "verify-vote",
                  "fan_out": { "over": "claim", "width": "verify.votes_per_claim" },
                  "max_targets": 25 },
                { "id": "synthesize", "produces": "research-report", "fan_out": 1 }
            ],
            "verify": { "votes_per_claim": 3, "refutations_required": 2 }
        }))
        .unwrap()
    }

    #[test]
    fn projects_a_conservative_upper_bound() {
        // scope 1 + search (uncapped over → DEFAULT 32) + fetch (Fixed, 15 max
        // ignored since Fixed(1)) ... compute explicitly:
        // scope: Fixed(1) = 1
        // search: Over uncapped width1 → 32
        // fetch: no fan_out ⇒ Fixed(1) = 1
        // extract: Over max 15 width1 → 15
        // verify: Over max_targets 25 width 3 → 75
        // synthesize: Fixed(1) = 1
        // total = 1 + 32 + 1 + 15 + 75 + 1 = 125
        assert_eq!(projected_fan_out(&deep_research()), 125);
    }

    #[test]
    fn passes_when_budget_covers_the_projection() {
        assert!(check_budget(&deep_research(), 125).is_ok());
        assert!(check_budget(&deep_research(), 1000).is_ok());
    }

    #[test]
    fn fails_fast_when_projection_exceeds_budget() {
        let err = check_budget(&deep_research(), 64).unwrap_err();
        assert_eq!(err.projected, 125);
        assert_eq!(err.budget, 64);
    }

    #[test]
    fn verify_barrier_width_multiplies_the_cap() {
        // A verify phase alone: max_targets 25 × votes_per_claim 5 = 125.
        let spec = WorkflowSkill::parse(&json!({
            "id": "v",
            "phases": [
                { "id": "verify", "produces": "verify-vote",
                  "fan_out": { "over": "claim", "width": "verify.votes_per_claim" },
                  "max_targets": 25 }
            ],
            "verify": { "votes_per_claim": 5, "refutations_required": 3 }
        }))
        .unwrap();
        assert_eq!(projected_fan_out(&spec), 125);
    }
}
