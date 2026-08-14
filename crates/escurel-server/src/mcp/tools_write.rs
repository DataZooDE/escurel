//! Agent-facing write tools: page updates, moves, deletes, chat messages and event capture.
//!
//! Split out of `mcp.rs` (R1 of
//! `docs/notes/complexity-reduction-plan.md`). Everything here takes the per-tenant write lock or emits an event, which
//! is the property that separates it from the read group.

use super::*;

#[derive(Deserialize)]
pub(super) struct UpdatePageArgs {
    page_id: String,
    content: String,
    /// Optimistic-concurrency guard (#246): the version the client last read.
    /// When the head has advanced past it, the write conflicts.
    ///
    /// The value to send is a page's **`version`**, as published by `expand`
    /// (and, immediately after a write, by this tool's own `new_version` —
    /// they are the same monotonic `v<hlc>` from the single CRDT authority).
    /// `open_session`'s `head_version` is NOT it: that is a live session's head
    /// at open time, and a session does not observe whole-page writes.
    #[serde(default)]
    base_version: Option<String>,
    /// Ask for a **strict** compare-and-swap: on a stale `base_version`,
    /// conflict outright instead of attempting the three-way auto-merge.
    ///
    /// Default `false` — the auto-merge is the right default for a
    /// read-modify-write agent, which wants its edit to land alongside a
    /// concurrent one.
    ///
    /// It is exactly wrong for a **human-in-the-loop approval**, and that is
    /// what this flag is for. A reviewer approved one specific diff against
    /// one specific base; a merge persists a *third* document that neither the
    /// reviewer nor the concurrent author ever saw. `auto_merged: true` tells
    /// the caller after the fact, but the merge has already been written — so
    /// a caller who treats it as a refusal is wrong about the store's state.
    /// With this set, nothing is merged and nothing is persisted: the caller
    /// gets `code: conflict` plus `head_content` and re-drafts.
    #[serde(default)]
    require_exact_base: bool,
    /// Optional provenance passthrough (#246): a runner-orchestrated write
    /// carries its `provenance.workflow`/`runner` block. Its presence suppresses
    /// the opt-in `page-edited` event (the cascade already handles those writes),
    /// so only genuine out-of-band edits trigger the eager improvement pass.
    #[serde(default)]
    provenance: Option<Value>,
}

/// What the compare-and-swap decided, named.
///
/// The staleness check used to be sixty lines of nested `if let` inline in
/// `tool_update_page`, which is why it took three readings to answer "what
/// does the gate actually decide". There are exactly three answers.
pub(super) enum Cas {
    /// The caller's base matched head, or sent none: persist the draft.
    Clean,
    /// The base was stale but the edits merged cleanly; persist the merge.
    Merged(String),
    /// Stale and unmergeable. Carries the ready-to-return issue, including
    /// `head_content` so the caller can re-draft against it.
    Conflict(Value),
}

/// The typed refusal for "you asked for a concurrency guard this deployment
/// cannot provide".
///
/// A `base_version` on a gateway with no CRDT backend used to be **silently
/// ignored** — the write went through, unguarded, reporting `ok: true`. That
/// is the worst possible answer to a request for a safety check: the caller
/// believes it is protected and is not, so a stale approval clobbers a
/// concurrent edit and nothing anywhere says so. (Observed downstream in
/// Heron, which shipped `base_version` on every approval and had it discarded
/// by the server for months.)
///
/// Refusing is the fail-closed reading, and it costs no correct caller
/// anything: a caller that does not want the guard simply omits the field, and
/// that path is untouched.
fn versioning_unavailable(location: &str) -> Value {
    json!({
        "ok": false,
        "issues": [{
            "severity": "error",
            "code": "versioning_unavailable",
            "location": location,
            "message": "this gateway has no CRDT backend, so page versions are not \
                        tracked and an optimistic-concurrency guard cannot be honoured; \
                        the write was NOT applied — omit the guard to write unguarded, \
                        or point the caller at a gateway with live versioning",
        }],
    })
}

/// Decide the compare-and-swap for one `update_page`.
///
/// MUST be called with `state.update_page_gate` held: the staleness decision
/// and the write that follows it are one atomic step, and separating them is
/// precisely the race the gate exists to prevent.
pub(super) async fn resolve_base_version(
    state: &crate::server::AppState,
    page_id: &str,
    base_version: Option<&str>,
    require_exact: bool,
    draft: &str,
    head_hlc: u64,
) -> Cas {
    let Some(base) = base_version else {
        return Cas::Clean;
    };
    let Some(backend) = state.crdt_backend.as_ref() else {
        // Asked to guard, unable to guard. See `versioning_unavailable`.
        return Cas::Conflict(versioning_unavailable("base_version"));
    };
    let head = Version::from_op_count(head_hlc);
    if base == head.as_str() {
        return Cas::Clean;
    }
    // Strict callers never reach the merge: for them a stale base is the
    // answer, and a merged document is not an outcome they can accept.
    let merged = if require_exact {
        None
    } else {
        try_auto_merge(backend, page_id, base, draft).await
    };
    match merged {
        Some(merged) => Cas::Merged(merged),
        None => {
            let head_content = hydrate_content(backend, page_id).await.ok().flatten();
            Cas::Conflict(json!({
                "ok": false,
                "issues": [{
                    "severity": "error",
                    "code": "conflict",
                    "location": "base_version",
                    "message": format!(
                        "base_version {base} is stale (head is {}) and the edits could \
                         not be auto-merged; re-draft against head_content",
                        head.as_str()
                    ),
                }],
                "head_content": head_content,
            }))
        }
    }
}

pub(super) async fn tool_update_page(
    state: &crate::server::AppState,
    indexer: &Indexer,
    caller: AclCaller<'_>,
    write_acl: crate::server::WriteAclMode,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: UpdatePageArgs = parse_args(args, "update_page")?;

    // `require_exact_base` without a base is a caller bug, and the harmless
    // reading of it — ignore the flag — is the one that loses data: the caller
    // asked for a strict CAS and would get an unguarded write. Rejected as
    // invalid params so a retry cannot help and the mistake is loud.
    if a.require_exact_base && a.base_version.is_none() {
        return Err(JsonRpcError::invalid_params(
            "update_page: `require_exact_base` needs a `base_version` to be exact about".to_owned(),
        ));
    }

    // Read-only-backend guard (REQ-BK-03): reject an attempt to write backend
    // data for a non-writable backend (creating a sql_view/document instance
    // via update_page, or stripping its backend_ref) with a typed
    // `backend_read_only` Issue. Overlay co-authoring stays allowed.
    if let Some(reason) = indexer
        .backend_read_only_rejection(&a.page_id, &a.content)
        .await
        .map_err(|e| JsonRpcError::internal(format!("update_page backend guard: {e}")))?
    {
        return Ok(json!({
            "ok": false,
            "issues": [{
                "severity": "error",
                "code": "backend_read_only",
                "location": "frontmatter.backend_ref",
                "message": reason,
            }],
        }));
    }

    // Base-layer guard (REQ-LAYER-02): a page imported from a subscribed
    // pack (`layer: base@<pack>@<version>`) is read-only at this node, and
    // `update_page` may not fabricate one. Same dispatch seam as the
    // backend guard above, lifted from per-backend-kind to per-page-layer.
    if let Some(reason) = indexer
        .layer_read_only_rejection(&a.page_id, &a.content)
        .await
        .map_err(|e| JsonRpcError::internal(format!("update_page layer guard: {e}")))?
    {
        return Ok(json!({
            "ok": false,
            "issues": [{
                "severity": "error",
                "code": "layer_read_only",
                "location": "frontmatter.layer",
                "message": reason,
            }],
        }));
    }

    // Curator marker guard (REQ-PROMO-01 / AT-PROMO-2): `promotable:
    // true` gates what may leave this node through the promotion
    // harvest, so only a curator (admin in the v1 two-role model) may
    // write it. Fail-closed: any non-admin draft carrying a truthy
    // `promotable` refuses — an agent can neither self-promote a page
    // nor keep the marker alive by re-writing a curated page.
    if !caller.is_admin
        && escurel_md::parse(&a.content).is_ok_and(|p| {
            p.frontmatter
                .fields
                .get("promotable")
                .and_then(escurel_md::YamlValue::as_bool)
                == Some(true)
        })
    {
        return Ok(json!({
            "ok": false,
            "issues": [{
                "severity": "error",
                "code": "promotable_requires_curator",
                "location": "frontmatter.promotable",
                "message": "`promotable: true` marks content eligible to leave this \
                            node through the promotion harvest; only a curator \
                            (admin role) may set or keep it",
            }],
        }));
    }

    // Shadow-creation gate (REQ-LAYER-03 / agy review): an overlay skill
    // page with a base skill's id changes which DEFINITION governs that
    // id's instances — backend binding, ACL, required_frontmatter. That
    // is curator work; an unprivileged agent authoring one would hijack
    // pack governance. Keyed on the draft's own skill id vs the indexed
    // base pages, so it also holds for the auto-merge path (the id is
    // identical on every side of a merge).
    if !caller.is_admin
        && let Ok(parsed) = escurel_md::parse(&a.content)
        && parsed.frontmatter.page_type == PageType::Skill
    {
        let skill_id = parsed
            .frontmatter
            .fields
            .get("id")
            .and_then(escurel_md::YamlValue::as_str)
            .unwrap_or_default()
            .to_owned();
        if !skill_id.is_empty()
            && indexer
                .skill_page_conflict(&skill_id, &a.page_id)
                .await
                .map_err(|e| JsonRpcError::internal(format!("update_page shadow gate: {e}")))?
                .is_some()
        {
            return Ok(json!({
                "ok": false,
                "issues": [{
                    "severity": "error",
                    "code": "shadow_requires_curator",
                    "location": "frontmatter.id",
                    "message": format!(
                        "skill `{skill_id}` is provided by a subscribed pack; authoring \
                         an overlay that shadows it changes which definition governs \
                         its instances — only a curator (admin role) may do that"
                    ),
                }],
            }));
        }
    }

    // Deterministic per-instance WRITE ACL (symmetric to the read ACL):
    // only the resolved owner (or admin) may mutate an owner-private
    // instance; public/no-owner instances are admin-write-only. `Off`
    // skips; `Log` records a would-be denial but allows; `Enforce` rejects.
    if write_acl != crate::server::WriteAclMode::Off {
        let allowed = indexer
            .may_write_page(&caller, &a.page_id, &a.content)
            .await
            .map_err(|e| JsonRpcError::internal(format!("update_page acl: {e}")))?;
        if !allowed {
            if write_acl == crate::server::WriteAclMode::Log {
                tracing::warn!(
                    subject = %caller.subject,
                    page_id = %a.page_id,
                    "write-ACL would deny this write (log mode) — allowing"
                );
            } else {
                return Ok(json!({
                    "ok": false,
                    "issues": [{
                        "severity": "error",
                        "code": "forbidden",
                        "location": "frontmatter",
                        "message": format!(
                            "write denied: caller `{}` does not own instance `{}`",
                            caller.subject, a.page_id
                        ),
                    }],
                }));
            }
        }
    }

    // #246 optimistic concurrency + monotonic versions + CRDT auto-merge. The
    // page version is `v<max_hlc>` from the CRDT backend (shared with the
    // apply_op session space). When a client sends a stale `base_version` the
    // head has advanced concurrently; rather than clobber or immediately
    // reject, attempt a Loro three-way auto-merge of (base → head) and
    // (base → incoming). A clean merge is persisted (`auto_merged: true`); an
    // unmergeable one falls back to `{ok:false, code:conflict, head_content}`
    // for the client to re-draft. Enforced only when a CRDT backend is wired;
    // otherwise behaviour is unchanged.
    //
    // The gate makes check-then-write atomic: the staleness decision, the
    // indexed write, and the `new_version` assignment must not interleave
    // with another `update_page`, or N simultaneous writes carrying the
    // same stale base all pass validation and silently last-write-win
    // (observed downstream: 20 racing read-modify-writes converged to one
    // survivor). Held to the end of the version bump below; every early
    // return releases it on drop.
    let _cas_gate = state.update_page_gate.lock().await;
    let head_hlc = match state.crdt_backend.as_ref() {
        Some(b) => u64::try_from(b.max_hlc(&a.page_id).await.unwrap_or(0)).unwrap_or(0),
        None => 0,
    };
    // The content we ultimately persist — an auto-merge may replace the raw
    // incoming draft with the merged result.
    let (content_to_write, auto_merged) = match resolve_base_version(
        state,
        &a.page_id,
        a.base_version.as_deref(),
        a.require_exact_base,
        &a.content,
        head_hlc,
    )
    .await
    {
        Cas::Conflict(issue) => return Ok(issue),
        Cas::Clean => (a.content.clone(), false),
        Cas::Merged(merged) => (merged, true),
    };

    // Re-run the write guards on the MERGED artifact (agy review): the
    // draft-side checks above saw `a.content`, but a clean auto-merge
    // persists `content_to_write`, whose frontmatter can carry keys the
    // head gained since the caller's base — including a laundered
    // `layer: base@…` or `promotable: true`. Whatever produced the
    // final content, what persists must pass the same gates.
    if auto_merged {
        if let Some(reason) = indexer
            .layer_read_only_rejection(&a.page_id, &content_to_write)
            .await
            .map_err(|e| JsonRpcError::internal(format!("update_page merged layer guard: {e}")))?
        {
            return Ok(json!({
                "ok": false,
                "issues": [{
                    "severity": "error",
                    "code": "layer_read_only",
                    "location": "frontmatter.layer",
                    "message": reason,
                }],
            }));
        }
        if !caller.is_admin
            && escurel_md::parse(&content_to_write).is_ok_and(|p| {
                p.frontmatter
                    .fields
                    .get("promotable")
                    .and_then(escurel_md::YamlValue::as_bool)
                    == Some(true)
            })
        {
            return Ok(json!({
                "ok": false,
                "issues": [{
                    "severity": "error",
                    "code": "promotable_requires_curator",
                    "location": "frontmatter.promotable",
                    "message": "the auto-merged result would persist `promotable: true`; \
                                only a curator (admin role) may write the marker — \
                                re-draft against the current head",
                }],
            }));
        }
    }

    // Refuse a write the dry run would have rejected. `update_page` did not
    // validate at all, so `page validate` and `page update` disagreed — and
    // agents call `update`. That is how an instance with no `id:` and links
    // to non-existent customers got into a real tenant.
    //
    // Error severity only: warnings (a forward reference to a page not
    // written yet) stay writable, which is what makes seeding and multi-part
    // `continues:` chains work.
    let issues = indexer
        .validate(Some(&a.page_id), &content_to_write)
        .await
        .map_err(|e| JsonRpcError::internal(format!("update_page validate: {e}")))?;
    // Enforce LINK INTEGRITY and PAGE IDENTITY only — the checks that decide
    // whether a page is reachable and whether its graph edges are real.
    //
    // Deliberately NOT `required_frontmatter` completeness. Enforcing that
    // here immediately broke escurel's own `distill` workflow: the echo
    // harness writes a `distill-claim` with no `target_page`, which its skill
    // requires. That violation is real and predates this change — it was
    // invisible because `update_page` never validated at all — but fixing it
    // is a separate migration with its own blast radius, and it must not
    // gate closing the link-integrity hole. `page validate` still reports it,
    // as it always has.
    let blocking: Vec<_> = issues
        .iter()
        .filter(|i| i.severity == Severity::Error)
        .filter(|i| match i.code.as_str() {
            // A link that names a type or a page that does not exist. This is
            // the hole being closed: an agent could cite
            // `[[customer::invented-gmbh]]` and the graph would carry it.
            "dangling_wikilink" => true,
            // ...but only for a WIKILINK. The same code also fires at
            // `frontmatter.skill` when a page declares a skill that is not
            // seeded yet, which is ordering-sensitive: a bulk seed may write
            // instances before their skill page, and escurel's own snapshot
            // tests do exactly that. Pre-existing, not this change's business.
            "unknown_skill" => i.location.starts_with("wikilink"),
            // A page with no `id` indexes but can neither be expanded nor
            // resolved — an identity failure, not a completeness one.
            "frontmatter_required_key_missing" => i.location == "frontmatter.id",
            // A skill page declaring an unrecognised `autonomy:` policy
            // (heron#5 / CR-1). GATED, unlike every other arm above, because
            // `autonomy:` has been unvalidated free-form frontmatter: a tenant
            // whose page already carries junk there would find the page
            // unwritable — for any edit, not just an edit to that field —
            // the moment the server upgraded. Off (default) → Log → Enforce
            // lets an operator find those pages first, the same rollout the
            // write ACL gets. `validate` reports it in every mode.
            "frontmatter_autonomy_unknown" => {
                state.autonomy_lint == crate::server::AutonomyLintMode::Enforce
            }
            _ => false,
        })
        .collect();

    // Log mode: tell the operator, allow the write, and leave the response
    // identical to an unlinted server's — the point of the middle rung is to
    // measure the blast radius without any client observing a change.
    if state.autonomy_lint == crate::server::AutonomyLintMode::Log
        && let Some(i) = issues
            .iter()
            .find(|i| i.code == "frontmatter_autonomy_unknown")
    {
        tracing::warn!(
            page_id = %a.page_id,
            message = %i.message,
            "autonomy lint (log mode): unrecognised `autonomy:` value written",
        );
    }

    if !blocking.is_empty() {
        // Log it: a refused write is otherwise invisible to the operator —
        // the tool call succeeds and only the `ok:false` payload carries the
        // reason, which an agent may swallow.
        tracing::warn!(
            page_id = %a.page_id,
            issues = %blocking
                .iter()
                .map(|i| format!("{}@{}: {}", i.code, i.location, i.message))
                .collect::<Vec<_>>()
                .join("; "),
            "update_page rejected by validation"
        );
        return Ok(json!({
            "ok": false,
            "issues": issues.iter().map(issue_to_json).collect::<Vec<_>>(),
        }));
    }
    // The write, attributed. `stamped_principal` reads ONLY the verified
    // token subject — nothing from `a`, so no argument, frontmatter key or
    // `provenance` block the caller sent can reach the column (#357).
    match indexer
        .update_page_as(
            &a.page_id,
            &content_to_write,
            stamped_principal(caller.subject),
        )
        .await
    {
        Ok(()) => {
            // Advance the monotonic version: snapshot the new whole-page content
            // at the next hlc so `max_hlc` (and any later `base_version` read)
            // reflects this write. The apply_op session path reads the same
            // space, so co-authoring and whole-page writes share versions.
            let new_version = if let Some(backend) = state.crdt_backend.as_ref() {
                let bytes = snapshot_bytes_from_markdown(&content_to_write)
                    .map_err(|e| JsonRpcError::internal(format!("update_page crdt: {e}")))?;
                // Allocate through the backend rather than stamping
                // `head_hlc + 1` read earlier under the gate: a live session
                // on this page allocates from the same space and does not
                // take that gate, so `head_hlc + 1` could already be spoken
                // for. The backend is the single authority (F1.3).
                let next = backend
                    .snapshot_next(&a.page_id, &Snapshot::new(bytes))
                    .await
                    .map(|h| u64::try_from(h).unwrap_or(head_hlc + 1))
                    .unwrap_or(head_hlc + 1);
                Version::from_op_count(next).as_str().to_owned()
            } else {
                "v1".to_owned()
            };

            // WI-6 absorption instrumentation: count the CONFIRMED write by
            // origin — the same runner/workflow-provenance discriminator the
            // page-edited suppression below uses. The runner/human ratio over
            // time is the interlocked-loops convergence curve.
            let origin = if a
                .provenance
                .as_ref()
                .is_some_and(|p| p.get("workflow").is_some() || p.get("runner").is_some())
            {
                "runner"
            } else {
                "human"
            };
            state.metrics.inc_write(indexer.tenant(), origin);

            // #246 eager per-edit improvement: an OUT-OF-BAND edit (no runner
            // provenance) optionally emits a `page-edited` inbox event so the
            // reactive loop re-lints/re-verifies the touched page. Runner-
            // orchestrated writes carry provenance → suppressed (the cascade
            // already handles them), so there is no loop storm.
            maybe_emit_page_edited(
                state.emit_edit_events,
                indexer,
                &state.events_tx,
                &a.page_id,
                a.provenance.as_ref(),
            )
            .await;

            // Announce the confirmed write as an inbox event, so a consumer
            // can be woken by a write instead of polling for it.
            emit_page_event(indexer, state.webhook.as_ref(), &a, &new_version);
            Ok(json!({
                "ok": true,
                "issues": [],
                "new_version": new_version,
                "auto_merged": auto_merged,
            }))
        }
        // The protected meta-skill rejects the write as an
        // error-severity issue rather than an opaque server error.
        Err(IndexerError::MetaSkillProtected { reason }) => Ok(json!({
            "ok": false,
            "issues": [{
                "severity": "error",
                "code": "meta_skill_protected",
                "location": "frontmatter",
                "message": reason,
            }],
        })),
        Err(e) => Err(JsonRpcError::internal(format!("update_page: {e}"))),
    }
}

#[derive(Deserialize)]
pub(super) struct DeletePageArgs {
    page_id: String,
    /// Optimistic-concurrency guard (#300, symmetric with `update_page`): the
    /// version the client last read. A stale value conflicts.
    #[serde(default)]
    base_version: Option<String>,
}

#[derive(serde::Deserialize)]
pub(super) struct MovePageArgs {
    from: String,
    to: String,
}

/// `move_page`: rename a page id, leaving nothing behind.
///
/// Distinct from `delete_page` on purpose. A delete is a *retraction* and
/// keeps the markdown as the audit record; a move is a *rename* and must not,
/// or every restructure litters the canonical store with husks. Restructuring
/// one tenant's 59 instance ids with update+delete left 59 of them.
pub(super) async fn tool_move_page(
    state: &crate::server::AppState,
    indexer: &Indexer,
    caller: AclCaller<'_>,
    write_acl: crate::server::WriteAclMode,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: MovePageArgs = parse_args(args, "move_page")?;

    let Some(existing) = indexer
        .read_page_markdown(&a.from)
        .await
        .map_err(|e| JsonRpcError::internal(format!("move_page read: {e}")))?
    else {
        return Ok(json!({
            "ok": false,
            "issues": [{
                "severity": "error",
                "code": "not_found",
                "location": "from",
                "message": format!("no page `{}` to move", a.from),
            }],
        }));
    };

    // Same gates as delete_page, evaluated against the stored page: a move is
    // a write to both ids, so a backend- or layer-managed page is off limits.
    if let Some(reason) = indexer
        .backend_read_only_rejection(&a.from, &existing)
        .await
        .map_err(|e| JsonRpcError::internal(format!("move_page backend guard: {e}")))?
    {
        return Ok(json!({
            "ok": false,
            "issues": [{
                "severity": "error",
                "code": "backend_read_only",
                "location": "frontmatter.backend_ref",
                "message": reason,
            }],
        }));
    }
    if let Some(reason) = indexer
        .layer_read_only_rejection(&a.from, &existing)
        .await
        .map_err(|e| JsonRpcError::internal(format!("move_page layer guard: {e}")))?
    {
        return Ok(json!({
            "ok": false,
            "issues": [{
                "severity": "error",
                "code": "layer_read_only",
                "location": "frontmatter",
                "message": reason,
            }],
        }));
    }
    if write_acl != crate::server::WriteAclMode::Off {
        let allowed = indexer
            .may_write_page(&caller, &a.from, &existing)
            .await
            .map_err(|e| JsonRpcError::internal(format!("move_page acl: {e}")))?;
        if !allowed {
            if write_acl == crate::server::WriteAclMode::Log {
                tracing::warn!(
                    subject = %caller.subject,
                    page_id = %a.from,
                    "write-ACL would deny this move (log mode) — allowing"
                );
            } else {
                return Ok(json!({
                    "ok": false,
                    "issues": [{
                        "severity": "error",
                        "code": "forbidden",
                        "location": "frontmatter",
                        "message": format!(
                            "move denied: caller `{}` does not own instance `{}`",
                            caller.subject, a.from
                        ),
                    }],
                }));
            }
        }
    }

    match indexer.move_page(&a.from, &a.to).await {
        Ok(true) => {
            state.metrics.inc_write(indexer.tenant(), "human");
            Ok(json!({ "ok": true, "issues": [], "from": a.from, "to": a.to }))
        }
        Ok(false) => Ok(json!({
            "ok": false,
            "issues": [{
                "severity": "error",
                "code": "not_found",
                "location": "from",
                "message": format!("no page `{}` to move", a.from),
            }],
        })),
        Err(IndexerError::PageExists { page_id }) => Ok(json!({
            "ok": false,
            "issues": [{
                "severity": "error",
                "code": "conflict",
                "location": "to",
                "message": format!("a live page already exists at `{page_id}`"),
            }],
        })),
        Err(IndexerError::MetaSkillProtected { reason }) => Ok(json!({
            "ok": false,
            "issues": [{
                "severity": "error",
                "code": "meta_skill_protected",
                "location": "frontmatter",
                "message": reason,
            }],
        })),
        Err(e) => Err(JsonRpcError::internal(format!("move_page: {e}"))),
    }
}

/// #300 `delete_page`: soft-delete (archive) a markdown page/instance. Mirrors
/// `update_page`'s gates — backend/layer read-only, write-ACL, meta-skill — but
/// evaluates them against the STORED page, since a delete carries no new draft.
/// The page is retracted from discovery (index rows + link edges dropped) while
/// its canonical markdown is retained, re-stamped `archived: true`, for audit.
pub(super) async fn tool_delete_page(
    state: &crate::server::AppState,
    indexer: &Indexer,
    caller: AclCaller<'_>,
    write_acl: crate::server::WriteAclMode,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: DeletePageArgs = parse_args(args, "delete_page")?;

    // Fetch the stored markdown; a missing page is a typed `not_found`, not a
    // 500. Idempotent: a second delete (page already retracted) also
    // reports `not_found`.
    let Some(existing) = indexer
        .read_page_markdown(&a.page_id)
        .await
        .map_err(|e| JsonRpcError::internal(format!("delete_page read: {e}")))?
    else {
        return Ok(json!({
            "ok": false,
            "issues": [{
                "severity": "error",
                "code": "not_found",
                "location": "page_id",
                "message": format!("no page `{}` to delete", a.page_id),
            }],
        }));
    };

    // Read-only-backend guard: a sql_view/document instance is managed by its
    // backend, not the markdown write surface — deleting the overlay here
    // would desync it. Evaluated against the stored content.
    if let Some(reason) = indexer
        .backend_read_only_rejection(&a.page_id, &existing)
        .await
        .map_err(|e| JsonRpcError::internal(format!("delete_page backend guard: {e}")))?
    {
        return Ok(json!({
            "ok": false,
            "issues": [{
                "severity": "error",
                "code": "backend_read_only",
                "location": "frontmatter.backend_ref",
                "message": reason,
            }],
        }));
    }

    // Base-layer guard: a page imported from a subscribed pack is read-only at
    // this node — it cannot be deleted here (unsubscribe the pack instead).
    if let Some(reason) = indexer
        .layer_read_only_rejection(&a.page_id, &existing)
        .await
        .map_err(|e| JsonRpcError::internal(format!("delete_page layer guard: {e}")))?
    {
        return Ok(json!({
            "ok": false,
            "issues": [{
                "severity": "error",
                "code": "layer_read_only",
                "location": "frontmatter.layer",
                "message": reason,
            }],
        }));
    }

    // Write ACL: a delete is an overwrite of the existing page, so the caller
    // must own it (or be admin). Passing the stored content as the write
    // content yields the own-the-existing-page (Verb::Update) decision.
    if write_acl != crate::server::WriteAclMode::Off {
        let allowed = indexer
            .may_write_page(&caller, &a.page_id, &existing)
            .await
            .map_err(|e| JsonRpcError::internal(format!("delete_page acl: {e}")))?;
        if !allowed {
            if write_acl == crate::server::WriteAclMode::Log {
                tracing::warn!(
                    subject = %caller.subject,
                    page_id = %a.page_id,
                    "write-ACL would deny this delete (log mode) — allowing"
                );
            } else {
                return Ok(json!({
                    "ok": false,
                    "issues": [{
                        "severity": "error",
                        "code": "forbidden",
                        "location": "frontmatter",
                        "message": format!(
                            "delete denied: caller `{}` does not own instance `{}`",
                            caller.subject, a.page_id
                        ),
                    }],
                }));
            }
        }
    }

    // Optimistic concurrency (#300, symmetric with update_page): a stale
    // `base_version` means the page changed since the caller read it — refuse
    // rather than retract a page they have not seen. Held under the same CAS
    // gate so the check-then-delete cannot interleave with an update_page.
    let _cas_gate = state.update_page_gate.lock().await;
    if a.base_version.is_some() && state.crdt_backend.is_none() {
        // Symmetric with update_page: a guard this gateway cannot honour is
        // refused, never silently dropped. See `versioning_unavailable`.
        return Ok(versioning_unavailable("base_version"));
    }
    if let (Some(backend), Some(base)) = (state.crdt_backend.as_ref(), a.base_version.as_deref()) {
        let head_hlc = u64::try_from(backend.max_hlc(&a.page_id).await.unwrap_or(0)).unwrap_or(0);
        let head = Version::from_op_count(head_hlc);
        if base != head.as_str() {
            return Ok(json!({
                "ok": false,
                "issues": [{
                    "severity": "error",
                    "code": "conflict",
                    "location": "base_version",
                    "message": format!(
                        "base_version {base} is stale (head is {}); re-read before deleting",
                        head.as_str()
                    ),
                }],
            }));
        }
    }

    match indexer.delete_page(&a.page_id).await {
        Ok(true) => {
            state.metrics.inc_write(indexer.tenant(), "human");
            Ok(json!({ "ok": true, "issues": [], "page_id": a.page_id }))
        }
        // The page vanished between the read above and here (racing delete).
        Ok(false) => Ok(json!({
            "ok": false,
            "issues": [{
                "severity": "error",
                "code": "not_found",
                "location": "page_id",
                "message": format!("no page `{}` to delete", a.page_id),
            }],
        })),
        Err(IndexerError::MetaSkillProtected { reason }) => Ok(json!({
            "ok": false,
            "issues": [{
                "severity": "error",
                "code": "meta_skill_protected",
                "location": "frontmatter",
                "message": reason,
            }],
        })),
        Err(e) => Err(JsonRpcError::internal(format!("delete_page: {e}"))),
    }
}

/// #246 CRDT three-way auto-merge for a stale `base_version`. Returns the
/// validated merged markdown to persist, or `None` when the write should fall
/// back to a plain conflict.
///
/// `None` (→ conflict) is returned when:
/// * `base_version` isn't a `"v<n>"` we can locate, or
/// * no snapshot was stored at that hlc (the base history is gone — e.g. it was
///   a bare session op-count, not an `update_page` snapshot), or
/// * the current head content can't be hydrated, or
/// * the Loro merge itself errors, or
/// * the merged page no longer parses, or its frontmatter matches *neither*
///   side (an interleaved / corrupted frontmatter — never persisted).
///
/// Body interleaving from overlapping same-region edits is *accepted* — that is
/// the CRDT merge semantics; the frontmatter guard only protects page identity.
pub(super) async fn try_auto_merge(
    backend: &std::sync::Arc<dyn CrdtBackend>,
    page_id: &str,
    base_version: &str,
    incoming: &str,
) -> Option<String> {
    // Map base_version "vN" -> the snapshot at hlc N that the client branched
    // from. Every update_page write snapshots at its version's hlc, so this
    // reconstructs the exact base for the three-way merge.
    let base_hlc = i64::try_from(Version::parse_op_count(base_version)?).ok()?;
    let base_snapshot = backend
        .snapshot_at(page_id, base_hlc)
        .await
        .ok()
        .flatten()?;
    let head_content = hydrate_content(backend, page_id).await.ok().flatten()?;

    let merged = three_way_merge(&base_snapshot, &head_content, incoming).ok()?;

    // Safety net: the merged page must still parse AND keep one side's
    // frontmatter intact. A body-only merge (the common case) leaves the
    // frontmatter equal to both sides; a one-sided frontmatter change survives
    // via the CRDT; only a genuine both-sides frontmatter divergence (or a
    // corrupt interleave) fails both equalities → conflict.
    let merged_fm = escurel_md::parse(&merged).ok()?.frontmatter.fields;
    let incoming_fm = escurel_md::parse(incoming).ok()?.frontmatter.fields;
    let head_fm = escurel_md::parse(&head_content).ok()?.frontmatter.fields;
    if merged_fm == incoming_fm || merged_fm == head_fm {
        Some(merged)
    } else {
        None
    }
}

/// #246 eager per-edit improvement. When `ESCUREL_EMIT_EDIT_EVENTS` is enabled
/// and the write carries NO runner/workflow provenance (a genuine out-of-band
/// edit), capture a `page-edited` inbox event for the touched page so the
/// runner re-lints/re-verifies it. A provenance-carrying (runner-orchestrated)
/// write is suppressed — the cascade already handles it — so the improvement
/// loop's own writes can't storm. Best-effort: a failure is logged, not fatal.
pub(super) async fn maybe_emit_page_edited(
    enabled: bool,
    indexer: &Indexer,
    events_tx: &tokio::sync::broadcast::Sender<std::sync::Arc<EventInfo>>,
    page_id: &str,
    provenance: Option<&Value>,
) {
    let runner_write =
        provenance.is_some_and(|p| p.get("workflow").is_some() || p.get("runner").is_some());
    if !enabled || runner_write {
        return;
    }
    match indexer
        .capture_event(escurel_index::events::NewEvent {
            event_id: None,
            at: None,
            source: "page-edited".to_owned(),
            mime: "text/plain".to_owned(),
            label_skill: "page-edited".to_owned(),
            instance_page_id: Some(page_id.to_owned()),
            title: format!("edited {page_id}"),
            body: format!("Page {page_id} was edited out of band; re-verify."),
            provenance: Some(json!({ "edit": { "page": page_id } })),
        })
        .await
    {
        Ok(stored) => {
            let _ = events_tx.send(std::sync::Arc::new(stored));
        }
        Err(e) => {
            tracing::warn!(page_id, error = %e, "page-edited event capture failed");
        }
    }
}

// --- chat tools (M-Chat, issue #63) -----------------------------

#[derive(Deserialize)]
pub(super) struct AppendMessageArgs {
    chat_group_id: String,
    role: String,
    content: String,
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    ts: Option<String>,
    #[serde(default)]
    metadata: Option<Value>,
    #[serde(default)]
    msg_id: Option<String>,
    #[serde(default = "default_embed")]
    embed: bool,
}

pub(super) fn default_embed() -> bool {
    true
}

pub(super) async fn tool_append_message(
    indexer: &Indexer,
    caller: AclCaller<'_>,
    write_acl: crate::server::WriteAclMode,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: AppendMessageArgs = parse_args(args, "append_message")?;

    // Chat-surface ACL: only the chat group's owner (or admin) may append.
    if write_acl != crate::server::WriteAclMode::Off {
        let allowed = indexer
            .may_access_chat(&caller, &a.chat_group_id)
            .await
            .map_err(|e| JsonRpcError::internal(format!("append_message acl: {e}")))?;
        if !allowed {
            if write_acl == crate::server::WriteAclMode::Log {
                tracing::warn!(
                    subject = %caller.subject, chat_group_id = %a.chat_group_id,
                    "chat-ACL would deny this append (log mode) — allowing"
                );
            } else {
                return Err(JsonRpcError::forbidden(format!(
                    "append denied: caller `{}` does not own chat `{}`",
                    caller.subject, a.chat_group_id
                )));
            }
        }
    }

    let stored = indexer
        .append_chat_message(AppendChatMessage {
            chat_group_id: &a.chat_group_id,
            role: &a.role,
            content: &a.content,
            author: a.author.as_deref(),
            ts: a.ts.as_deref(),
            metadata: a.metadata,
            msg_id: a.msg_id.as_deref(),
            embed: a.embed,
        })
        .await
        .map_err(|e| JsonRpcError::internal(format!("append_message: {e}")))?;
    Ok(json!({
        "msg_id": stored.msg_id,
        "ts": stored.ts,
    }))
}

#[derive(Deserialize)]
pub(super) struct ListMessagesArgs {
    chat_group_id: String,
    #[serde(default)]
    since: Option<String>,
    #[serde(default)]
    until: Option<String>,
    #[serde(default = "default_chat_limit")]
    limit: usize,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    direction: Option<String>,
}

pub(super) fn default_chat_limit() -> usize {
    100
}

pub(super) async fn tool_list_messages(
    indexer: &Indexer,
    caller: AclCaller<'_>,
    write_acl: crate::server::WriteAclMode,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: ListMessagesArgs = parse_args(args, "list_messages")?;

    // Chat-surface ACL: only the chat group's owner (or admin) may read its
    // history. A denial returns an EMPTY page (non-leaking, like expand→null),
    // never another member's transcript.
    if write_acl != crate::server::WriteAclMode::Off {
        let allowed = indexer
            .may_access_chat(&caller, &a.chat_group_id)
            .await
            .map_err(|e| JsonRpcError::internal(format!("list_messages acl: {e}")))?;
        if !allowed {
            if write_acl == crate::server::WriteAclMode::Log {
                tracing::warn!(
                    subject = %caller.subject, chat_group_id = %a.chat_group_id,
                    "chat-ACL would deny this read (log mode) — allowing"
                );
            } else {
                return Ok(json!({ "messages": [] }));
            }
        }
    }
    // Default to descending — typical "give me the most recent N"
    // call site. Consumers paging the forward log pass "asc".
    let direction = match a
        .direction
        .as_deref()
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        None | Some("") | Some("desc") => OrderDir::Desc,
        Some("asc") => OrderDir::Asc,
        Some(other) => {
            return Err(JsonRpcError::invalid_params(format!(
                "list_messages: direction `{other}`; expected asc|desc",
            )));
        }
    };
    let page = indexer
        .list_chat_messages(ListChatMessages {
            chat_group_id: &a.chat_group_id,
            since: a.since.as_deref(),
            until: a.until.as_deref(),
            limit: a.limit,
            cursor: a.cursor.as_deref(),
            direction,
        })
        .await
        .map_err(|e| JsonRpcError::internal(format!("list_messages: {e}")))?;
    let messages: Vec<Value> = page.messages.iter().map(chat_message_to_json).collect();
    let mut out = json!({ "messages": messages });
    if let Some(c) = page.next_cursor {
        out["next_cursor"] = json!(c);
    }
    Ok(out)
}

pub(super) fn chat_message_to_json(m: &ChatMessage) -> Value {
    let mut out = json!({
        "chat_group_id": m.chat_group_id,
        "msg_id": m.msg_id,
        "ts": m.ts,
        "role": m.role,
        "content": m.content,
        "embedded": m.embedded,
    });
    if let Some(author) = &m.author {
        out["author"] = json!(author);
    }
    if let Some(meta) = &m.metadata {
        out["metadata"] = meta.clone();
    }
    out
}

// --- events / inbox tools (M7 — Event-sourcing surface) --------

#[derive(Deserialize)]
pub(super) struct CaptureEventArgs {
    #[serde(default)]
    event_id: Option<String>,
    #[serde(default)]
    at: Option<String>,
    #[serde(default)]
    source: String,
    #[serde(default)]
    mime: String,
    /// Skill id that knows how to process this event type (the label→skill link).
    #[serde(default)]
    label_skill: String,
    /// Optional candidate instance (Gmail-label style); the event still
    /// lands in the inbox until `assign_event`.
    #[serde(default)]
    instance_page_id: Option<String>,
    #[serde(default)]
    title: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    provenance: Option<Value>,
}

/// The principal to persist for a write by `subject` (escurel#357 / CR-6):
/// `pages.last_written_by`, `crdt_ops.principal`.
///
/// The value is the verified token subject and nothing else — no tool
/// argument, frontmatter key or `provenance` block is an input here, which
/// is what makes the stamp unforgeable. Same discipline as
/// [`stamp_captured_by`], one layer down.
///
/// An empty subject becomes `None`. That only arises on a gateway with no
/// verifier wired (the WS dev path), where there is no principal to record
/// and `""` would read as an attributed write by a zero-length name. The
/// HTTP path never produces it: `mcp.rs` substitutes `"anonymous"`, which is
/// itself an honest answer — an open gateway really was written to by
/// someone unidentified, and recording that is more useful than NULL.
pub(crate) fn stamped_principal(subject: &str) -> Option<&str> {
    (!subject.is_empty()).then_some(subject)
}

/// Stamp the verified caller subject into an event's `provenance` as the
/// server-owned `captured_by` claim, replacing anything the client sent
/// under that key (a forged stamp would be an ACL bypass).
///
/// Done in EVERY [`crate::server::EventAclMode`], including `Off`: the
/// stamp is data, not a decision, and without it flipping the mode on
/// later would find a history of events with no recorded owner.
///
/// A non-object `provenance` (a bare string/number a caller supplied) is
/// promoted to `{"provenance": <old>, "captured_by": …}` rather than
/// dropped, so no caller data is lost to the stamp.
pub(super) fn stamp_captured_by(provenance: Option<Value>, subject: &str) -> Option<Value> {
    let mut obj = match provenance {
        Some(Value::Object(m)) => Value::Object(m),
        None | Some(Value::Null) => json!({}),
        Some(other) => json!({ "provenance": other }),
    };
    obj[escurel_index::CAPTURED_BY_FIELD] = json!(subject);
    Some(obj)
}

pub(super) async fn tool_capture_event(
    indexer: &Indexer,
    caller: AclCaller<'_>,
    event_acl: crate::server::EventAclMode,
    webhook: Option<&crate::webhook::Webhook>,
    events_tx: &tokio::sync::broadcast::Sender<std::sync::Arc<EventInfo>>,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: CaptureEventArgs = parse_args(args, "capture_event")?;
    let requested = NewEvent {
        event_id: a.event_id,
        at: a.at,
        source: a.source,
        mime: a.mime,
        label_skill: a.label_skill,
        instance_page_id: a.instance_page_id,
        title: a.title,
        body: a.body,
        provenance: stamp_captured_by(a.provenance, caller.subject),
    };
    let stored = indexer
        .capture_event(requested.clone())
        .await
        .map_err(|e| JsonRpcError::internal(format!("capture_event: {e}")))?;

    // `capture_event` is idempotent on `event_id` and returns the STORED
    // (first-writer) row — which quietly makes it a READ, and so a way
    // around every filter below if the id can be guessed. It can be:
    // `event_id` IS the caller's idempotency key, chosen client-side.
    //
    // A caller who may not see the stored row gets its OWN submission back
    // instead, wearing the stored id — indistinguishable from a first
    // capture, disclosing neither the other caller's content nor the fact
    // that the id was taken. The owner's retry is untouched: it can read
    // the stored row, so it still gets the authoritative first-writer event
    // that makes a retry converge instead of forking.
    let visible = event_acl == crate::server::EventAclMode::Off
        || indexer
            .may_read_event(&caller, &stored)
            .await
            .map_err(|e| JsonRpcError::internal(format!("capture_event acl: {e}")))?;
    if !visible && event_acl == crate::server::EventAclMode::Log {
        tracing::warn!(
            subject = %caller.subject, event_id = %stored.event_id,
            "event-ACL would hide this idempotent read-back (log mode) — showing"
        );
    }
    let event = if visible || event_acl == crate::server::EventAclMode::Log {
        event_to_json(&stored)
    } else {
        event_to_json(&echoed_event(&stored, &requested))
    };
    // Notify any external processor of the new inbox item (opt-in,
    // fire-and-forget; never fails the capture). The gateway is
    // single-tenant per indexer, so `indexer.tenant()` is the
    // authoritative tenant we stamp into the delivered payload (#147).
    //
    // Always the STORED event, never the echo above: the webhook is a
    // server-to-server announcement of what is in the store, and a
    // subscriber that received the echo would process a row that does not
    // exist. The echo exists to withhold data from a caller, not to lie to
    // the pipeline.
    if let Some(hook) = webhook {
        hook.notify(event_to_json(&stored), indexer.tenant());
    }
    // Same announcement to in-process WS subscribers (#333) — the STORED
    // row for the same reason as the webhook above; each subscriber's
    // ACL filter runs at delivery, not here.
    let _ = events_tx.send(std::sync::Arc::new(stored));
    Ok(event)
}

/// The response for an idempotent re-capture the caller may NOT read: its
/// own submission, wearing the stored `event_id` and `status`.
///
/// Everything else — title, body, provenance, source, the first writer's
/// timestamp — comes from what THIS caller sent, so the response carries no
/// bit of the stored row and reads exactly like a first capture. Only
/// `event_id` (which the caller supplied anyway) and `status` (always
/// `inbox` for a fresh capture) come from the store.
fn echoed_event(stored: &EventInfo, requested: &NewEvent) -> EventInfo {
    EventInfo {
        event_id: stored.event_id.clone(),
        at: requested.at.clone(),
        source: requested.source.clone(),
        mime: requested.mime.clone(),
        label_skill: requested.label_skill.clone(),
        instance_page_id: requested.instance_page_id.clone(),
        status: "inbox".to_owned(),
        title: requested.title.clone(),
        body: requested.body.clone(),
        provenance: requested.provenance.clone().unwrap_or(Value::Null),
    }
}

/// Announce a confirmed `update_page` write to the outbound webhook.
///
/// ## Notification, not work
///
/// This does **not** capture an inbox item. The inbox is a work queue: the
/// runner drains it and dispatches each entry. Turning every write into an
/// inbox entry makes a write into work, and then
///
///   * a write announces itself,
///   * the runner dispatches the announcement,
///   * processing it writes again,
///
/// which is unbounded — a page-write carries no cascade lineage for #157's
/// depth and budget caps to bite on. Measured, not theorised: with writes
/// enqueued, one demo run produced 122 announcements for a single skill and
/// starved the real workflow behind a quota gate.
///
/// So a page write is delivered as a **notification only**. Consumers that
/// want to be woken by a write subscribe to the webhook; the work queue
/// keeps its meaning. This is also what makes workflow completion
/// observable at all: a runner-authored write is exactly the interesting
/// one, and any inbox-based scheme has to suppress it to avoid the loop.
///
/// Best-effort and fire-and-forget, like the capture webhook: a failure
/// never touches the write, which already succeeded.
pub(super) fn emit_page_event(
    indexer: &Indexer,
    webhook: Option<&crate::webhook::Webhook>,
    a: &UpdatePageArgs,
    new_version: &str,
) {
    let Some(hook) = webhook else { return };
    // Instance pages only: the skill lives in the path
    // (`markdown/instances/<skill>/<id>.md`). A skill-page write is not an
    // instance change and is not announced.
    let Some(skill) = a
        .page_id
        .split('/')
        .nth(2)
        .filter(|_| a.page_id.starts_with("markdown/instances/"))
        .map(str::to_string)
    else {
        return;
    };
    hook.notify(
        json!({
            "kind": "page_write",
            "label_skill": skill,
            "instance_page_id": a.page_id,
            "new_version": new_version,
        }),
        indexer.tenant(),
    );
}

#[derive(Deserialize)]
pub(super) struct ListInboxArgs {
    #[serde(default)]
    limit: Option<usize>,
    /// Resume cursor from a previous page's `next_cursor`.
    #[serde(default)]
    cursor: Option<String>,
}

/// Drop the events `caller` may not see (`Indexer::may_read_event`),
/// preserving order — the same post-filter `list_instances` applies to
/// instance rows. Enumeration must not leak what a direct read would deny.
///
/// In [`crate::server::EventAclMode::Off`] this is the identity function;
/// in `Log` it warns and keeps the row, so an operator can find the
/// callers a switch to `Enforce` would blind before it blinds them.
///
/// `limit` is applied by the query, before this filter, exactly as it is
/// for `list_instances`: a page may come back short, and a caller that
/// needs N rows pages until it has them.
async fn acl_filter_events(
    indexer: &Indexer,
    caller: &AclCaller<'_>,
    mode: crate::server::EventAclMode,
    tool: &str,
    events: Vec<EventInfo>,
) -> Result<Vec<EventInfo>, JsonRpcError> {
    if mode == crate::server::EventAclMode::Off {
        return Ok(events);
    }
    let mut out = Vec::with_capacity(events.len());
    for e in events {
        let allowed = indexer
            .may_read_event(caller, &e)
            .await
            .map_err(|err| JsonRpcError::internal(format!("{tool} acl: {err}")))?;
        if allowed {
            out.push(e);
        } else if mode == crate::server::EventAclMode::Log {
            tracing::warn!(
                subject = %caller.subject, event_id = %e.event_id, tool,
                "event-ACL would hide this event (log mode) — showing"
            );
            out.push(e);
        }
    }
    Ok(out)
}

pub(super) async fn tool_list_inbox(
    indexer: &Indexer,
    caller: AclCaller<'_>,
    event_acl: crate::server::EventAclMode,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: ListInboxArgs = parse_args(args, "list_inbox")?;
    let page = indexer
        .list_inbox_page(
            a.limit.unwrap_or(escurel_index::EVENTS_MAX_LIMIT),
            a.cursor.as_deref(),
        )
        .await
        .map_err(|e| cursor_aware_error("list_inbox", e))?;
    let events = acl_filter_events(indexer, &caller, event_acl, "list_inbox", page.events).await?;
    Ok(events_page_json(events, page.next_cursor))
}

/// An undecodable cursor is the caller's mistake (`invalid_params`),
/// not a server fault.
fn cursor_aware_error(tool: &str, e: IndexerError) -> JsonRpcError {
    match e {
        IndexerError::InvalidCursor(msg) => {
            JsonRpcError::invalid_params(format!("{tool}: cursor: {msg}"))
        }
        e => JsonRpcError::internal(format!("{tool}: {e}")),
    }
}

/// `{events, next_cursor?}` — `next_cursor` is present iff more rows
/// lie past the page. Its ABSENCE (never a short page — the ACL filter
/// shortens pages legitimately) is the termination signal.
fn events_page_json(events: Vec<EventInfo>, next_cursor: Option<String>) -> Value {
    let mut out = json!({ "events": events.iter().map(event_to_json).collect::<Vec<_>>() });
    if let Some(c) = next_cursor {
        out["next_cursor"] = json!(c);
    }
    out
}

#[derive(Deserialize)]
pub(super) struct ListEventsArgs {
    #[serde(default)]
    instance_page_id: String,
    #[serde(default)]
    limit: Option<usize>,
    /// By-event lookup. When present, returns just that event (whatever its
    /// status) and `instance_page_id` is ignored.
    #[serde(default)]
    event_id: Option<String>,
    /// Resume cursor from a previous page's `next_cursor` (listing
    /// branch only; meaningless with `event_id`).
    #[serde(default)]
    cursor: Option<String>,
}

pub(super) async fn tool_list_events(
    indexer: &Indexer,
    caller: AclCaller<'_>,
    event_acl: crate::server::EventAclMode,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: ListEventsArgs = parse_args(args, "list_events")?;

    // `event_id` answers "where did this event go?" — the question an
    // instance-scoped listing cannot ask, because you would need the answer
    // to form the query. An event with no match is an empty list, not an
    // error: "not found" is a legitimate answer here, and the caller
    // distinguishes it from "found, still in the inbox".
    let (events, next_cursor) = if let Some(event_id) = a.event_id.as_deref() {
        let events = indexer
            .get_event(event_id)
            .await
            .map_err(|e| JsonRpcError::internal(format!("list_events: {e}")))?
            .into_iter()
            .collect::<Vec<_>>();
        (events, None)
    } else {
        if a.instance_page_id.is_empty() {
            return Err(JsonRpcError::invalid_params(
                "list_events: one of `instance_page_id` or `event_id` is required".to_owned(),
            ));
        }
        let page = indexer
            .list_events_page(
                &a.instance_page_id,
                a.limit.unwrap_or(escurel_index::EVENTS_MAX_LIMIT),
                a.cursor.as_deref(),
            )
            .await
            .map_err(|e| cursor_aware_error("list_events", e))?;
        (page.events, page.next_cursor)
    };
    // Both branches are filtered: the by-event lookup is a direct read of
    // one row and must not be a way around the instance-scoped listing.
    // A hidden event comes back as an empty list — the same answer as an
    // event that does not exist, which is what this surface already
    // returns for a miss, so no existence is disclosed here either.
    let events = acl_filter_events(indexer, &caller, event_acl, "list_events", events).await?;
    Ok(events_page_json(events, next_cursor))
}

#[derive(Deserialize)]
pub(super) struct ListSnapshotsArgs {
    page_id: String,
}

pub(super) async fn tool_list_snapshots(
    indexer: &Indexer,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: ListSnapshotsArgs = parse_args(args, "list_snapshots")?;
    let snapshots = indexer
        .list_snapshots(&a.page_id)
        .await
        .map_err(|e| JsonRpcError::internal(format!("list_snapshots: {e}")))?;
    Ok(json!({ "snapshots": snapshots }))
}

#[derive(Deserialize)]
pub(super) struct AssignEventArgs {
    event_id: String,
    instance_page_id: String,
}

pub(super) async fn tool_assign_event(
    indexer: &Indexer,
    caller: AclCaller<'_>,
    event_acl: crate::server::EventAclMode,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: AssignEventArgs = parse_args(args, "assign_event")?;

    // Claiming an event you may not read is refused as NOT FOUND, using the
    // indexer's own `EventNotFound` so the code and the message are
    // byte-identical to a claim of an event that genuinely does not exist.
    // An error that distinguished the two would be an existence oracle:
    // "forbidden" tells you the event is there and whose it is, which is
    // most of what the listing refuses to tell you.
    //
    // This is a pre-check, NOT a replacement for the compare-and-set below:
    // the CAS is what makes a claim at-most-once under a race, and it still
    // runs unchanged. This check only decides whether the caller is allowed
    // to attempt it at all.
    if event_acl != crate::server::EventAclMode::Off {
        let event = indexer
            .get_event(&a.event_id)
            .await
            .map_err(|e| JsonRpcError::internal(format!("assign_event acl: {e}")))?;
        // Absent here is left to the CAS below, which reports it (and its
        // already-claimed sibling) with the precision the runner relies on.
        if let Some(event) = event {
            let allowed = indexer
                .may_read_event(&caller, &event)
                .await
                .map_err(|e| JsonRpcError::internal(format!("assign_event acl: {e}")))?;
            if !allowed {
                if event_acl == crate::server::EventAclMode::Log {
                    tracing::warn!(
                        subject = %caller.subject, event_id = %a.event_id,
                        "event-ACL would refuse this claim (log mode) — allowing"
                    );
                } else {
                    return Err(JsonRpcError::invalid_params(format!(
                        "assign_event: {}",
                        IndexerError::EventNotFound {
                            event_id: a.event_id.clone(),
                        }
                    )));
                }
            }
            // The TARGET check (#363): assignment is the operation that
            // decides which record an event belongs to — its body follows
            // the target's read audience from here on — so an unchecked
            // target is a visibility-laundering primitive, not just a
            // missing write check. Gated on the WRITE ACL of the target
            // (`may_assign_event_target`), refused in the same
            // `EventNotFound` shape as everything above so neither the
            // target's existence nor its ownership leaks.
            //
            // Only an UNASSIGNED event's claim is checked: a claim on an
            // already-assigned event never re-files it — the CAS below
            // reports `already assigned` (different target) or idempotent
            // `Ok` (same target), both without moving the event — and
            // collapsing those outcomes into not-found would break the
            // runner recovery path the CAS comment documents.
            let unassigned = event
                .instance_page_id
                .as_deref()
                .is_none_or(|cur| cur.is_empty());
            if unassigned {
                let target_ok = indexer
                    .may_assign_event_target(&caller, &a.instance_page_id)
                    .await
                    .map_err(|e| JsonRpcError::internal(format!("assign_event acl: {e}")))?;
                if !target_ok {
                    if event_acl == crate::server::EventAclMode::Log {
                        tracing::warn!(
                            subject = %caller.subject,
                            event_id = %a.event_id,
                            target = %a.instance_page_id,
                            "event-ACL would refuse this target (log mode) — allowing"
                        );
                    } else {
                        return Err(JsonRpcError::invalid_params(format!(
                            "assign_event: {}",
                            IndexerError::EventNotFound {
                                event_id: a.event_id.clone(),
                            }
                        )));
                    }
                }
            }
        }
    }

    indexer
        .assign_event(&a.event_id, &a.instance_page_id)
        .await
        .map_err(|e| match e {
            // Caller errors, not server faults: the event does not exist,
            // or another agent already claimed it. `invalid_params` matches
            // how `PackSkillMissing` is surfaced and — unlike `internal` —
            // tells a retrying caller that retrying will not help.
            // Re-assigning to the SAME instance returns Ok(()) and never
            // reaches here, so runner recovery is unaffected.
            IndexerError::EventNotFound { .. } | IndexerError::EventAlreadyAssigned { .. } => {
                JsonRpcError::invalid_params(format!("assign_event: {e}"))
            }
            other => JsonRpcError::internal(format!("assign_event: {other}")),
        })?;
    Ok(
        json!({ "event_id": a.event_id, "instance_page_id": a.instance_page_id, "status": "processed" }),
    )
}

pub(crate) fn event_to_json(e: &EventInfo) -> Value {
    json!({
        "event_id": e.event_id,
        "at": e.at,
        "source": e.source,
        "mime": e.mime,
        "label_skill": e.label_skill,
        "instance_page_id": e.instance_page_id,
        "status": e.status,
        "title": e.title,
        "body": e.body,
        "provenance": e.provenance,
    })
}

#[derive(Deserialize)]
pub(super) struct PurgePageArgs {
    page_id: String,
}

/// `purge_page`: hard-remove an archived husk. Admin-shaped rather than
/// agent-shaped — it destroys the audit record a soft delete kept, so the
/// dispatch arm gates it on the admin role and it refuses anything still
/// live.
pub(super) async fn tool_purge_page(
    state: &crate::server::AppState,
    indexer: &Indexer,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: PurgePageArgs = serde_json::from_value(args)
        .map_err(|e| JsonRpcError::invalid_params(format!("purge_page: {e}")))?;

    match indexer.purge_page(&a.page_id).await {
        Ok(true) => {
            state.metrics.inc_write(indexer.tenant(), "human");
            Ok(json!({ "ok": true, "issues": [], "page_id": a.page_id }))
        }
        Ok(false) => Ok(json!({
            "ok": false,
            "issues": [{
                "severity": "error",
                "code": "not_found",
                "location": "page_id",
                "message": format!("no page `{}` to purge", a.page_id),
            }],
        })),
        Err(IndexerError::NotArchived { page_id }) => Ok(json!({
            "ok": false,
            "issues": [{
                "severity": "error",
                "code": "not_archived",
                "location": "page_id",
                "message": format!(
                    "`{page_id}` is live; retract it with delete_page before purging"
                ),
            }],
        })),
        Err(IndexerError::MetaSkillProtected { reason }) => Ok(json!({
            "ok": false,
            "issues": [{
                "severity": "error",
                "code": "meta_skill_protected",
                "location": "frontmatter",
                "message": reason,
            }],
        })),
        Err(e) => Err(JsonRpcError::internal(format!("purge_page: {e}"))),
    }
}
