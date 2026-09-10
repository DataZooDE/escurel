//! The workflow authoring **dialect** (owner directive, 2026-09-10): a simple,
//! natural-language markdown a curator writes to define a workflow — "numbered
//! prose + inline directives". It compiles to the same internal [`WorkflowSkill`]
//! the reducer already consumes, adding the per-step outcome policy and
//! human-in-the-loop gates the runtime reads.
//!
//! ```text
//! # Reorder review
//! Goal: propose and place a safe reorder.
//!
//! 1. Draft a proposal with [[skill::reorder_policy]].
//! 2. Validate service levels with [[skill::validate_service_level]].
//!    on failure: retry once, then ask a human.
//! 3. A human approves the proposal.   (human-in-the-loop)
//! 4. Place the order with [[skill::place_order]].
//!
//! on any unrecoverable failure: stop and report.
//! ```
//!
//! Grammar (v1): an optional `Goal:` line; a numbered list where each item is a
//! step and the first `[[skill::<id>]]` link is its skill; indented outcome
//! directives (`on failure: retry <N>[, then <ask a human|stop|skip>]`); a step
//! marked `(human-in-the-loop)` (or phrased "a human approves/reviews …") is a
//! human gate; a trailing `on any unrecoverable failure: <fallback>` sets the
//! workflow default. Steps run in order (v1 is sequential — `fan_out: Fixed(1)`,
//! `writes: New`).

use crate::spec::{
    Fallback, FanOut, HUMAN_GATE_SKILL, OutcomePolicy, Phase, VerifyPolicy, WorkflowSkill,
};

/// The workflow-level fallback line — its fallback becomes the default
/// `on_exhausted` for any step that does not author its own `on failure`.
const GLOBAL_FALLBACK_PREFIX: &str = "on any unrecoverable failure:";

/// A dialect parse error — fail closed rather than silently drop a step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialectError(pub String);

impl std::fmt::Display for DialectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "workflow dialect: {}", self.0)
    }
}
impl std::error::Error for DialectError {}

/// Parse a workflow authored in the prose dialect into a [`WorkflowSkill`].
///
/// Fail-closed (dialect F8): every non-blank line must be recognised — a
/// numbered step, a step's indented directive, a heading, the optional `Goal:`
/// line, or the global fallback. An unrecognised flush-left line is a parse
/// error, never silently dropped. The global fallback, if present, is applied
/// as the default `on_exhausted` for any step that does not author its own
/// `on failure`.
pub fn parse_workflow_dialect(id: &str, body: &str) -> Result<WorkflowSkill, DialectError> {
    let lines: Vec<&str> = body.lines().collect();

    // Pass 1: the workflow-level default fallback (F8). Absent ⇒ Stop, matching
    // the per-step default. A malformed global fallback fails the whole parse.
    let mut default_fallback = Fallback::Stop;
    for line in &lines {
        let t = line.trim();
        if let Some(rest) = strip_prefix_ci(t, GLOBAL_FALLBACK_PREFIX) {
            default_fallback = parse_fallback(rest, 0)?;
        }
    }

    let mut phases: Vec<Phase> = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        let line = lines[i];
        if let Some(text) = numbered_item(line) {
            // Gather the step's indented directive lines: more-indented,
            // non-blank, not a new numbered item. A blank line, a new step, or
            // any flush-left line ends this step.
            let mut directives: Vec<&str> = Vec::new();
            let mut j = i + 1;
            while j < lines.len() {
                let l = lines[j];
                if l.trim().is_empty() || numbered_item(l).is_some() {
                    break;
                }
                if !l.starts_with(char::is_whitespace) {
                    break;
                }
                directives.push(l.trim());
                j += 1;
            }
            phases.push(build_step(
                phases.len() + 1,
                text,
                &directives,
                default_fallback,
            )?);
            i = j;
            continue;
        }
        // Not a step: only blanks, a heading, `Goal:`, and the global fallback
        // are consumed silently. Anything else flush-left fails closed — an
        // orphan indented line, or prose the author expected to matter.
        let t = line.trim();
        let recognised = t.is_empty()
            || t.starts_with('#')
            || strip_prefix_ci(t, "goal:").is_some()
            || strip_prefix_ci(t, GLOBAL_FALLBACK_PREFIX).is_some();
        if !recognised {
            return Err(DialectError(format!(
                "unrecognised line {t:?} (expected a numbered step, a heading, `Goal:`, \
                 or the global `on any unrecoverable failure:` fallback)"
            )));
        }
        i += 1;
    }
    if phases.is_empty() {
        return Err(DialectError("no numbered steps found".to_owned()));
    }
    Ok(WorkflowSkill {
        id: id.to_owned(),
        run_skill: crate::spec::DEFAULT_RUN_SKILL.to_owned(),
        harness: None,
        phases,
        verify: VerifyPolicy::default(),
    })
}

/// One numbered step (`step-<n>`), sequential `Fixed(1)` / `New`. A step that
/// authors no `on failure` inherits `default_fallback` (the workflow-level
/// global, else `Stop`).
fn build_step(
    n: usize,
    text: &str,
    directives: &[&str],
    default_fallback: Fallback,
) -> Result<Phase, DialectError> {
    let human_gate = is_human_gate(text);
    let skill = first_skill(text);
    if skill.is_none() && !human_gate {
        return Err(DialectError(format!(
            "step {n} has no [[skill::…]] link and is not a human-in-the-loop gate: {text:?}"
        )));
    }
    let mut outcome: Option<OutcomePolicy> = None;
    for d in directives {
        let dl = d.to_ascii_lowercase();
        if let Some(rest) = dl.strip_prefix("on failure:") {
            if outcome.is_some() {
                return Err(DialectError(format!(
                    "step {n}: more than one `on failure:` directive"
                )));
            }
            outcome = Some(parse_on_failure(rest.trim(), n)?);
        } else {
            return Err(DialectError(format!(
                "step {n}: unrecognised directive {d:?} (expected `on failure: …`)"
            )));
        }
    }
    // A step with no authored `on failure` inherits the workflow default (F8).
    let outcome = outcome.unwrap_or(OutcomePolicy {
        retries: 0,
        on_exhausted: default_fallback,
    });
    // F5: a human gate is a real phase whose instance a human writes; give it a
    // concrete, non-empty `produces` (the gateway rejects an empty label). An
    // explicit skill link on the same step still wins.
    let produces = match skill {
        Some(s) => s,
        None => HUMAN_GATE_SKILL.to_owned(),
    };
    Ok(Phase {
        id: format!("step-{n}"),
        produces,
        fan_out: FanOut::Fixed(1),
        writes: crate::spec::WriteMode::New,
        dedup_by: None,
        max: None,
        max_targets: None,
        harness: None,
        outcome,
        human_gate,
    })
}

/// A step is a human gate iff it carries an explicit gate marker. Deliberately
/// anchored (F9): the bare word "human" is NOT enough — the fallback phrase
/// "then ask a human" lives on a *directive* line, not the step text, and must
/// not accidentally turn a step into a gate.
fn is_human_gate(text: &str) -> bool {
    let l = text.to_ascii_lowercase();
    l.contains("human-in-the-loop")
        || l.contains("a human approves")
        || l.contains("a human reviews")
}

/// `retry <N>[, then <ask a human|stop|skip>]` — or a bare fallback.
fn parse_on_failure(rest: &str, n: usize) -> Result<OutcomePolicy, DialectError> {
    let (retries, fallback_part) = if let Some(after) = rest.strip_prefix("retry") {
        // "retry once, then ask a human" | "retry 3, then stop" | "retry twice"
        let (count_part, then_part) = match after.split_once("then") {
            Some((c, t)) => (c, Some(t)),
            None => (after, None),
        };
        let count = word_to_count(count_part.trim().trim_end_matches(',').trim(), n)?;
        (count, then_part.map(str::trim))
    } else {
        (0, Some(rest))
    };
    let on_exhausted = match fallback_part {
        None => Fallback::Stop, // "retry N" with no fallback ⇒ stop after.
        Some(f) => parse_fallback(f, n)?,
    };
    Ok(OutcomePolicy {
        retries,
        on_exhausted,
    })
}

/// Map a fallback phrase to a [`Fallback`]. Fail-closed on ambiguity (F9): the
/// prose may carry extra words ("stop and report", "ask a human"), but if it
/// mentions more than one of the three actions it is contradictory and rejected
/// rather than resolved by check order. `n == 0` denotes the global fallback.
fn parse_fallback(s: &str, n: usize) -> Result<Fallback, DialectError> {
    let s = s.trim().trim_end_matches('.').trim().to_ascii_lowercase();
    let human = s.contains("human");
    let stop = s.contains("stop");
    let skip = s.contains("skip");
    let where_ = if n == 0 {
        "global fallback".to_owned()
    } else {
        format!("step {n}")
    };
    match (human, stop, skip) {
        (true, false, false) => Ok(Fallback::AskHuman),
        (false, true, false) => Ok(Fallback::Stop),
        (false, false, true) => Ok(Fallback::Skip),
        (false, false, false) => Err(DialectError(format!(
            "{where_}: unknown failure fallback {s:?} (expected ask a human | stop | skip)"
        ))),
        _ => Err(DialectError(format!(
            "{where_}: contradictory fallback {s:?} (names more than one of human/stop/skip)"
        ))),
    }
}

/// Case-insensitive `strip_prefix`, returning the remainder trimmed. The prose
/// dialect is written by humans, so `Goal:`/`goal:` and mixed-case fallback
/// lines must both parse.
fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let s_trim = s.trim_start();
    // Byte comparison so a mismatch that lands mid-UTF-8-char cannot panic; on
    // a match the first `prefix.len()` bytes are all ASCII, so `prefix.len()`
    // is a valid char boundary and the remainder slice is safe.
    if s_trim.len() >= prefix.len()
        && s_trim.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
    {
        Some(s_trim[prefix.len()..].trim())
    } else {
        None
    }
}

fn word_to_count(s: &str, n: usize) -> Result<u32, DialectError> {
    match s {
        "once" => Ok(1),
        "twice" => Ok(2),
        "thrice" => Ok(3),
        other => other.parse::<u32>().map_err(|_| {
            DialectError(format!(
                "step {n}: unrecognised retry count {other:?} (a number, or once/twice/thrice)"
            ))
        }),
    }
}

/// First `[[skill::<id>]]` link in `text`, if any.
fn first_skill(text: &str) -> Option<String> {
    let start = text.find("[[skill::")?;
    let after = &text[start + "[[skill::".len()..];
    let end = after.find("]]")?;
    let id = after[..end].trim();
    (!id.is_empty()).then(|| id.to_owned())
}

/// A numbered list item (`  3. text`) → its text after `N. `, else `None`.
fn numbered_item(line: &str) -> Option<&str> {
    let t = line.trim_start();
    let digits = t.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 || t.as_bytes().get(digits) != Some(&b'.') {
        return None;
    }
    let rest = t[digits + 1..].trim_start();
    (!rest.is_empty()).then_some(rest)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Raw string so the directive line keeps its indentation (a `\`-continuation
    // would strip leading whitespace and detach the `on failure:` directive).
    const REORDER_REVIEW: &str = r#"# Reorder review
Goal: propose and place a safe reorder.

1. Draft a proposal with [[skill::reorder_policy]].
2. Validate service levels with [[skill::validate_service_level]].
   on failure: retry once, then ask a human.
3. A human approves the proposal.   (human-in-the-loop)
4. Place the order with [[skill::place_order]].

on any unrecoverable failure: stop and report.
"#;

    #[test]
    fn parses_the_canonical_four_step_workflow() {
        let wf = parse_workflow_dialect("reorder-review", REORDER_REVIEW).expect("parse");
        assert_eq!(wf.id, "reorder-review");
        assert_eq!(wf.phases.len(), 4, "four numbered steps");

        // 1: plain step, default outcome (fail on error), no gate.
        assert_eq!(wf.phases[0].produces, "reorder_policy");
        assert!(!wf.phases[0].human_gate);
        assert_eq!(wf.phases[0].outcome, OutcomePolicy::default());

        // 2: retry once, then ask a human.
        assert_eq!(wf.phases[1].produces, "validate_service_level");
        assert_eq!(wf.phases[1].outcome.retries, 1);
        assert_eq!(wf.phases[1].outcome.on_exhausted, Fallback::AskHuman);

        // 3: human-in-the-loop gate (no skill link) — F5: it carries the
        // non-empty sentinel `produces`, never an empty label.
        assert!(wf.phases[2].human_gate, "step 3 is a human gate");
        assert_eq!(wf.phases[2].produces, HUMAN_GATE_SKILL);
        assert!(
            !wf.phases[2].produces.is_empty(),
            "a human gate must not compile to an empty produces"
        );

        // 4: plain step; the global fallback ("stop and report") is Stop, and an
        // un-annotated step inherits it (F8 — the global line is actually used).
        assert_eq!(wf.phases[3].produces, "place_order");
        assert_eq!(wf.phases[3].outcome.on_exhausted, Fallback::Stop);
    }

    /// F8: the global `on any unrecoverable failure:` line is APPLIED — an
    /// un-annotated step inherits it, not the hardcoded `Stop`. (Regression: the
    /// old parser dropped the line and the canonical test passed vacuously
    /// because the default already was `Stop`.)
    #[test]
    fn global_fallback_is_applied_to_unannotated_steps() {
        let body = "1. Do it with [[skill::x]].\n\non any unrecoverable failure: ask a human.\n";
        let wf = parse_workflow_dialect("w", body).unwrap();
        assert_eq!(wf.phases[0].outcome.retries, 0);
        assert_eq!(
            wf.phases[0].outcome.on_exhausted,
            Fallback::AskHuman,
            "an un-annotated step inherits the global fallback"
        );
    }

    /// F8: a step's own `on failure` still wins over the global default.
    #[test]
    fn per_step_on_failure_overrides_the_global_fallback() {
        let body = "1. A with [[skill::x]].\n   on failure: retry once, then skip.\n2. B with [[skill::y]].\n\non any unrecoverable failure: ask a human.\n";
        let wf = parse_workflow_dialect("w", body).unwrap();
        // Step 1 authored its own fallback → Skip; step 2 inherits the global.
        assert_eq!(wf.phases[0].outcome.on_exhausted, Fallback::Skip);
        assert_eq!(wf.phases[1].outcome.on_exhausted, Fallback::AskHuman);
    }

    /// F8: an unrecognised flush-left line fails closed rather than being
    /// silently ignored (it might be a directive the author expected to matter).
    #[test]
    fn an_unrecognised_flush_left_line_fails_closed() {
        let body = "1. Do it with [[skill::x]].\n\nplease also notify the team.\n";
        let err = parse_workflow_dialect("w", body).unwrap_err();
        assert!(
            err.0.contains("unrecognised line"),
            "expected an unrecognised-line error, got: {}",
            err.0
        );
    }

    /// F9: a contradictory fallback naming two actions is rejected, not resolved
    /// by check order.
    #[test]
    fn a_contradictory_fallback_fails_closed() {
        let body =
            "1. Do it with [[skill::x]].\n   on failure: retry once, then stop or ask a human.\n";
        let err = parse_workflow_dialect("w", body).unwrap_err();
        assert!(
            err.0.contains("contradictory"),
            "expected a contradictory-fallback error, got: {}",
            err.0
        );
    }

    /// F9: a global fallback line that is contradictory fails the whole parse.
    #[test]
    fn a_contradictory_global_fallback_fails_closed() {
        let body = "1. Do it with [[skill::x]].\n\non any unrecoverable failure: stop, or skip.\n";
        assert!(parse_workflow_dialect("w", body).is_err());
    }

    #[test]
    fn retry_count_words_and_digits() {
        let twice = "1. Do it with [[skill::x]].\n   on failure: retry twice, then skip.\n";
        let wf = parse_workflow_dialect("w", twice).unwrap();
        assert_eq!(wf.phases[0].outcome.retries, 2);
        assert_eq!(wf.phases[0].outcome.on_exhausted, Fallback::Skip);

        let three = "1. Do it with [[skill::x]].\n   on failure: retry 3, then stop.\n";
        let wf = parse_workflow_dialect("w", three).unwrap();
        assert_eq!(wf.phases[0].outcome.retries, 3);
        assert_eq!(wf.phases[0].outcome.on_exhausted, Fallback::Stop);
    }

    #[test]
    fn a_step_with_no_skill_and_no_human_marker_is_an_error() {
        let bad = "1. Just some prose with no skill and no gate.\n";
        assert!(parse_workflow_dialect("w", bad).is_err());
    }

    #[test]
    fn an_unknown_failure_directive_fails_closed() {
        let bad = "1. Do it with [[skill::x]].\n   on failure: explode everything.\n";
        assert!(parse_workflow_dialect("w", bad).is_err());
    }

    #[test]
    fn empty_or_stepless_body_is_an_error() {
        assert!(parse_workflow_dialect("w", "# Title only\nGoal: nothing.\n").is_err());
    }
}
