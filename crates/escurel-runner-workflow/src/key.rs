//! Deterministic step identity — the keystone of `§3.6`
//! (`docs/contract/dynamic-workflows.md`).
//!
//! escurel's existing idempotency is not enough on its own: the ledger's
//! only dedup key is `(tenant, event_id)` and `content_hash` is stored
//! `NULL`, so a reducer that re-runs on every confirmed step (or two steps
//! confirming concurrently) would emit the *same* logical next-step with
//! *different* event ids → duplicate runs. The fix is to give every step a
//! **content-addressed identity** the reducer computes deterministically
//! from `(run_id, phase, slot)`:
//!
//! ```text
//! step_key = blake3(run_id, phase, slot)
//! ```
//!
//! `step_key` is used in three places (which closes the three blockers):
//! 1. the emitted **event id** (a ULID projected from the hash) — a re-run
//!    or concurrent `reduce` produces the *same* id, so `capture_event`'s
//!    `ON CONFLICT (event_id) DO NOTHING` + the ledger's `(tenant,
//!    event_id)` unique index collapse the duplicate;
//! 2. the **pre-flagged produced-instance page id** — a re-driven step
//!    overwrites its own instance rather than forking a new one;
//! 3. (for a barrier) the **vote slot** is the `COUNT(DISTINCT)` key.
//!
//! Determinism is load-bearing: no wall-clock, no randomness — the same
//! `(run_id, phase, slot)` always yields the same key, id, and page id.

/// The raw 32-byte content-addressed step key: `blake3(run_id ‖ phase ‖
/// slot)` with length-delimited fields so `("a","bc")` and `("ab","c")`
/// can never collide.
#[must_use]
pub fn step_key(run_id: &str, phase: &str, slot: &str) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    for field in [run_id, phase, slot] {
        // Length-prefix each field so concatenation is unambiguous.
        hasher.update(&(field.len() as u64).to_le_bytes());
        hasher.update(field.as_bytes());
    }
    *hasher.finalize().as_bytes()
}

/// The deterministic **event id** for a step: the first 16 bytes of the
/// step key projected into a ULID's Crockford-base32 string. It looks and
/// sorts like any other event id, but is a pure function of `(run_id,
/// phase, slot)` — the property that makes emission idempotent.
#[must_use]
pub fn step_event_id(run_id: &str, phase: &str, slot: &str) -> String {
    let key = step_key(run_id, phase, slot);
    let mut hi = [0u8; 16];
    hi.copy_from_slice(&key[..16]);
    ulid::Ulid::from(u128::from_be_bytes(hi)).to_string()
}

/// The deterministic **pre-flagged instance page id** a producing step
/// writes: `markdown/instances/<produces>/<run_slug>-<phase>-<hash12>.md`.
///
/// `run_slug` is the run instance's own id (the last path segment of the
/// run page id, sans `.md`) for human legibility; the 12-hex step-key
/// prefix guarantees uniqueness per logical step and ties the instance to
/// the step. A re-driven step computes the *same* page id, so recovery
/// overwrites rather than forks (`§3.6` point 2).
#[must_use]
pub fn step_instance_page_id(produces: &str, run_id: &str, phase: &str, slot: &str) -> String {
    let key = step_key(run_id, phase, slot);
    let hash12: String = key[..6].iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "markdown/instances/{produces}/{run}-{phase}-{hash12}.md",
        run = run_slug(run_id),
    )
}

/// The run instance's own id: the last path segment of the run page id,
/// with any `.md` suffix and directory prefix stripped. Public so the
/// runner's workflow driver can run-scope a `list_instances` result by the
/// same page-id convention the pre-flagged instance ids use.
#[must_use]
pub fn run_slug(run_id: &str) -> &str {
    run_id
        .rsplit('/')
        .next()
        .unwrap_or(run_id)
        .strip_suffix(".md")
        .unwrap_or_else(|| run_id.rsplit('/').next().unwrap_or(run_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_key_is_deterministic() {
        let a = step_key("run1", "verify", "c12-v0");
        let b = step_key("run1", "verify", "c12-v0");
        assert_eq!(a, b, "same inputs must yield the same key");
    }

    #[test]
    fn step_key_fields_are_unambiguous() {
        // Length-delimiting prevents the classic concatenation collision:
        // ("a","bc",_) must differ from ("ab","c",_).
        assert_ne!(
            step_key("a", "bc", "x"),
            step_key("ab", "c", "x"),
            "field boundaries must be unambiguous"
        );
    }

    #[test]
    fn distinct_slots_yield_distinct_keys_and_ids() {
        assert_ne!(
            step_key("run1", "verify", "c12-v0"),
            step_key("run1", "verify", "c12-v1")
        );
        assert_ne!(
            step_event_id("run1", "verify", "c12-v0"),
            step_event_id("run1", "verify", "c12-v1"),
        );
    }

    #[test]
    fn step_event_id_is_deterministic_and_ulid_shaped() {
        let id = step_event_id("run1", "scope", "0");
        assert_eq!(id, step_event_id("run1", "scope", "0"));
        // A ULID is 26 Crockford-base32 characters.
        assert_eq!(id.len(), 26, "event id is a ULID string: {id}");
        assert!(id.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn instance_page_id_is_deterministic_valid_and_run_scoped() {
        let p = step_instance_page_id(
            "verify-vote",
            "markdown/instances/workflow-run/r1.md",
            "verify",
            "c12-v0",
        );
        assert_eq!(
            p,
            step_instance_page_id(
                "verify-vote",
                "markdown/instances/workflow-run/r1.md",
                "verify",
                "c12-v0",
            )
        );
        assert!(p.starts_with("markdown/instances/verify-vote/r1-verify-"));
        assert!(p.ends_with(".md"));
        // Different slot ⇒ different pre-flagged instance (no fork collision).
        let q = step_instance_page_id(
            "verify-vote",
            "markdown/instances/workflow-run/r1.md",
            "verify",
            "c12-v1",
        );
        assert_ne!(p, q);
    }
}
