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

/// The label a thread close/reopen request is captured under (VS Code
/// workbench PR-9). One label with a body `{action: close|reopen, ...}`,
/// like `escurel:run-control`'s one label with `{action: cancel|retry|…}`:
/// the runner drops every `escurel:`-prefixed event, so a close is never
/// mistaken for work to dispatch.
pub(super) const THREAD_CLOSED_LABEL: &str = "escurel:thread-closed";

/// The actions a thread request may carry.
const THREAD_ACTIONS: [&str; 2] = ["close", "reopen"];

/// The longest `reason` a close may carry, and the longest `root_title` stamped, in characters.
const THREAD_TEXT_MAX: usize = 200;

/// How many of the thread's own label rows to scan for the latest
/// close/reopen. Closes are rare per root; the scan is newest-first, so
/// the first readable row with a `thread.action` is the current state.
const THREAD_SCAN: usize = 8;

/// What `capture_event` stores for an authorised thread close/reopen.
pub(super) struct ThreadClose {
    /// The root's page: the request is filed into the thread's own record.
    pub instance_page_id: Option<String>,
    pub root_event_id: String,
    /// The `provenance.thread` block, requester stamped.
    pub thread: Value,
    /// The latest close/reopen row for this root the caller may read, if
    /// any — so re-asking for the state it already has is a no-op answer.
    pub current: Option<escurel_index::EventInfo>,
}

fn thread_mistake(msg: impl std::fmt::Display) -> JsonRpcError {
    JsonRpcError::invalid_params(format!("capture_event: `{THREAD_CLOSED_LABEL}`: {msg}"))
}

/// One message for "no such thread", "not yours to see" and "a machine
/// asked": a thread the caller may not read must not be confirmed to
/// exist, and a run-bound token learns nothing either.
fn thread_denied() -> JsonRpcError {
    JsonRpcError::invalid_params(
        "capture_event: `escurel:thread-closed`: no such thread, or not yours to see".to_owned(),
    )
    .with_code("event_not_found", false)
}

/// Authorise a thread close/reopen against the caller and the root it names.
///
/// The gate is the root's own readability — whoever may SEE a thread may
/// close or reopen it — and a root the caller may not see is refused
/// exactly as a missing one is. A machine caller (a run-bound token, a
/// narrowed agent, one acting for a runner) is refused the same way even
/// when the root is readable: closing is a human dismissal, and #686's
/// bookkeeper bar already keeps that whole class off `escurel:` labels.
/// A malformed request (no action, an unknown one, no root, a body that
/// is not JSON) is a caller mistake: plain `invalid_params`, no
/// `event_not_found`.
pub(super) async fn authorise_thread_close(
    indexer: &Indexer,
    caller: &AclCaller<'_>,
    body: &str,
) -> Result<ThreadClose, JsonRpcError> {
    let req: Value = serde_json::from_str(body).map_err(|e| {
        thread_mistake(format!(
            "the body is JSON `{{action, root_event_id, reason?}}` ({e})"
        ))
    })?;
    let Some(action) = text(&req["action"]) else {
        return Err(thread_mistake(format!(
            "`action` is one of {}",
            THREAD_ACTIONS.join("|")
        )));
    };
    if !THREAD_ACTIONS.contains(&action) {
        return Err(thread_mistake(format!(
            "unknown action `{action}`; one of {}",
            THREAD_ACTIONS.join("|")
        )));
    }
    let Some(root_id) = text(&req["root_event_id"]) else {
        return Err(thread_mistake(format!(
            "`{action}` names a `root_event_id`"
        )));
    };
    if super::tools_write::is_machine_caller(caller) {
        return Err(thread_denied());
    }
    let root = indexer
        .get_event(root_id)
        .await
        .map_err(|e| JsonRpcError::internal(format!("capture_event thread-closed: {e}")))?
        .ok_or_else(thread_denied)?;
    if !indexer
        .may_read_event(caller, &root)
        .await
        .map_err(|e| JsonRpcError::internal(format!("capture_event thread-closed acl: {e}")))?
    {
        return Err(thread_denied());
    }
    let mut thread = json!({
        "action": action,
        "root_event_id": root.event_id,
        "requested_by": caller.subject,
    });
    if let Some(reason) = text(&req["reason"]) {
        // A human's free text, shown in other people's outlines: bounded here.
        if reason.chars().count() > THREAD_TEXT_MAX {
            return Err(thread_mistake(format!(
                "`reason` is at most {THREAD_TEXT_MAX} characters"
            )));
        }
        thread["reason"] = json!(reason);
    }
    // The root's title at close time, so the outline's "Closed" group reads
    // it off the label listing instead of fetching every closed root (event
    // rows are immutable, so it cannot go stale).
    if !root.title.is_empty() {
        // The server's own data, but unbounded: stamped cut on a character boundary.
        let title: String = root.title.chars().take(THREAD_TEXT_MAX).collect();
        thread["root_title"] = json!(title);
    }
    // The current state, newest first: the first row this caller may read
    // that carries a close/reopen action. Rows on pages the caller may not
    // read are skipped, not trusted — the state a caller sees is the state
    // it may see.
    let page = indexer
        .list_events_filtered_page(
            &EventListFilter {
                root_event_id: Some(root.event_id.clone()),
                label_skill: Some(THREAD_CLOSED_LABEL.to_owned()),
                include_system: true,
                ..Default::default()
            },
            false,
            THREAD_SCAN,
            None,
        )
        .await
        .map_err(|e| JsonRpcError::internal(format!("capture_event thread-closed: {e}")))?;
    let mut current = None;
    for e in &page.events {
        let action = e
            .provenance
            .get("thread")
            .and_then(|t| t.get("action"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if !THREAD_ACTIONS.contains(&action) {
            continue;
        }
        let readable = indexer
            .may_read_event(caller, e)
            .await
            .map_err(|e| JsonRpcError::internal(format!("capture_event thread-closed acl: {e}")))?;
        if readable {
            current = Some(e.clone());
            break;
        }
    }
    Ok(ThreadClose {
        instance_page_id: root.instance_page_id.clone(),
        root_event_id: root.event_id.clone(),
        thread,
        current,
    })
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
