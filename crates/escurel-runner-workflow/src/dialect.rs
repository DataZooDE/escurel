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

use crate::spec::{Fallback, FanOut, OutcomePolicy, Phase, VerifyPolicy, WorkflowSkill};

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
pub fn parse_workflow_dialect(id: &str, body: &str) -> Result<WorkflowSkill, DialectError> {
    let lines: Vec<&str> = body.lines().collect();
    let mut phases: Vec<Phase> = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        let Some(text) = numbered_item(lines[i]) else {
            i += 1;
            continue;
        };
        // Gather the step's indented directive lines: more-indented,
        // non-blank, not a new numbered item. A blank line, a new step, or a
        // flush-left line (e.g. the global fallback) ends this step.
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
        phases.push(build_step(phases.len() + 1, text, &directives)?);
        i = j;
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

/// One numbered step (`step-<n>`), sequential `Fixed(1)` / `New`.
fn build_step(n: usize, text: &str, directives: &[&str]) -> Result<Phase, DialectError> {
    let lower = text.to_ascii_lowercase();
    let human_gate = lower.contains("human-in-the-loop")
        || lower.contains("human approve")
        || lower.contains("human review")
        || lower.contains("ask a human");
    let skill = first_skill(text);
    if skill.is_none() && !human_gate {
        return Err(DialectError(format!(
            "step {n} has no [[skill::…]] link and is not a human-in-the-loop gate: {text:?}"
        )));
    }
    let mut outcome = OutcomePolicy::default();
    for d in directives {
        let dl = d.to_ascii_lowercase();
        if let Some(rest) = dl.strip_prefix("on failure:") {
            outcome = parse_on_failure(rest.trim(), n)?;
        } else {
            return Err(DialectError(format!(
                "step {n}: unrecognised directive {d:?} (expected `on failure: …`)"
            )));
        }
    }
    Ok(Phase {
        id: format!("step-{n}"),
        produces: skill.unwrap_or_default(),
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

fn parse_fallback(s: &str, n: usize) -> Result<Fallback, DialectError> {
    let s = s.trim().trim_end_matches('.').trim();
    if s.contains("human") {
        Ok(Fallback::AskHuman)
    } else if s.contains("stop") {
        Ok(Fallback::Stop)
    } else if s.contains("skip") {
        Ok(Fallback::Skip)
    } else {
        Err(DialectError(format!(
            "step {n}: unknown failure fallback {s:?} (expected ask a human | stop | skip)"
        )))
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

        // 3: human-in-the-loop gate (no skill link).
        assert!(wf.phases[2].human_gate, "step 3 is a human gate");

        // 4: plain step; the global fallback is Stop.
        assert_eq!(wf.phases[3].produces, "place_order");
        assert_eq!(wf.phases[3].outcome.on_exhausted, Fallback::Stop);
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
