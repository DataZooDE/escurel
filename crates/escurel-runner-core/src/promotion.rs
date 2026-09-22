//! The promotion → cascade bridge (knowledge-workbench backend P2-1).
//!
//! A held write does not cascade: nothing has landed. When a human lands
//! it — `promote_draft` / `promote_changeset` — the gateway publishes an
//! `escurel:review` `draft-promoted` event, and the runner's promotion
//! tail turns that into the cascade the run would have emitted had it
//! landed the write itself, under the run's own lineage. This module is
//! the pure half: what a promotion event says, and the deterministic id
//! that makes cascading it idempotent (a retried decision, a changeset's
//! own event, a restart — one cascade).

use escurel_types::Event;

/// The label the gateway publishes review transitions under.
pub const REVIEW_LABEL: &str = "escurel:review";

/// A `draft-promoted` review event, read for the cascade.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromotedDraft {
    pub draft_id: String,
    /// The event the drafting run was triggered by — the ledger's key.
    pub trigger_event_id: String,
    /// The page the promotion landed on.
    pub target_page_id: String,
    /// The run that proposed the draft, when the token that drafted it
    /// carried one.
    pub run_id: Option<String>,
}

/// Read a promotion out of a review event; `None` for every other
/// transition (created, discarded, the changeset-level ones — a changeset
/// promotion is its members' `draft-promoted` events, one each) and for a
/// promotion that names no trigger event (a draft nobody's run made).
#[must_use]
pub fn promoted_draft(event: &Event) -> Option<PromotedDraft> {
    if event.label_skill != REVIEW_LABEL || event.title != "draft-promoted" {
        return None;
    }
    let review = event.provenance.get("review")?;
    let text = |v: &serde_json::Value| v.as_str().filter(|s| !s.is_empty()).map(str::to_owned);
    let body: serde_json::Value = serde_json::from_str(&event.body).unwrap_or_default();
    Some(PromotedDraft {
        draft_id: text(&review["draft_id"])?,
        trigger_event_id: text(&review["event_id"])?,
        target_page_id: text(&body["target_page_id"])
            .or_else(|| Some(event.instance_page_id.clone()).filter(|s| !s.is_empty()))?,
        run_id: text(&review["run_id"]),
    })
}

/// The cascade event's id for a promoted draft: one per draft, whatever
/// route announced the promotion and however often. `capture_event` is
/// first-writer-wins on the id, so the second emission is a no-op.
#[must_use]
pub fn cascade_event_id(draft_id: &str) -> String {
    format!("cascade:{draft_id}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn review(title: &str, review: serde_json::Value, body: serde_json::Value) -> Event {
        Event {
            event_id: format!("review:d1:{title}"),
            label_skill: REVIEW_LABEL.to_owned(),
            title: title.to_owned(),
            body: body.to_string(),
            provenance: json!({ "review": review }),
            instance_page_id: "markdown/instances/note/plan.md".to_owned(),
            ..Event::default()
        }
    }

    #[test]
    fn a_draft_promotion_names_the_draft_its_trigger_and_its_page() {
        let e = review(
            "draft-promoted",
            json!({ "draft_id": "d1", "event_id": "01HTRIG", "run_id": "01HRUN" }),
            json!({ "target_page_id": "markdown/instances/note/plan.md" }),
        );
        assert_eq!(
            promoted_draft(&e),
            Some(PromotedDraft {
                draft_id: "d1".into(),
                trigger_event_id: "01HTRIG".into(),
                target_page_id: "markdown/instances/note/plan.md".into(),
                run_id: Some("01HRUN".into()),
            })
        );
    }

    #[test]
    fn other_transitions_and_triggerless_promotions_are_not_cascades() {
        for title in ["draft-created", "draft-discarded", "changeset-promoted"] {
            let e = review(
                title,
                json!({ "draft_id": "d1", "event_id": "01HTRIG" }),
                json!({}),
            );
            assert!(promoted_draft(&e).is_none(), "{title}");
        }
        // A human's draft answers no event: nothing to cascade from.
        let e = review("draft-promoted", json!({ "draft_id": "d1" }), json!({}));
        assert!(promoted_draft(&e).is_none());
        // Not a review event at all.
        let mut e = review(
            "draft-promoted",
            json!({ "draft_id": "d1", "event_id": "x" }),
            json!({}),
        );
        e.label_skill = "escurel:run".into();
        assert!(promoted_draft(&e).is_none());
    }

    #[test]
    fn the_cascade_id_is_one_per_draft() {
        assert_eq!(cascade_event_id("d1"), "cascade:d1");
        assert_eq!(cascade_event_id("d1"), cascade_event_id("d1"));
    }
}
