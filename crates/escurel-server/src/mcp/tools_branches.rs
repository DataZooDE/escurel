//! Branches: `create_branch`, `list_branches`, `merge_branch`,
//! `abandon_branch` — and the write context that makes them safe (#512).
//!
//! escurel already read `base ∪ overlay` with a per-slug override, which is a
//! branch VIEW. These are the parts that make it a branch:
//!
//! - a **registry**, so a branch has an author, a base and a decidable
//!   status rather than being a string somebody typed into frontmatter;
//! - the branch as a property of the **WRITE**, not of the page. This is the
//!   dangerous one the issue singles out: an agent "working on branch B" had
//!   to remember to stamp `scenario: B` into every page it wrote, and ONE
//!   forgotten stamp wrote to production. Now the write names its branch out
//!   of band, the server derives the overlay page id and stamps the scenario,
//!   and the agent cannot get it wrong because it never types it;
//! - **tombstones**, so a branch can say "this was wrong, remove it";
//! - a **merge verb**, so a branch can land — through the same three-way
//!   merge `update_page` already performs, not a second one.
//!
//! The merge is all-or-nothing, and for the same reason a changeset is: a
//! branch that lands half its pages leaves the corpus in a state nobody
//! authored. Every member is pre-checked before anything is written.

use super::*;

/// Derive the overlay page id a branch write lands on.
///
/// `markdown/instances/note/alpha.md` + `wip` →
/// `markdown/instances/note/alpha@wip.md`. The base and its overlay are two
/// files sharing one slug, which is exactly what the scenario read model
/// expects — and deriving it here is what keeps the branch out of the
/// caller's hands.
///
/// The branch name is sanitised into the filename: a name with a `/`
/// (`agent/inbox-scan`, the issue's own example) must not become a directory.
#[must_use]
pub(super) fn overlay_page_id(page_id: &str, branch: &str) -> String {
    let slug_safe: String = branch
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect();
    match page_id.rsplit_once('.') {
        Some((stem, ext)) => format!("{stem}@{slug_safe}.{ext}"),
        None => format!("{page_id}@{slug_safe}"),
    }
}

/// Stamp `scenario: <branch>` into a document's frontmatter, replacing any
/// value the caller supplied.
///
/// Server-owned, like `captured_by` and `last_written_by`: a caller who could
/// choose the scenario could write into somebody else's branch, and a caller
/// who could omit it would write to the base.
#[must_use]
pub(super) fn stamp_scenario(content: &str, branch: &str) -> String {
    // Unparseable content is refused by validation further down; leave it
    // untouched rather than guessing where its frontmatter ends.
    escurel_md::set_frontmatter_str(content, "scenario", Some(branch))
        .unwrap_or_else(|_| content.to_owned())
}

/// Resolve a named branch that is open, or the refusal that stops the write.
///
/// A write naming a branch that was never opened is REFUSED rather than
/// silently creating one: a typo must not conjure an isolated workspace
/// nobody knows about, which is the failure the registry exists to prevent.
pub(super) async fn require_open_branch(
    indexer: &Indexer,
    name: &str,
) -> Result<Result<escurel_index::BranchInfo, Value>, JsonRpcError> {
    let found = indexer
        .get_branch(name)
        .await
        .map_err(|e| JsonRpcError::internal(format!("branch: {e}")))?;
    let Some(branch) = found else {
        return Ok(Err(json!({
            "ok": false,
            "issues": [{
                "severity": "error",
                "code": "unknown_branch",
                "location": "branch",
                "message": format!(
                    "no branch `{name}` — open one with `create_branch` first; a write \
                     naming an unknown branch is refused rather than creating one"
                ),
            }],
        })));
    };
    if branch.status != "open" {
        return Ok(Err(json!({
            "ok": false,
            "issues": [{
                "severity": "error",
                "code": "already_decided",
                "location": "branch",
                "message": format!(
                    "branch `{name}` was already {} by `{}`; an abandoned or merged \
                     workspace must not keep accumulating work",
                    branch.status, branch.decided_by
                ),
            }],
        })));
    }
    Ok(Ok(branch))
}

#[derive(Deserialize)]
pub(super) struct CreateBranchArgs {
    name: String,
}

#[derive(Deserialize)]
pub(super) struct DecideBranchArgs {
    name: String,
    #[serde(default)]
    reason: String,
}

/// Open an isolated workspace.
pub(super) async fn tool_create_branch(
    state: &crate::server::AppState,
    indexer: &Indexer,
    caller: AclCaller<'_>,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: CreateBranchArgs = parse_args(args, "create_branch")?;
    let name = a.name.trim();
    if name.is_empty() {
        return Err(JsonRpcError::invalid_params(
            "create_branch: `name` is required — a branch is identified by it".to_owned(),
        ));
    }

    // The corpus state the branch forks from. A merge asks "did the base twin
    // move since the branch started?", and answering that from the branch's
    // own pages would mean trusting whatever the branch contains.
    let base_version = match state.crdt_backend.as_ref() {
        Some(backend) => {
            let hlc = u64::try_from(backend.max_hlc("").await.unwrap_or(0)).unwrap_or(0);
            format!("v{hlc}")
        }
        // No CRDT backend: record the wall clock instead of a version that
        // does not exist. It is not comparable, and the merge says so rather
        // than pretending a `v0` means anything.
        None => format!(
            "t{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
        ),
    };

    match indexer
        .create_branch(name, caller.subject, &base_version)
        .await
        .map_err(|e| JsonRpcError::internal(format!("create_branch: {e}")))?
    {
        Some(branch) => Ok(json!({ "ok": true, "branch": branch_to_json(&branch) })),
        None => Ok(json!({
            "ok": false,
            "issues": [{
                "severity": "error",
                "code": "already_exists",
                "location": "name",
                "message": format!(
                    "branch `{name}` already exists — joining somebody else's workspace \
                     by accident is what a named registry exists to prevent"
                ),
            }],
        })),
    }
}

/// Every branch, newest first — including decided ones.
pub(super) async fn tool_list_branches(
    indexer: &Indexer,
    _caller: AclCaller<'_>,
    _args: Value,
) -> Result<Value, JsonRpcError> {
    let branches = indexer
        .list_branches()
        .await
        .map_err(|e| JsonRpcError::internal(format!("list_branches: {e}")))?;
    Ok(json!({
        "branches": branches.iter().map(branch_to_json).collect::<Vec<_>>(),
    }))
}

fn branch_to_json(b: &escurel_index::BranchInfo) -> Value {
    json!({
        "name": b.name,
        "base_version": b.base_version,
        "author": b.author,
        "status": b.status,
        "reason": b.reason,
        "decided_by": b.decided_by,
        "created_at": b.created_at,
    })
}

/// Land a branch onto the base timeline.
///
/// All-or-nothing by pre-flight, exactly like `promote_changeset`: every page
/// the branch carries is checked first, and a single member that could not
/// land blocks the whole merge. A branch that landed half its pages would
/// leave the corpus in a state nobody authored — and unlike a changeset, a
/// branch may be hours of work, so the half-landed state would be much harder
/// to reason back out of.
///
/// Each member goes through the ORDINARY write path (`update_page` /
/// `delete_page`) so the layer, backend, curator and validation guards, the
/// CAS, the lake publish and the provenance stamp all apply. The merge is not
/// a second write path, and it introduces no second merge algorithm: a base
/// twin that moved is handled by the three-way merge `update_page` already
/// performs.
pub(super) async fn tool_merge_branch(
    state: &crate::server::AppState,
    indexer: &Indexer,
    caller: AclCaller<'_>,
    write_acl: crate::server::WriteAclMode,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: DecideBranchArgs = parse_args(args, "merge_branch")?;
    let branch = match require_open_branch(indexer, &a.name).await? {
        Ok(b) => b,
        Err(refusal) => return Ok(refusal),
    };

    let pages = indexer
        .branch_pages(&branch.name)
        .await
        .map_err(|e| JsonRpcError::internal(format!("merge_branch: {e}")))?;
    if pages.is_empty() {
        return Ok(json!({
            "ok": false,
            "issues": [{
                "severity": "error",
                "code": "empty_branch",
                "location": "name",
                "message": format!("branch `{}` carries no pages; nothing to merge", branch.name),
            }],
        }));
    }

    // ── Pre-flight. Nothing is written until every member could be. ──
    let mut issues = Vec::new();
    let mut plan = Vec::new();
    for page in &pages {
        let base_twin = indexer
            .base_twin(&page.skill, &page.slug)
            .await
            .map_err(|e| JsonRpcError::internal(format!("merge_branch: {e}")))?;
        if page.deleted {
            // A tombstone needs something to remove. A branch that deleted a
            // slug the base never had is not a conflict — it is a no-op, and
            // saying so beats refusing the whole merge over it.
            plan.push((page.clone(), base_twin, None));
            continue;
        }
        let content = indexer
            .read_page_markdown(&page.page_id)
            .await
            .map_err(|e| JsonRpcError::internal(format!("merge_branch: {e}")))?;
        let Some(content) = content else {
            issues.push(json!({
                "severity": "error",
                "code": "conflict",
                "location": page.page_id,
                "message": format!(
                    "branch page `{}` has no stored markdown; the branch cannot land",
                    page.page_id
                ),
            }));
            continue;
        };
        // The content that lands on the base is the branch's content with the
        // scenario stamp REMOVED — a merged page belongs to the base
        // timeline, and a `scenario:` left on it would make it an overlay of
        // itself.
        let landed = strip_scenario(&content);
        let validation = indexer
            .validate(base_twin.as_deref(), &landed)
            .await
            .map_err(|e| JsonRpcError::internal(format!("merge_branch validate: {e}")))?;
        for issue in crate::mcp::tools_write::blocking_issues(state, &validation) {
            issues.push(crate::mcp::tools_read::issue_to_json(issue));
        }

        // **Did the base twin move since the branch forked, and can the two
        // be reconciled?** (#512 §4.) This is the question a merge exists to
        // ask, and it is asked HERE — before anything is written — because a
        // branch may be hours of work and a half-landed one is far harder to
        // reason back out of than a half-landed changeset.
        //
        // The answer comes from the merge that already ships: base twin
        // unchanged → land; moved with disjoint frontmatter keys →
        // three-way merge; the same key on both sides, or an unparseable
        // result → conflict. No second merge algorithm.
        if let (Some(base), Some(backend)) = (&base_twin, state.crdt_backend.as_ref())
            && branch.base_version.starts_with('v')
        {
            let head_hlc = u64::try_from(backend.max_hlc(base).await.unwrap_or(0)).unwrap_or(0);
            let head = Version::from_op_count(head_hlc);
            if head.as_str() != branch.base_version
                && crate::mcp::tools_write::try_auto_merge(
                    backend,
                    base,
                    &branch.base_version,
                    &landed,
                )
                .await
                .is_none()
            {
                issues.push(json!({
                    "severity": "error",
                    "code": "conflict",
                    "location": base,
                    "message": format!(
                        "`{base}` moved since branch `{}` forked at {} (head is {}) and the \
                         edits could not be auto-merged; nothing in the branch lands",
                        branch.name,
                        branch.base_version,
                        head.as_str()
                    ),
                }));
                continue;
            }
        }
        plan.push((page.clone(), base_twin, Some(landed)));
    }
    if !issues.is_empty() {
        return Ok(json!({ "ok": false, "name": branch.name, "issues": issues }));
    }

    // ── Apply. ──
    let mut results = Vec::new();
    for (page, base_twin, landed) in plan {
        let target = base_twin
            .clone()
            .unwrap_or_else(|| base_page_id(&page.page_id, &branch.name));
        let out = match landed {
            Some(content) => {
                // `base_version` travels so the write takes the SAME merge
                // path pre-flight just simulated — including ADR-0008's
                // re-run of the write guards on the merged artifact.
                let mut args = json!({ "page_id": target, "content": content });
                if base_twin.is_some() && branch.base_version.starts_with('v') {
                    args["base_version"] = json!(branch.base_version);
                }
                crate::mcp::tools_write::tool_update_page(state, indexer, caller, write_acl, args)
                    .await?
            }
            None => match &base_twin {
                Some(base) => {
                    crate::mcp::tools_write::tool_delete_page(
                        state,
                        indexer,
                        caller,
                        write_acl,
                        json!({ "page_id": base }),
                    )
                    .await?
                }
                // A tombstone for a slug the base never had: nothing to do,
                // and reported as such rather than as a failure.
                None => json!({ "ok": true, "no_op": true }),
            },
        };
        let landed_ok = out.get("ok").and_then(Value::as_bool) != Some(false);
        results.push(json!({
            "page_id": page.page_id,
            "target": target,
            "deleted": page.deleted,
            "ok": landed_ok,
        }));
        if !landed_ok {
            // Pre-flight said every member could land, so this is a race or
            // an I/O fault. Stop rather than pressing on; the members already
            // landed stay landed, and the branch stays OPEN so a re-run
            // completes it.
            tracing::warn!(
                branch = %branch.name,
                page_id = %page.page_id,
                outcome = %out,
                "merge_branch: a pre-flighted page refused mid-apply; stopping. \
                 Re-running the merge completes it."
            );
            return Ok(json!({
                "ok": false,
                "name": branch.name,
                "partial": true,
                "results": results,
                "issues": out.get("issues").cloned().unwrap_or(json!([])),
            }));
        }
    }

    indexer
        .close_branch(&branch.name, "merged", caller.subject, "")
        .await
        .map_err(|e| JsonRpcError::internal(format!("merge_branch close: {e}")))?;

    Ok(json!({
        "ok": true,
        "name": branch.name,
        "decided_by": caller.subject,
        "results": results,
    }))
}

/// Close a branch without landing anything.
pub(super) async fn tool_abandon_branch(
    indexer: &Indexer,
    caller: AclCaller<'_>,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: DecideBranchArgs = parse_args(args, "abandon_branch")?;
    match require_open_branch(indexer, &a.name).await? {
        Ok(_) => {}
        Err(refusal) => return Ok(refusal),
    }
    indexer
        .close_branch(&a.name, "abandoned", caller.subject, &a.reason)
        .await
        .map_err(|e| JsonRpcError::internal(format!("abandon_branch: {e}")))?;
    // The overlay pages are deliberately LEFT in place: they are the record
    // of what was proposed, they are invisible to the base timeline, and
    // deleting them would destroy the only evidence of an abandoned run.
    Ok(json!({
        "ok": true,
        "name": a.name,
        "decided_by": caller.subject,
        "reason": a.reason,
    }))
}

/// The base page id an overlay corresponds to — the inverse of
/// [`overlay_page_id`], used only when the base twin does not exist yet (the
/// branch CREATED the page).
fn base_page_id(overlay: &str, branch: &str) -> String {
    let slug_safe: String = branch
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect();
    overlay.replace(&format!("@{slug_safe}"), "")
}

/// Remove `scenario:` from a document's frontmatter.
///
/// A merged page belongs to the base timeline; a `scenario:` left on it would
/// make it an overlay of itself, invisible to every base read.
fn strip_scenario(content: &str) -> String {
    escurel_md::set_frontmatter_str(content, "scenario", None)
        .unwrap_or_else(|_| content.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_overlay_page_id_is_the_base_one_with_the_branch_appended() {
        assert_eq!(
            overlay_page_id("markdown/instances/note/alpha.md", "wip"),
            "markdown/instances/note/alpha@wip.md"
        );
        // A branch name with a slash is the issue's own example
        // (`agent/inbox-scan`) and must NOT become a directory.
        assert_eq!(
            overlay_page_id("markdown/instances/note/alpha.md", "agent/inbox-scan"),
            "markdown/instances/note/alpha@agent-inbox-scan.md"
        );
        assert_eq!(
            base_page_id("markdown/instances/note/alpha@wip.md", "wip"),
            "markdown/instances/note/alpha.md"
        );
    }

    #[test]
    fn the_scenario_stamp_is_server_owned_in_both_directions() {
        let doc = "---\ntype: instance\nskill: note\nid: a\n---\n# a\nbody\n";
        let stamped = stamp_scenario(doc, "wip");
        let fm = escurel_md::parse(&stamped)
            .expect("parses")
            .frontmatter
            .fields;
        assert_eq!(
            fm.get("scenario").and_then(escurel_md::YamlValue::as_str),
            Some("wip")
        );
        assert!(stamped.contains("body"), "the body survives: {stamped}");

        // A caller-supplied scenario is REPLACED, not honoured: choosing it
        // would mean writing into somebody else's branch.
        let forged = "---\ntype: instance\nskill: note\nid: a\nscenario: theirs\n---\n# a\n";
        let fm = escurel_md::parse(&stamp_scenario(forged, "mine"))
            .expect("parses")
            .frontmatter
            .fields;
        assert_eq!(
            fm.get("scenario").and_then(escurel_md::YamlValue::as_str),
            Some("mine")
        );

        // And stripping is the exact inverse: a merged page must not be an
        // overlay of itself.
        let merged = strip_scenario(&stamp_scenario(doc, "wip"));
        let fm = escurel_md::parse(&merged)
            .expect("parses")
            .frontmatter
            .fields;
        assert!(fm.get("scenario").is_none(), "{merged}");
        assert!(merged.contains("body"));
    }
}
