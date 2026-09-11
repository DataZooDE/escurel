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
    /// The HUMAN who decided, when the caller is deciding on their behalf.
    ///
    /// A gateway in front of escurel authenticates a person and then writes
    /// with its OWN credential, because escurel's write ACL matches on groups
    /// and a person's minted bearer carries none (heron#100). Without this
    /// field the decision is recorded against that service identity and the
    /// person who actually approved is stored nowhere — which is what heron's
    /// BR-HIL-6 ("every change carries an approver") asks for and what the
    /// draft path silently stopped providing when it replaced the proposal
    /// path. Measured on lab: pages promoted from drafts read
    /// `last_written_by: heron-onbehalf`, and no record named the consultant.
    ///
    /// **Admin only.** The claim is "I verified this person", which is exactly
    /// the claim a caller must not be able to make about itself; a non-admin
    /// sending it is refused rather than ignored, so a client cannot quietly
    /// forge an audit trail and believe it worked.
    ///
    /// `last_written_by` on the page is untouched and still says who WROTE —
    /// the service — because that field is stamped from the verified token and
    /// must keep meaning exactly that (#357).
    #[serde(default)]
    decided_by: Option<String>,
}

/// Who to record as having decided: the human the caller vouches for, or the
/// caller itself.
///
/// See [`DecideDraftArgs::decided_by`] for why the field exists. The admin
/// check is the whole of its security: "this person approved it" is a claim
/// about someone else, and a caller that could make it about itself could
/// write any name into the audit trail. Refused rather than ignored — a
/// gateway that silently lost the attribution is how this was missed the
/// first time.
fn decided_by_or_caller(
    a: &DecideDraftArgs,
    caller: &AclCaller<'_>,
) -> Result<String, JsonRpcError> {
    match a.decided_by.as_deref().map(str::trim) {
        None | Some("") => Ok(caller.subject.to_owned()),
        // Naming YOURSELF is not vouching for anyone — it is the same fact the
        // token already carries, and refusing it made the argument unusable by
        // any caller that decides on its own behalf. heron sends it
        // unconditionally (its service credential is optional, and when it is
        // absent the consultant's own bearer does the write), so the strict
        // rule turned every approval in that shape into `invalid_params`.
        // Caught by heron's `draft_verbs` suite the moment the field shipped.
        Some(human) if human == caller.subject => Ok(human.to_owned()),
        Some(_) if !caller.is_admin => Err(JsonRpcError::invalid_params(
            "`decided_by` names the human a gateway verified, and only an \
             admin may vouch for another subject"
                .to_owned(),
        )),
        Some(human) => Ok(human.to_owned()),
    }
}

/// Whether `caller` may SEE this draft, decided from the proposed content's
/// own frontmatter — the same question, answered the same way, as for the
/// page it would become.
///
/// This is not optional hardening. escurel's own deployment model is one
/// shared tenant with several people in it (heron's D7), so an unfiltered
/// queue would show every consultant every other consultant's held writes,
/// including the content. A draft carries no owner column on purpose: two
/// places answering "who may read this?" from two different sources
/// eventually disagree, and the disagreement is silent.
///
/// Unparseable content fails CLOSED — nobody but an admin sees a draft whose
/// ACL cannot be determined.
async fn may_see(
    indexer: &Indexer,
    caller: &AclCaller<'_>,
    draft: &escurel_index::drafts::DraftInfo,
) -> Result<bool, JsonRpcError> {
    if caller.is_admin {
        return Ok(true);
    }
    let Ok(parsed) = escurel_md::parse(&draft.content) else {
        return Ok(false);
    };
    let skill = parsed
        .frontmatter
        .fields
        .get("skill")
        .and_then(escurel_md::YamlValue::as_str)
        .unwrap_or_default()
        .to_owned();
    let fm = serde_json::to_value(&parsed.frontmatter.fields).unwrap_or_else(|_| json!({}));
    indexer
        .may_read_instance(caller, &skill, &fm)
        .await
        .map_err(|e| JsonRpcError::internal(format!("draft acl: {e}")))
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

    // **An empty base means "no page here yet". Check that it is true.**
    //
    // `base_sha256: ""` is the approve-CREATE sentinel: promotion passes it
    // to `update_page`, which lands only if the target still does not exist.
    // Against a page that DOES exist it can never promote — it is a draft
    // born un-approvable, and the only place that shows up is a human tapping
    // Approve and watching nothing happen.
    //
    // Measured in the lab on 2026-09-09: seven runs, seven drafts, every one
    // with an empty base against a page written days earlier. Every approve
    // refused `conflict`, correctly, and the review feed just sat there. The
    // agent had taken the escape hatch in its instructions ("an empty string
    // when no page exists yet") without reading the target first.
    //
    // So refuse it HERE, while the agent is still running and can fix it: the
    // message names `expand` and the field to carry. This is the same rule as
    // the validation below, one field earlier — a draft that cannot be
    // promoted is worse than a refused write, because it costs a human a
    // review before anyone finds out.
    if a.base_sha256.as_deref() == Some("")
        && indexer
            .expand(&a.target_page_id, None, None)
            .await
            .map_err(|e| JsonRpcError::internal(format!("create_draft head: {e}")))?
            .is_some()
    {
        return Ok(json!({
            "ok": false,
            "issues": [{
                "severity": "error",
                "code": "conflict",
                "location": "base_sha256",
                "message": format!(
                    "`{}` already exists, so an empty `base_sha256` (the \
                     create sentinel) can never promote. Call `expand` on it \
                     and pass its `content_sha256` as `base_sha256` — that is \
                     what makes the approval refuse if the page moves under \
                     your draft, and it is the base a reviewer approves \
                     against.",
                    a.target_page_id
                ),
            }],
        }));
    }

    // Validate at DRAFT time, with the same blocking set promotion will
    // apply. A draft that cannot be promoted is worse than a refused write:
    // it costs a human a review before anyone finds out.
    let issues = indexer
        .validate(Some(&a.target_page_id), &a.content)
        .await
        .map_err(|e| JsonRpcError::internal(format!("create_draft validate: {e}")))?;
    let blocking = draft_blocking_issues(state, &issues);
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
    caller: AclCaller<'_>,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: ListDraftsArgs = parse_args(args, "list_drafts")?;
    let drafts = indexer
        .list_drafts(a.limit)
        .await
        .map_err(|e| JsonRpcError::internal(format!("list_drafts: {e}")))?;
    let mut visible = Vec::new();
    for d in &drafts {
        if may_see(indexer, &caller, d).await? {
            visible.push(draft_to_json(d));
        }
    }
    Ok(json!({ "drafts": visible }))
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
    if !may_see(indexer, &caller, &draft).await? {
        // Denial reads as absence, as it does for every other scoped read
        // here: "there is a draft you may not see" is itself information.
        return Ok(json!({
            "ok": false,
            "issues": [{
                "severity": "error",
                "code": "not_found",
                "location": "draft_id",
                "message": format!("no draft `{}`", a.draft_id),
            }],
        }));
    }
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
    let subject = decided_by_or_caller(&a, &caller)?;
    let target_page_id = draft.target_page_id.clone();
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

        // **The event is absorbed the moment the write lands.**
        //
        // A draft made under `autonomy: review` deliberately leaves its event
        // in the inbox: the run produced no state, so the event is still
        // waiting on a human. Promotion IS that human, and until now nothing
        // said so — the event stayed unassigned for ever, and every restart
        // of a runner with an ephemeral ledger re-dispatched it and drafted
        // the same page again. Measured in the lab on 2026-09-10: seven
        // approved emails came back as seven fresh drafts, several
        // byte-identical to the page that had just landed, and a review queue
        // that refills itself is one nobody can trust to be finished.
        //
        // Best effort, and deliberately so. The write has landed; the draft
        // is closed; a failure to retire the event must not turn a successful
        // promotion into an error the reviewer sees. It is logged, and the
        // worst case is the state this replaces.
        if let Some(event_id) = draft.event_id.as_deref().filter(|e| !e.is_empty()) {
            let assigned = crate::mcp::tools_write::tool_assign_event(
                indexer,
                caller,
                state.event_acl,
                json!({
                    "event_id": event_id,
                    "instance_page_id": &target_page_id,
                }),
            )
            .await;
            match assigned {
                Ok(out) if out.get("ok").and_then(Value::as_bool) != Some(false) => {}
                other => tracing::warn!(
                    draft_id = %draft.draft_id,
                    event_id = %event_id,
                    page_id = %target_page_id,
                    outcome = ?other,
                    "promoted a draft but could not retire its event; it will \
                     be dispatched again"
                ),
            }
        }
    }
    // Name the decider back to the caller. A gateway that vouched for a human
    // can log what the store recorded rather than what it hoped the store
    // recorded — and a test can assert it without a second read path.
    let mut result = result;
    if let Some(obj) = result.as_object_mut() {
        obj.insert("decided_by".to_owned(), json!(subject));
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
    // Refusing someone else's held write is a decision on their work, so it
    // takes the same visibility check promotion does. Without it the cheapest
    // attack on this surface is to discard every draft in the tenant.
    if let Some(draft) = indexer
        .get_draft(&a.draft_id)
        .await
        .map_err(|e| JsonRpcError::internal(format!("discard_draft: {e}")))?
        && !may_see(indexer, &caller, &draft).await?
    {
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
    let closed = indexer
        .close_draft(
            &a.draft_id,
            "discarded",
            &decided_by_or_caller(&a, &caller)?,
            &a.reason,
        )
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

/// The blocking set for a DRAFT, which is the shared one plus every key the
/// skill itself declares required.
///
/// `update_page` deliberately blocks only `id` and `skill` among the
/// `required_frontmatter` keys, because a bulk seed or an older corpus may
/// legitimately lack the rest and breaking those writes is a migration, not a
/// fix. The draft path carries none of that history — it shipped in
/// 2026-09 — so it can hold the stricter line, and it is the path where the
/// looser one does real damage.
///
/// Measured on 2026-09-06, end to end with a real model: a Gemini run drafted
/// a page with no `engagement:`, `create_draft` answered `ok`, and the draft
/// was then invisible to every consultant — Heron scopes the review queue by
/// exactly that field and fails closed on its absence. The run had turns left
/// and could have acted on a refusal; instead it succeeded into a black hole,
/// and the phone said "Nothing waiting. Everything captured has been filed or
/// decided."
///
/// A draft is content proposed for a HUMAN to approve. One that cannot be
/// attributed cannot be shown to the person who would approve it, and in a
/// shared tenant an unattributable record about a customer is precisely what
/// the attribution rule exists to prevent. Refusing it while an agent is
/// still running is the recoverable failure.
fn draft_blocking_issues<'a>(
    state: &crate::server::AppState,
    issues: &'a [escurel_index::Issue],
) -> Vec<&'a escurel_index::Issue> {
    let mut blocking = crate::mcp::tools_write::blocking_issues(state, issues);
    for i in issues {
        if i.severity == escurel_index::Severity::Error
            && i.code == "frontmatter_required_key_missing"
            && !blocking.iter().any(|b| std::ptr::eq(*b, i))
        {
            blocking.push(i);
        }
    }
    blocking
}
