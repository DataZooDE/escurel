//! Controls as events (knowledge-workbench backend P2-2): the authorisation
//! branch `capture_event` runs for the `escurel:run-control` label.
//!
//! A human cancels, retries, pauses, resumes or requeues by capturing a
//! control event; the runner's subscriber acts on it. The gateway stays
//! automation-free — it decides only WHO may ask WHAT, and stamps who
//! asked:
//!
//! - `cancel` / `retry` name a run. The caller must be allowed to write the
//!   run's target page (the page its `run-started` names), under the same
//!   `ESCUREL_WRITE_ACL` gate `update_page` applies. An unknown run and a
//!   forbidden one read the same — `event_not_found` — so the label is no
//!   existence oracle for runs on pages the caller may not see.
//! - `pause` / `resume` / `requeue` are tenant-wide and admin-only; a
//!   non-admin reads the same `event_not_found`.
//! - A malformed request (no action, an unknown one, `cancel` without a
//!   run, a body that is not JSON) is a caller mistake: plain
//!   `invalid_params`, no `event_not_found`.
//!
//! The request is stored as bookkeeping (`kind: system`) on the run's
//! target page — never inbox work, hidden from the default listings,
//! present in the run's own record (`list_events{run_id}`) and under the
//! label the runner tails. `provenance.control` is written here from the
//! token: `{action, run_id?, event_id?, reason?, requested_by}`; a
//! caller-supplied block is replaced, never merged.

use escurel_index::{AclCaller, EventListFilter, Indexer};
use serde_json::{Value, json};

use super::JsonRpcError;

/// The label a control request is captured under.
pub(super) const RUN_CONTROL_LABEL: &str = "escurel:run-control";

/// The actions a control event may carry, and who may ask for them.
const RUN_ACTIONS: [&str; 2] = ["cancel", "retry"];
const TENANT_ACTIONS: [&str; 3] = ["pause", "resume", "requeue"];

/// How far back to look for the run's `run-started` in the run's own
/// record (a run has a handful of lifecycle rows plus progress snapshots).
const RUN_SCAN: usize = 128;

/// What `capture_event` stores for an authorised control request.
pub(super) struct RunControl {
    /// The run's target page for `cancel` / `retry`; `None` tenant-wide.
    pub instance_page_id: Option<String>,
    pub run_id: Option<String>,
    pub root_event_id: Option<String>,
    /// The `provenance.control` block, requester stamped.
    pub control: Value,
}

fn mistake(msg: impl std::fmt::Display) -> JsonRpcError {
    JsonRpcError::invalid_params(format!("capture_event: `escurel:run-control`: {msg}"))
}

/// One message for "no such run" and "not yours to control": a run on a
/// page the caller may not write must not be confirmed to exist.
fn denied() -> JsonRpcError {
    JsonRpcError::invalid_params(
        "capture_event: `escurel:run-control`: no such run, or not yours to control".to_owned(),
    )
    .with_code("event_not_found", false)
}

fn text(v: &Value) -> Option<&str> {
    v.as_str().filter(|s| !s.is_empty())
}

/// Authorise a control request against the caller and the run it names.
pub(super) async fn authorise_run_control(
    indexer: &Indexer,
    caller: &AclCaller<'_>,
    write_acl: crate::server::WriteAclMode,
    body: &str,
) -> Result<RunControl, JsonRpcError> {
    let req: Value = serde_json::from_str(body).map_err(|e| {
        mistake(format!(
            "the body is JSON `{{action, run_id?, reason?}}` ({e})"
        ))
    })?;
    let Some(action) = text(&req["action"]) else {
        return Err(mistake(format!(
            "`action` is one of {} (per run) or {} (tenant-wide, admin)",
            RUN_ACTIONS.join("|"),
            TENANT_ACTIONS.join("|")
        )));
    };
    let mut control = json!({ "action": action, "requested_by": caller.subject });
    if let Some(reason) = text(&req["reason"]) {
        control["reason"] = json!(reason);
    }

    if RUN_ACTIONS.contains(&action) {
        let Some(run_id) = text(&req["run_id"]) else {
            return Err(mistake(format!("`{action}` names a `run_id`")));
        };
        // The run's own record names its target page on `run-started`.
        let page = indexer
            .list_events_filtered_page(
                &EventListFilter {
                    run_id: Some(run_id.to_owned()),
                    include_system: true,
                    ..Default::default()
                },
                true,
                RUN_SCAN,
                None,
            )
            .await
            .map_err(|e| JsonRpcError::internal(format!("capture_event run-control: {e}")))?;
        let Some(started) = page
            .events
            .iter()
            .find(|e| e.label_skill == "escurel:run" && e.title == "run-started")
        else {
            return Err(denied());
        };
        let target = started.instance_page_id.clone().filter(|p| !p.is_empty());
        if !caller.is_admin {
            // A non-admin controls a run through the page it works on: the
            // same write gate `update_page` applies to that page.
            let Some(target) = target.as_deref() else {
                return Err(denied());
            };
            if write_acl != crate::server::WriteAclMode::Off {
                let Some(existing) = indexer.read_page_markdown(target).await.map_err(|e| {
                    JsonRpcError::internal(format!("capture_event run-control: {e}"))
                })?
                else {
                    return Err(denied());
                };
                let allowed = indexer
                    .may_write_page(caller, target, &existing)
                    .await
                    .map_err(|e| {
                        JsonRpcError::internal(format!("capture_event run-control acl: {e}"))
                    })?;
                if !allowed {
                    if write_acl == crate::server::WriteAclMode::Log {
                        tracing::warn!(
                            subject = %caller.subject, run_id, page_id = %target,
                            "write-ACL would refuse this run-control request (log mode) — allowing"
                        );
                    } else {
                        return Err(denied());
                    }
                }
            }
        }
        control["run_id"] = json!(run_id);
        return Ok(RunControl {
            instance_page_id: target,
            run_id: Some(run_id.to_owned()),
            root_event_id: started.root_event_id.clone(),
            control,
        });
    }

    if TENANT_ACTIONS.contains(&action) {
        if !caller.is_admin {
            return Err(denied());
        }
        if action == "requeue" {
            // Requeue names the dead-lettered event to put back.
            let Some(event_id) = text(&req["event_id"]) else {
                return Err(mistake("`requeue` names the dead-lettered `event_id`"));
            };
            control["event_id"] = json!(event_id);
        }
        return Ok(RunControl {
            instance_page_id: None,
            run_id: None,
            root_event_id: None,
            control,
        });
    }

    Err(mistake(format!(
        "unknown action `{action}`; one of {} (per run) or {} (tenant-wide, admin)",
        RUN_ACTIONS.join("|"),
        TENANT_ACTIONS.join("|")
    )))
}

// ── review comments ───────────────────────────────────────────────

/// The reserved label a human's review comment is filed under (VS Code
/// workbench PR-3). Reserved on purpose: the runner drops every
/// `escurel:`-prefixed event, so a comment is never mistaken for work to
/// dispatch — the label it would otherwise need (`review-comment`) names no
/// skill, and such an event dead-letters.
pub(super) const REVIEW_COMMENT_LABEL: &str = "escurel:review-comment";

/// What `capture_event` stores for an authorised comment.
pub(super) struct ReviewComment {
    /// The draft's target page: where the comment is filed.
    pub instance_page_id: Option<String>,
    /// The `provenance.review` block, author and the draft's lineage
    /// stamped. `capture_event` reads `root_event_id` / `run_id` back out of
    /// it into the event's lineage columns, so a comment folds into the
    /// thread of the run that proposed the draft.
    pub review: Value,
}

/// Authorise a review comment against the draft it is about.
///
/// The gate is the draft's own visibility — a reviewer who may SEE a draft
/// may say something about it — and a draft the caller may not see is
/// refused exactly as a missing one is, so the comment surface confirms no
/// draft's existence.
pub(super) async fn authorise_review_comment(
    indexer: &Indexer,
    caller: &AclCaller<'_>,
    provenance: Option<&Value>,
) -> Result<ReviewComment, JsonRpcError> {
    let mistake = |msg: &str| {
        JsonRpcError::invalid_params(format!("capture_event: `{REVIEW_COMMENT_LABEL}`: {msg}"))
    };
    let denied = || {
        JsonRpcError::invalid_params(format!(
            "capture_event: `{REVIEW_COMMENT_LABEL}`: no such draft, or not yours to see"
        ))
        .with_code("event_not_found", false)
    };
    let review = provenance.and_then(|p| p.get("review"));
    let Some(draft_id) = review.and_then(|r| text(&r["draft_id"])) else {
        return Err(mistake(
            "`provenance.review.draft_id` names the draft the comment is about",
        ));
    };
    let draft = indexer
        .get_draft(draft_id)
        .await
        .map_err(|e| JsonRpcError::internal(format!("capture_event: draft: {e}")))?
        .ok_or_else(denied)?;
    if !super::tools_drafts::may_see(indexer, caller, &draft).await? {
        return Err(denied());
    }
    let mut out = json!({ "draft_id": draft.draft_id, "commented_by": caller.subject });
    // The line a comment hangs on is the caller's to state; everything else
    // here comes from the draft row.
    if let Some(line) = review.and_then(|r| r["line"].as_i64()) {
        out["line"] = json!(line);
    }
    if let Some(changeset) = &draft.changeset_id {
        out["changeset_id"] = json!(changeset);
    }
    // The draft's lineage, so the comment lands in the same thread as the
    // run that proposed it (`lineage_from_provenance` reads these).
    if let Some(root) = &draft.root_event_id {
        out["root_event_id"] = json!(root);
    }
    if let Some(run) = &draft.run_id {
        out["run_id"] = json!(run);
    }
    Ok(ReviewComment {
        instance_page_id: Some(draft.target_page_id.clone()),
        review: out,
    })
}
