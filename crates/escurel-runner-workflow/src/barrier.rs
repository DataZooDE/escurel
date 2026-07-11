//! The quorum-barrier tally (`§3.5`) — the verify phase's close/survivor
//! decision, as a **pure** function over the run's `verify-vote` instances
//! plus a ledger read of the barrier's terminal (dead-lettered) steps.
//!
//! The decision path is deliberately agent-safe: the caller gathers votes
//! via `list_instances(verify-vote, {run})` (the `Role::Agent` surface) and
//! the terminal-step counts via the runner's own ledger — never
//! `run_stored_query` (admin-only). This module is the pure arithmetic.
//!
//! Three properties make it correct where a naive `HAVING count(*)` is not:
//!
//! - **Monotonic under retries / at-least-once.** Votes are tallied by
//!   `COUNT(DISTINCT vote_index)`, not row count. A retried or duplicated
//!   `verify-vote` reuses its deterministic slot (`§3.6`), so it can never
//!   inflate a claim past `votes_per_claim` or flip a survivor on replay.
//! - **Terminal-outcome closure, not "all instances present."** A vote step
//!   that dead-letters (budget/depth/cycle — terminal, never re-driven)
//!   writes no instance, so closure unions the vote instances with the
//!   barrier's terminal step count. A dead-lettered or `unverified` vote is
//!   scored a **non-refutation** — it counts toward closure but never kills a
//!   claim. Without this, one dead-lettered vote wedges the barrier forever.
//! - **Survivors are frozen once closed.** A claim is decided only once its
//!   barrier is closed; the reducer records the survivor set as a
//!   `barrier.closed` marker so a later re-projection reads the recorded
//!   decision rather than re-deriving a tally over a still-settling table.

use std::collections::{BTreeMap, BTreeSet};

/// A single `verify-vote` instance, as the caller projects it from
/// `list_instances(verify-vote, {run})`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vote {
    /// The claim ref this vote is about (the barrier's `COUNT(DISTINCT)` key
    /// groups on this).
    pub claim: String,
    /// The deterministic vote slot `0..votes_per_claim` (`§3.6`). Two votes
    /// with the same `(claim, vote_index)` are the *same* logical vote.
    pub vote_index: u32,
    /// `refuted` | `valid` | `unverified`. Only `refuted` counts against a
    /// claim; `unverified` (a vote the verifier could not cast) counts
    /// toward closure but never refutes.
    pub verdict: String,
}

/// The inputs to a barrier tally for one run's verify phase.
#[derive(Debug, Clone, Default)]
pub struct BarrierInput {
    /// Every `verify-vote` instance for the run (any claim).
    pub votes: Vec<Vote>,
    /// Per-claim count of the barrier's **terminal** (dead-lettered) vote
    /// steps — read from the ledger, since a dead-letter writes no instance.
    /// A claim absent from this map has zero terminal steps.
    pub deadlettered: BTreeMap<String, u32>,
    /// The quorum size (votes gathered per claim).
    pub votes_per_claim: u32,
    /// Refutations that kill a claim (`≥` this ⇒ dropped).
    pub refutations_required: u32,
}

/// The tally outcome for one claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimOutcome {
    pub claim: String,
    /// Distinct votes cast (by `vote_index`).
    pub votes: u32,
    /// Distinct refuting votes (by `vote_index`, `verdict == "refuted"`).
    pub refutations: u32,
    /// True once votes-plus-dead-lettered-steps reach `votes_per_claim`.
    pub closed: bool,
    /// True when the claim closed with `refutations < refutations_required`.
    /// Only meaningful when `closed`.
    pub survived: bool,
}

/// Tally every claim's barrier from the run's votes + terminal step counts.
/// Deterministic and pure: same input ⇒ same outcome, in claim-sorted order.
#[must_use]
pub fn tally_barrier(input: &BarrierInput) -> Vec<ClaimOutcome> {
    // Group distinct vote slots + distinct refuting slots per claim.
    let mut slots: BTreeMap<&str, BTreeSet<u32>> = BTreeMap::new();
    let mut refuting: BTreeMap<&str, BTreeSet<u32>> = BTreeMap::new();
    for v in &input.votes {
        slots.entry(&v.claim).or_default().insert(v.vote_index);
        if v.verdict == "refuted" {
            refuting.entry(&v.claim).or_default().insert(v.vote_index);
        }
    }

    // Every claim that has either a vote or a terminal step is in play.
    let claims: BTreeSet<&str> = slots
        .keys()
        .copied()
        .chain(input.deadlettered.keys().map(String::as_str))
        .collect();

    claims
        .into_iter()
        .map(|claim| {
            let votes = slots.get(claim).map_or(0, |s| s.len() as u32);
            let refutations = refuting.get(claim).map_or(0, |s| s.len() as u32);
            let terminal = input.deadlettered.get(claim).copied().unwrap_or(0);
            // Closure counts real votes AND dead-lettered steps: a
            // dead-lettered vote is a non-refutation that still closes.
            let closed = votes + terminal >= input.votes_per_claim;
            let survived = closed && refutations < input.refutations_required;
            ClaimOutcome {
                claim: claim.to_owned(),
                votes,
                refutations,
                closed,
                survived,
            }
        })
        .collect()
}

/// True when *every* claim in the tally has closed — the verify barrier is
/// done and the run may advance to synthesize. An empty tally is vacuously
/// closed (no claims to verify).
#[must_use]
pub fn barrier_closed(outcomes: &[ClaimOutcome]) -> bool {
    outcomes.iter().all(|o| o.closed)
}

/// The surviving claim refs (closed, under the refutation threshold), in
/// deterministic order — what synthesize fans out over.
#[must_use]
pub fn survivors(outcomes: &[ClaimOutcome]) -> Vec<String> {
    outcomes
        .iter()
        .filter(|o| o.survived)
        .map(|o| o.claim.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vote(claim: &str, idx: u32, verdict: &str) -> Vote {
        Vote {
            claim: claim.to_owned(),
            vote_index: idx,
            verdict: verdict.to_owned(),
        }
    }

    fn input(votes: Vec<Vote>, deadlettered: &[(&str, u32)]) -> BarrierInput {
        BarrierInput {
            votes,
            deadlettered: deadlettered
                .iter()
                .map(|(c, n)| ((*c).to_owned(), *n))
                .collect(),
            votes_per_claim: 3,
            refutations_required: 2,
        }
    }

    #[test]
    fn three_valid_votes_close_and_survive() {
        let out = tally_barrier(&input(
            vec![
                vote("c1", 0, "valid"),
                vote("c1", 1, "valid"),
                vote("c1", 2, "valid"),
            ],
            &[],
        ));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].votes, 3);
        assert!(out[0].closed);
        assert!(out[0].survived);
        assert!(barrier_closed(&out));
        assert_eq!(survivors(&out), vec!["c1".to_owned()]);
    }

    #[test]
    fn a_duplicate_vote_does_not_inflate_the_count() {
        // Two rows at the same slot are the SAME logical vote (at-least-once
        // delivery / a retry). Only two DISTINCT slots ⇒ not yet closed.
        let out = tally_barrier(&input(
            vec![
                vote("c1", 0, "valid"),
                vote("c1", 0, "valid"), // duplicate slot 0
                vote("c1", 1, "valid"),
            ],
            &[],
        ));
        assert_eq!(out[0].votes, 2, "distinct slots, not row count");
        assert!(!out[0].closed, "only 2 of 3 distinct votes → open");
    }

    #[test]
    fn two_refutations_drop_the_claim() {
        let out = tally_barrier(&input(
            vec![
                vote("c1", 0, "refuted"),
                vote("c1", 1, "refuted"),
                vote("c1", 2, "valid"),
            ],
            &[],
        ));
        assert!(out[0].closed);
        assert_eq!(out[0].refutations, 2);
        assert!(!out[0].survived, "2 refutations kills the claim");
        assert!(survivors(&out).is_empty());
    }

    #[test]
    fn a_duplicate_refutation_does_not_inflate_refutations() {
        // Same refuting slot twice is one refutation — cannot flip a survivor.
        let out = tally_barrier(&input(
            vec![
                vote("c1", 0, "refuted"),
                vote("c1", 0, "refuted"), // duplicate refutation, same slot
                vote("c1", 1, "valid"),
                vote("c1", 2, "valid"),
            ],
            &[],
        ));
        assert_eq!(out[0].refutations, 1);
        assert!(out[0].survived, "one distinct refutation < 2 → survives");
    }

    #[test]
    fn unverified_and_deadlettered_close_but_never_refute() {
        // c1: one valid vote, one `unverified` vote, and one dead-lettered
        // step. Closure = 2 distinct votes + 1 terminal = 3 ⇒ closed. Neither
        // the unverified vote nor the dead-letter is a refutation ⇒ survives.
        let out = tally_barrier(&input(
            vec![vote("c1", 0, "valid"), vote("c1", 1, "unverified")],
            &[("c1", 1)],
        ));
        assert_eq!(out[0].votes, 2);
        assert_eq!(out[0].refutations, 0, "unverified/deadletter never refute");
        assert!(out[0].closed, "2 votes + 1 terminal step = quorum");
        assert!(out[0].survived);
    }

    #[test]
    fn a_lone_deadletter_does_not_wedge_closure() {
        // Without terminal-step closure, a claim whose only fate is a
        // dead-lettered vote would never close. Here 2 votes + 1 dead-letter
        // reach quorum, so the phase can proceed and synthesize over it.
        let out = tally_barrier(&input(
            vec![vote("c9", 0, "valid"), vote("c9", 1, "valid")],
            &[("c9", 1)],
        ));
        assert!(out[0].closed);
        assert!(out[0].survived);
    }

    #[test]
    fn open_barrier_blocks_and_multi_claim_is_deterministic() {
        // c1 closed+survives; c2 only 1 vote (open) ⇒ whole barrier not closed.
        let out = tally_barrier(&input(
            vec![
                vote("c2", 0, "valid"),
                vote("c1", 0, "valid"),
                vote("c1", 1, "valid"),
                vote("c1", 2, "valid"),
            ],
            &[],
        ));
        // Claim-sorted order.
        assert_eq!(
            out.iter().map(|o| o.claim.as_str()).collect::<Vec<_>>(),
            vec!["c1", "c2"]
        );
        assert!(!barrier_closed(&out), "c2 open ⇒ barrier not closed");
        assert_eq!(survivors(&out), vec!["c1".to_owned()]);
    }
}
