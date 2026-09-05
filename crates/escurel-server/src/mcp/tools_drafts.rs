//! Held writes: `create_draft`, `list_drafts`, `promote_draft`, `discard_draft`.
//!
//! The third piece of the human-in-the-loop gate. escurel already had the
//! other two — a skill declares `autonomy: auto|review|confirm`
//! (`escurel-index/src/read.rs`), and a write can be approved against exact
//! bytes (`update_page`'s `base_sha256`, #354) — but nothing in between:
//! there was nowhere to put a change that is finished and not yet wanted, so
//! an agent could only write or not write. Consumers filled the gap
//! privately, which put consumer-shaped objects in the knowledge base and
//! made "what is waiting for me?" a question only that consumer could answer.
//!
//! Two rules give this surface its safety, and both are enforced here rather
//! than documented:
//!
//! 1. **A draft may not stage a write its author could never make.** The
//!    write ACL runs at `create_draft`, against the target and the proposed
//!    content, exactly as `update_page` runs it. Deferring the check to
//!    promotion would let an unauthorised author queue work that a
//!    reviewer's authority then lands.
//! 2. **Promotion goes through `update_page`.** It is not a second write
//!    path: `promote_draft` re-enters `tool_update_page` with the draft's
//!    bytes and its `base_sha256`, so the layer/backend/curator guards, the
//!    validation, the CAS, the lake publish and the provenance stamp all
//!    apply at the moment bytes actually land — under the approver's
//!    identity, which is whose authority the write is made on.

use super::*;
use escurel_index::drafts::NewDraft;

#[derive(Deserialize)]
pub(super) struct CreateDraftArgs {
    target_page_id: String,
    content: String,
    /// The target's `content_sha256` when the draft was written, as published
    /// by `expand`; `""` means "no page yet" (the create case). Carried
    /// verbatim into `update_page`'s CAS at promotion, so a target that moved
    /// underneath a pending draft conflicts instead of being clobbered.
    #[serde(default)]
    base_sha256: Option<String>,
    /// The inbox event this draft answers, when it answers one.
    #[serde(default)]
    event_id: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct ListDraftsArgs {
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Deserialize)]
pub(super) struct DecideDraftArgs {
    draft_id: String,
    /// Why it was discarded. Ignored by `promote_draft`.
    #[serde(default)]
    reason: String,
}

fn draft_to_json(d: &escurel_index::drafts::DraftInfo) -> Value {
    json!({
        "draft_id": d.draft_id,
        "target_page_id": d.target_page_id,
        "content": d.content,
        "content_sha256": d.content_sha256,
        "base_sha256": d.base_sha256,
        "author": d.author,
        "event_id": d.event_id,
        "status": d.status,
        "reason": d.reason,
        "decided_by": d.decided_by,
        "created_at": d.created_at,
    })
}

/// Hold a finished write for a human.
pub(super) async fn tool_create_draft(
    state: &crate::server::AppState,
    indexer: &Indexer,
    caller: AclCaller<'_>,
    write_acl: crate::server::WriteAclMode,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: CreateDraftArgs = parse_args(args, "create_draft")?;

    // Rule 1. Same call, same arguments and same three modes as
    // `update_page`'s — a draft is a write in waiting, and the authority to
    // make it is checked when it is written, not when it is approved.
    if write_acl != crate::server::WriteAclMode::Off {
        let allowed = indexer
            .may_write_page(&caller, &a.target_page_id, &a.content)
            .await
            .map_err(|e| JsonRpcError::internal(format!("create_draft acl: {e}")))?;
        if !allowed {
            if write_acl == crate::server::WriteAclMode::Log {
                tracing::warn!(
                    subject = %caller.subject,
                    page_id = %a.target_page_id,
                    "write-ACL would deny this draft (log mode) — allowing"
                );
            } else {
                return Ok(json!({
                    "ok": false,
                    "issues": [{
                        "severity": "error",
                        "code": "forbidden",
                        "location": "target_page_id",
                        "message": format!(
                            "draft denied: caller `{}` does not own instance `{}`",
                            caller.subject, a.target_page_id
                        ),
                    }],
                }));
            }
        }
    }

    // Validate at DRAFT time, with the same blocking set promotion will
    // apply. A draft that cannot be promoted is worse than a refused write:
    // it costs a human a review before anyone finds out.
    let issues = indexer
        .validate(Some(&a.target_page_id), &a.content)
        .await
        .map_err(|e| JsonRpcError::internal(format!("create_draft validate: {e}")))?;
    let blocking = crate::mcp::tools_write::blocking_issues(state, &issues);
    if !blocking.is_empty() {
        return Ok(json!({
            "ok": false,
            "issues": issues.iter().map(issue_to_json).collect::<Vec<_>>(),
        }));
    }

    let stored = indexer
        .create_draft(NewDraft {
            target_page_id: a.target_page_id,
            content: a.content,
            base_sha256: a.base_sha256,
            // The author is the verified token subject, never an argument:
            // "who proposed this?" is the first thing a reviewer asks, and a
            // caller-supplied answer to it is not evidence.
            author: caller.subject.to_owned(),
            event_id: a.event_id,
        })
        .await
        .map_err(|e| JsonRpcError::internal(format!("create_draft: {e}")))?;

    Ok(json!({ "ok": true, "draft": draft_to_json(&stored) }))
}

/// Everything still waiting, newest first.
pub(super) async fn tool_list_drafts(
    indexer: &Indexer,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: ListDraftsArgs = parse_args(args, "list_drafts")?;
    let drafts = indexer
        .list_drafts(a.limit)
        .await
        .map_err(|e| JsonRpcError::internal(format!("list_drafts: {e}")))?;
    Ok(json!({
        "drafts": drafts.iter().map(draft_to_json).collect::<Vec<_>>(),
    }))
}

/// Land a held write, under the approver's identity.
pub(super) async fn tool_promote_draft(
    state: &crate::server::AppState,
    indexer: &Indexer,
    caller: AclCaller<'_>,
    write_acl: crate::server::WriteAclMode,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: DecideDraftArgs = parse_args(args, "promote_draft")?;
    let Some(draft) = indexer
        .get_draft(&a.draft_id)
        .await
        .map_err(|e| JsonRpcError::internal(format!("promote_draft: {e}")))?
    else {
        return Ok(json!({
            "ok": false,
            "issues": [{
                "severity": "error",
                "code": "not_found",
                "location": "draft_id",
                "message": format!("no draft `{}`", a.draft_id),
            }],
        }));
    };
    if draft.status != "open" {
        // Not an error the caller can retry away: the decision was already
        // taken, and the honest answer is which one.
        return Ok(json!({
            "ok": false,
            "issues": [{
                "severity": "error",
                "code": "already_decided",
                "location": "draft_id",
                "message": format!(
                    "draft `{}` was already {} by `{}`",
                    draft.draft_id, draft.status, draft.decided_by
                ),
            }],
            "draft": draft_to_json(&draft),
        }));
    }

    // Rule 2. `update_page` is the only write path; this is a call into it,
    // not a copy of it. `base_sha256` travels as the draft recorded it — a
    // target that moved since drafting conflicts here rather than silently
    // overwriting what the reviewer never saw.
    let subject = caller.subject.to_owned();
    let mut write_args = json!({
        "page_id": draft.target_page_id,
        "content": draft.content,
    });
    if let Some(base) = &draft.base_sha256 {
        write_args["base_sha256"] = json!(base);
    }
    let result =
        crate::mcp::tools_write::tool_update_page(state, indexer, caller, write_acl, write_args)
            .await?;

    // Close the draft ONLY on a landed write. An `ok:false` (a stale CAS, a
    // validation refusal) leaves it open, which is what makes a re-drafted
    // proposal the answer rather than a lost queue entry.
    if result.get("ok").and_then(Value::as_bool) == Some(true) {
        indexer
            .close_draft(&draft.draft_id, "promoted", &subject, "")
            .await
            .map_err(|e| JsonRpcError::internal(format!("promote_draft close: {e}")))?;
    }
    Ok(result)
}

/// Refuse a held write. Nothing is written to the page.
pub(super) async fn tool_discard_draft(
    indexer: &Indexer,
    caller: AclCaller<'_>,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: DecideDraftArgs = parse_args(args, "discard_draft")?;
    let closed = indexer
        .close_draft(&a.draft_id, "discarded", caller.subject, &a.reason)
        .await
        .map_err(|e| JsonRpcError::internal(format!("discard_draft: {e}")))?;
    if !closed {
        return Ok(json!({
            "ok": false,
            "issues": [{
                "severity": "error",
                "code": "not_found",
                "location": "draft_id",
                "message": format!("no OPEN draft `{}`", a.draft_id),
            }],
        }));
    }
    Ok(json!({ "ok": true, "draft_id": a.draft_id }))
}
