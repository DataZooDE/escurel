//! The loop-control admission gate (#157).
//!
//! Lifecycle step 4 of
//! [`docs/contract/agent-orchestration.md`](https://github.com/DataZooDE/escurel/blob/main/docs/contract/agent-orchestration.md)
//! opens the dispatch gate with idempotency ([`crate::Ledger::begin_run`]) and
//! then enforces the **loop controls**: depth/budget/cycle. This module owns
//! that second half — a single focused, pure-ish decision function
//! ([`admit`]) that, given a normalised [`Trigger`], the loop-control limits,
//! and a read-only view over the durable ledger, returns whether the run may
//! proceed or must be **dead-lettered** with a recorded reason.
//!
//! The three controls (the dispatch gate, before a run is admitted):
//!
//! 1. **Depth** — when `trigger.lineage.depth > max_depth`, deny
//!    [`DeadLetterReason::DepthExceeded`]. The hard backstop: bounded even if
//!    the cycle check misses (e.g. instances differ every hop).
//! 2. **Per-root budget** — when the count of runs sharing this trigger's
//!    `root_event_id` is at/over `max_runs_per_root`, deny
//!    [`DeadLetterReason::BudgetExceeded`]. Catches a cascade that fans out
//!    wide without deepening.
//! 3. **Cycle** — when the candidate instance (the instance this trigger
//!    would write — `trigger.instance_page_id`) is already in the lineage's
//!    `instance_path`, deny [`DeadLetterReason::Cycle`]. This is what stops an
//!    A→B→A oscillation where each hop re-visits an instance it already wrote.
//!
//! Dedup (in-flight + identical `(instance, content_hash)`) lives in
//! [`crate::Ledger::begin_run`] (the idempotency half) — combined with the
//! cycle control here, an oscillation is stopped even when instances differ
//! but content is unchanged.

use crate::ledger::DeadLetterReason;
use crate::{Ledger, LedgerError, Trigger};

/// The loop-control limits the gate enforces, lifted from [`RunnerConfig`].
///
/// [`RunnerConfig`]: crate::RunnerConfig
#[derive(Debug, Clone, Copy)]
pub struct LoopLimits {
    /// Maximum cascade depth before [`DeadLetterReason::DepthExceeded`].
    pub max_depth: u32,
    /// Maximum runs sharing a `root_event_id` before
    /// [`DeadLetterReason::BudgetExceeded`].
    pub max_runs_per_root: u64,
}

/// The gate's decision for one trigger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// The run may proceed.
    Admit,
    /// The run must be dead-lettered with this reason (it would deepen past
    /// the cap, exhaust the per-root budget, or close a cascade cycle).
    DeadLetter(DeadLetterReason),
}

/// Decide whether `trigger` may be admitted, given the loop `limits` and a
/// read-only view over the durable `ledger`. Pure of side effects: the caller
/// records the dead-letter against the ledger row.
///
/// Evaluation order is **depth → cycle → budget** — depth first because it is
/// the cheapest, most decisive backstop; cycle next because a re-visited
/// instance is the precise loop signal; budget last as the catch-all.
///
/// `runs_for_root` is read from the ledger *including* this trigger's own
/// just-created `pending` row (the gate runs after `begin_run`), so the budget
/// fires once the cascade tree reaches `max_runs_per_root` rows.
pub fn admit(
    trigger: &Trigger,
    limits: &LoopLimits,
    ledger: &Ledger,
) -> Result<Admission, LedgerError> {
    // 1. Depth — the hard backstop.
    if trigger.lineage.depth > limits.max_depth {
        return Ok(Admission::DeadLetter(DeadLetterReason::DepthExceeded));
    }

    // 2. Cycle — the candidate instance (what this trigger would write) is
    //    already in the lineage's instance chain → admitting closes a cycle.
    if let Some(candidate) = &trigger.instance_page_id
        && trigger.lineage.instance_path.iter().any(|i| i == candidate)
    {
        return Ok(Admission::DeadLetter(DeadLetterReason::Cycle));
    }

    // 3. Per-root budget — count the whole cascade tree (this row included).
    let runs_for_root =
        ledger.count_runs_for_root(&trigger.tenant, &trigger.lineage.root_event_id)?;
    if runs_for_root > limits.max_runs_per_root {
        return Ok(Admission::DeadLetter(DeadLetterReason::BudgetExceeded));
    }

    Ok(Admission::Admit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LedgerDecision, Lineage};

    fn limits(max_depth: u32, max_runs_per_root: u64) -> LoopLimits {
        LoopLimits {
            max_depth,
            max_runs_per_root,
        }
    }

    fn trigger_at(depth: u32, instance: Option<&str>, instance_path: Vec<String>) -> Trigger {
        Trigger {
            tenant: "acme".into(),
            event_id: format!("EVT-{depth}"),
            label_skill: "alpha".into(),
            instance_page_id: instance.map(str::to_owned),
            lineage: Lineage {
                root_event_id: "ROOT".into(),
                depth,
                lineage_path: vec!["ROOT".into()],
                instance_path,
                trace_id: None,
            },
            workflow: None,
        }
    }

    #[test]
    fn depth_over_cap_dead_letters_depth_exceeded() {
        let ledger = Ledger::open_in_memory().expect("open");
        let t = trigger_at(4, None, vec![]);
        assert_eq!(
            admit(&t, &limits(3, 100), &ledger).expect("admit"),
            Admission::DeadLetter(DeadLetterReason::DepthExceeded),
        );
    }

    #[test]
    fn depth_at_cap_is_admitted() {
        let ledger = Ledger::open_in_memory().expect("open");
        let t = trigger_at(3, None, vec![]);
        assert_eq!(
            admit(&t, &limits(3, 100), &ledger).expect("admit"),
            Admission::Admit,
        );
    }

    #[test]
    fn revisited_instance_dead_letters_cycle() {
        let ledger = Ledger::open_in_memory().expect("open");
        // The candidate instance b1 is already in the chain → cycle.
        let t = trigger_at(
            1,
            Some("markdown/instances/beta/b1.md"),
            vec!["markdown/instances/beta/b1.md".into()],
        );
        assert_eq!(
            admit(&t, &limits(100, 100), &ledger).expect("admit"),
            Admission::DeadLetter(DeadLetterReason::Cycle),
        );
    }

    #[test]
    fn fresh_instance_is_admitted() {
        let ledger = Ledger::open_in_memory().expect("open");
        let t = trigger_at(
            1,
            Some("markdown/instances/alpha/a1.md"),
            vec!["markdown/instances/beta/b1.md".into()],
        );
        assert_eq!(
            admit(&t, &limits(100, 100), &ledger).expect("admit"),
            Admission::Admit,
        );
    }

    #[test]
    fn over_budget_dead_letters_budget_exceeded() {
        let ledger = Ledger::open_in_memory().expect("open");
        // Seed 3 runs sharing ROOT, budget 2 → over.
        for i in 0..3 {
            let mut t = trigger_at(0, None, vec![]);
            t.event_id = format!("R-{i}");
            assert!(matches!(
                ledger.begin_run(&t).expect("begin"),
                LedgerDecision::Created(_)
            ));
        }
        let candidate = trigger_at(0, None, vec![]);
        assert_eq!(
            admit(&candidate, &limits(100, 2), &ledger).expect("admit"),
            Admission::DeadLetter(DeadLetterReason::BudgetExceeded),
        );
    }
}
