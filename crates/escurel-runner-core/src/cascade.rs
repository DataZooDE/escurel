//! The cascade emitter — the "change → event" bridge (#156).
//!
//! Lifecycle: after the reconciler (#155) **confirms** a successful write
//! (the triggering event is `processed` and an instance's version advanced),
//! the runner decides whether that change is worth announcing as a follow-on
//! event. `update_page` itself emits no event, so the *runner* — not the
//! server — bridges a confirmed write into a new `capture_event`, tagged with
//! cascade lineage in the existing `provenance` JSON (no schema migration).
//!
//! ## When a write cascades
//!
//! Only a **genuine cross-skill confirmed change** cascades: the produced
//! instance's skill must differ from the triggering event's own
//! `label_skill`. The common single-stage case — an event of skill `X` folded
//! into an `X` instance — is *not* a cascade target, so it never spuriously
//! emits. A meeting event folded into a `decision-record` instance *is*
//! cross-skill, so it cascades a `decision-record` event describing the
//! change. This structural gate (plus the confirmed-effect precondition) is
//! what bounds the chain: a converged hop that produces no cross-skill change
//! stops. The loop/cycle/budget controls proper are #157; this module only
//! emits + tags + lets the event re-enter the same pipeline.
//!
//! ## The lineage carrier
//!
//! The emitted event's `provenance.runner` object carries:
//!
//! ```json
//! { "root_event_id", "parent_event_id", "parent_run_id", "depth",
//!   "lineage_path": [...], "cause", "changed_instance", "changed_version" }
//! ```
//!
//! `depth` = parent depth + 1; `root_event_id` stays the chain's original
//! event; `lineage_path` is the parent's path with this hop's event appended.
//! When the cascaded event is later seen by the runner,
//! [`crate::Trigger::from_event`] reads `provenance.runner` back so the next
//! hop continues the same lineage.

use escurel_client::{CaptureEventRequest, Client};
use serde_json::json;

use crate::{ConfirmedEffect, Trigger};

/// The outcome of a cascade decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CascadeOutcome {
    /// A follow-on event was emitted; carries its new `event_id`.
    Emitted {
        /// The id of the newly captured cascaded event.
        event_id: String,
        /// The skill label the cascaded event was tagged with (the
        /// produced instance's skill).
        label_skill: String,
    },
    /// No cascade: the confirmed write was not a cross-skill change (the
    /// produced instance's skill matched the event's own label), so there
    /// is no follow-on to announce.
    NotCrossSkill,
}

/// Errors raised while emitting a cascade.
#[derive(Debug, thiserror::Error)]
pub enum CascadeError {
    /// The follow-on `capture_event` call against `/mcp` failed.
    #[error("cascade capture_event failed: {0}")]
    Capture(#[source] escurel_client::Error),
}

/// Decide whether a confirmed write should cascade, and if so emit the
/// follow-on event tagged with lineage. Called by the dispatch loop right
/// after the reconciler returns a [`ConfirmedEffect`].
///
/// `parent_trigger` is the trigger this run just processed; `parent_run_id`
/// is this run's ledger id; `effect` is the reconciler's confirmed result
/// (the produced instance + its confirmed version).
///
/// Returns [`CascadeOutcome::NotCrossSkill`] (no emit) when the produced
/// instance's skill equals the parent event's label — the structural gate
/// that prevents a converged same-skill hop from looping.
pub async fn emit_cascade(
    client: &Client,
    parent_trigger: &Trigger,
    parent_run_id: &str,
    effect: &ConfirmedEffect,
) -> Result<CascadeOutcome, CascadeError> {
    let produced_skill = match instance_skill(&effect.instance_page_id) {
        Some(skill) => skill,
        // Cannot derive the produced instance's skill from its page id —
        // treat as non-cascading rather than guess a label.
        None => return Ok(CascadeOutcome::NotCrossSkill),
    };

    // The gate: only a cross-skill confirmed change is a cascade target.
    if produced_skill == parent_trigger.label_skill {
        return Ok(CascadeOutcome::NotCrossSkill);
    }

    let provenance = build_runner_provenance(parent_trigger, parent_run_id, effect);
    let depth = parent_trigger.lineage.depth + 1;
    let title = format!("{produced_skill} updated by {}", parent_trigger.label_skill);
    let body = format!(
        "Instance {} was updated while processing {} event {} (root {}, depth {depth}).",
        effect.instance_page_id,
        parent_trigger.label_skill,
        parent_trigger.event_id,
        parent_trigger.lineage.root_event_id,
    );

    let event = client
        .capture_event(CaptureEventRequest {
            source: "runner-cascade".to_owned(),
            mime: "text/plain".to_owned(),
            label_skill: produced_skill.clone(),
            // Unassigned: the cascaded event re-enters the pipeline as a fresh
            // inbox item. Leaving it unbound is also what makes the chain
            // converge — a no-target hop produces no cross-skill change.
            instance_page_id: String::new(),
            title,
            body,
            provenance,
            ..Default::default()
        })
        .await
        .map_err(CascadeError::Capture)?;

    Ok(CascadeOutcome::Emitted {
        event_id: event.event_id,
        label_skill: produced_skill,
    })
}

/// Build the `provenance.runner` lineage object for a cascaded event:
/// `depth` = parent depth + 1, the stable `root_event_id`, and a
/// `lineage_path` that is the parent's path extended to (and including) the
/// parent event — so the emitted hop reads root → … → parent. The next
/// normalisation ([`crate::Trigger::from_event`]) reads this back to continue
/// the chain.
fn build_runner_provenance(
    parent_trigger: &Trigger,
    parent_run_id: &str,
    effect: &ConfirmedEffect,
) -> serde_json::Value {
    let parent_lineage = &parent_trigger.lineage;
    let depth = parent_lineage.depth + 1;
    let mut lineage_path = parent_lineage.lineage_path.clone();
    if lineage_path.last().map(String::as_str) != Some(parent_trigger.event_id.as_str()) {
        lineage_path.push(parent_trigger.event_id.clone());
    }
    json!({
        "runner": {
            "root_event_id": parent_lineage.root_event_id,
            "parent_event_id": parent_trigger.event_id,
            "parent_run_id": parent_run_id,
            "depth": depth,
            "lineage_path": lineage_path,
            "cause": format!("instance-updated:{}", parent_trigger.label_skill),
            "changed_instance": effect.instance_page_id,
            "changed_version": effect.version,
        }
    })
}

/// Derive an instance's skill from its page id
/// (`markdown/instances/<skill>/<id>.md`). Returns `None` when the path does
/// not match that shape.
fn instance_skill(page_id: &str) -> Option<String> {
    let rest = page_id.strip_prefix("markdown/instances/")?;
    let (skill, _) = rest.split_once('/')?;
    if skill.is_empty() {
        return None;
    }
    Some(skill.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Lineage;

    fn trigger(label: &str, event_id: &str, lineage: Lineage) -> Trigger {
        Trigger {
            tenant: "acme".into(),
            event_id: event_id.into(),
            label_skill: label.into(),
            instance_page_id: Some("markdown/instances/decision-record/x.md".into()),
            lineage,
        }
    }

    fn effect() -> ConfirmedEffect {
        ConfirmedEffect {
            instance_page_id: "markdown/instances/decision-record/q3.md".into(),
            version: "sha256:abc".into(),
        }
    }

    #[test]
    fn instance_skill_parses_the_path() {
        assert_eq!(
            instance_skill("markdown/instances/decision-record/q3.md").as_deref(),
            Some("decision-record")
        );
        assert_eq!(instance_skill("markdown/skills/note.md"), None);
        assert_eq!(instance_skill("markdown/instances//q3.md"), None);
    }

    #[test]
    fn provenance_from_a_root_parent_is_depth_one() {
        // A depth-0 (webhook-origin) parent cascades a depth-1 hop whose
        // root stays the parent and whose path is root → parent.
        let parent = trigger("meeting", "ROOT0", Lineage::root("ROOT0"));
        let runner = build_runner_provenance(&parent, "run-7", &effect())["runner"].clone();
        assert_eq!(runner["depth"], json!(1));
        assert_eq!(runner["root_event_id"], json!("ROOT0"));
        assert_eq!(runner["parent_event_id"], json!("ROOT0"));
        assert_eq!(runner["parent_run_id"], json!("run-7"));
        assert_eq!(runner["lineage_path"], json!(["ROOT0"]));
        assert_eq!(
            runner["changed_instance"],
            json!("markdown/instances/decision-record/q3.md")
        );
        assert_eq!(runner["changed_version"], json!("sha256:abc"));
    }

    #[test]
    fn provenance_extends_a_deeper_chain() {
        // A depth-1 parent (itself cascaded) extends to depth 2, keeping the
        // original root and appending its own event to the path.
        let parent = trigger(
            "decision-record",
            "HOP1",
            Lineage {
                root_event_id: "ROOT0".into(),
                depth: 1,
                lineage_path: vec!["ROOT0".into(), "HOP1".into()],
            },
        );
        let runner = build_runner_provenance(&parent, "run-9", &effect())["runner"].clone();
        assert_eq!(runner["depth"], json!(2));
        assert_eq!(runner["root_event_id"], json!("ROOT0"));
        assert_eq!(runner["lineage_path"], json!(["ROOT0", "HOP1"]));
    }
}
