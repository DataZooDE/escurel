//! `list_lineage` — one tenant-scoped read for a thread (knowledge-workbench
//! backend P1, BRD FR-L-1): everything under a root event as a flat list of
//! nodes with ids and parents, for the client to fold into a tree.
//!
//! The tree strictly alternates **event → run → {changeset → draft | draft
//! | event}**: an event's parent is the run that emitted it
//! (`provenance.runner.parent_run_id`, null for the root), a run's parent
//! is the event that triggered it, a changeset's the run that proposed it,
//! a draft's its changeset (or its run). Runs are folded from their
//! `escurel:run` rows; `escurel:review` rows are not nodes (the drafts
//! carry that state). One indexed read over the lineage's events (paged
//! with the usual cursor) plus one over its drafts.
//!
//! ACL fails closed per node and prunes the subtree: a node is emitted only
//! when it is readable, its root is readable, and every parent present on
//! this page is. Denial is absence, never an error. A draft whose run is not
//! on this page hangs off the root (its `run_id` still names the run), so a
//! runner that writes no run events still yields a usable tree.

use std::collections::{BTreeMap, HashMap};

use escurel_index::{AclCaller, EventInfo, EventListFilter, Indexer};
use serde::Deserialize;
use serde_json::{Value, json};

use super::tools_drafts::may_see;
use super::{JsonRpcError, parse_args};

const DEFAULT_LIMIT: usize = 500;

#[derive(Deserialize)]
pub(super) struct ListLineageArgs {
    #[serde(default)]
    root_event_id: String,
    /// Node types to return: `events`, `runs`, `drafts` (drafts implies
    /// changesets). Empty = all.
    #[serde(default)]
    include: Vec<String>,
    /// Events per page (the drafts half is not paged).
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    cursor: Option<String>,
}

/// A run folded from its lifecycle rows.
#[derive(Default)]
struct RunAgg {
    parent: Option<String>,
    state: String,
    visible: Option<bool>,
    attrs: serde_json::Map<String, Value>,
}

/// One candidate node before pruning.
struct Candidate {
    id: String,
    kind: &'static str,
    parent: Option<String>,
    visible: bool,
    value: Value,
}

async fn readable(
    indexer: &Indexer,
    caller: &AclCaller<'_>,
    mode: crate::server::EventAclMode,
    e: &EventInfo,
) -> Result<bool, JsonRpcError> {
    if mode == crate::server::EventAclMode::Off {
        return Ok(true);
    }
    let allowed = indexer
        .may_read_event(caller, e)
        .await
        .map_err(|err| JsonRpcError::internal(format!("list_lineage acl: {err}")))?;
    if !allowed && mode == crate::server::EventAclMode::Log {
        tracing::warn!(
            subject = %caller.subject, event_id = %e.event_id,
            "event-ACL would prune this lineage node (log mode) — showing"
        );
        return Ok(true);
    }
    Ok(allowed)
}

pub(super) async fn tool_list_lineage(
    indexer: &Indexer,
    caller: AclCaller<'_>,
    event_acl: crate::server::EventAclMode,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: ListLineageArgs = parse_args(args, "list_lineage")?;
    let root = a.root_event_id.trim();
    if root.is_empty() {
        return Err(JsonRpcError::invalid_params(
            "list_lineage: `root_event_id` is required".to_owned(),
        ));
    }
    for inc in &a.include {
        if !matches!(inc.as_str(), "events" | "runs" | "drafts" | "tool_calls") {
            return Err(JsonRpcError::invalid_params(format!(
                "list_lineage: `include` entries are events | runs | drafts | tool_calls, got `{inc}`"
            )));
        }
    }
    let want = |t: &str| a.include.is_empty() || a.include.iter().any(|i| i == t);

    // The root anchors every chain, on every page — resolved once, on its
    // own. An unknown root is an empty tree, not an error; an unreadable one
    // prunes everything (denial is absence).
    let root_visible = match indexer
        .get_event(root)
        .await
        .map_err(|e| JsonRpcError::internal(format!("list_lineage: {e}")))?
    {
        Some(e) => readable(indexer, &caller, event_acl, &e).await?,
        None => {
            return Ok(json!({ "root_event_id": root, "nodes": [] }));
        }
    };
    if !root_visible {
        return Ok(json!({ "root_event_id": root, "nodes": [] }));
    }

    let page = indexer
        .list_events_filtered_page(
            &EventListFilter {
                root_event_id: Some(root.to_owned()),
                include_system: true,
                ..Default::default()
            },
            true,
            a.limit.unwrap_or(DEFAULT_LIMIT),
            a.cursor.as_deref(),
        )
        .await
        .map_err(|e| super::tools_write::cursor_aware_error("list_lineage", e))?;

    let mut candidates: Vec<Candidate> = Vec::new();
    let mut runs: BTreeMap<usize, (String, RunAgg)> = BTreeMap::new();
    let mut run_index: HashMap<String, usize> = HashMap::new();
    for e in &page.events {
        let vis = readable(indexer, &caller, event_acl, e).await?;
        let runner = &e.provenance["runner"];
        if e.label_skill == "escurel:run"
            && let Some(run_id) = e.run_id.clone()
        {
            let idx = *run_index
                .entry(run_id.clone())
                .or_insert_with(|| runs.len());
            let (_, agg) = runs.entry(idx).or_insert_with(|| {
                let mut agg = RunAgg {
                    state: "running".to_owned(),
                    ..Default::default()
                };
                agg.parent = runner["event_id"].as_str().map(str::to_owned);
                (run_id.clone(), agg)
            });
            // A run is as visible as its first row (its `run-started`, on the
            // target page or unassigned): one rule for the whole run.
            agg.visible.get_or_insert(vis);
            for key in [
                "harness",
                "model",
                "max_attempts",
                "target_page_id",
                "trace_id",
                "depth",
            ] {
                if let Some(v) = runner.get(key)
                    && !v.is_null()
                {
                    agg.attrs.entry(key.to_owned()).or_insert_with(|| v.clone());
                }
            }
            let body: Value = serde_json::from_str(&e.body).unwrap_or(Value::Null);
            match e.title.as_str() {
                "run-started" => {
                    agg.attrs.insert("started_at".to_owned(), json!(e.at));
                }
                "run-attempt" => {
                    if let Some(n) = body["attempt"].as_u64() {
                        agg.attrs.insert("attempt".to_owned(), json!(n));
                    }
                }
                "run-progress" => {
                    if !body["plan"].is_null() {
                        agg.attrs.insert("plan".to_owned(), body["plan"].clone());
                    }
                }
                "run-finished" => {
                    if let Some(s) = body["status"].as_str() {
                        agg.state = s.to_owned();
                    }
                    agg.attrs.insert("finished_at".to_owned(), json!(e.at));
                    for key in [
                        "summary",
                        "attempts",
                        "produced_instance",
                        "produced_version",
                        "reason",
                        "held",
                        "tool_calls",
                        "usage",
                    ] {
                        if let Some(v) = body.get(key)
                            && !v.is_null()
                        {
                            agg.attrs.insert(key.to_owned(), v.clone());
                        }
                    }
                    if !body["plan"].is_null() {
                        agg.attrs.insert("plan".to_owned(), body["plan"].clone());
                    }
                    if let Some(v) = runner.get("autonomy") {
                        agg.attrs.insert("autonomy".to_owned(), v.clone());
                    }
                }
                _ => {}
            }
            continue;
        }
        if e.label_skill == "escurel:review" {
            continue;
        }
        let parent = if e.event_id == root {
            None
        } else {
            runner["parent_run_id"].as_str().map(str::to_owned)
        };
        candidates.push(Candidate {
            id: e.event_id.clone(),
            kind: "event",
            parent,
            visible: vis,
            value: json!({
                "state": e.status,
                "label_skill": e.label_skill,
                "title": e.title,
                "at": e.at,
                "kind": e.kind.as_str(),
                "instance_page_id": e.instance_page_id,
                "parent_event_id": runner["parent_event_id"],
                "depth": runner["depth"],
            }),
        });
    }
    // A summary of each run's recorded `/mcp` calls (P3-2), only when
    // asked for: one grouped read for every run on this page.
    let summaries = if a.include.iter().any(|i| i == "tool_calls") {
        let ids: Vec<String> = runs.values().map(|(id, _)| id.clone()).collect();
        indexer
            .run_tool_call_summaries(&ids)
            .await
            .map_err(|e| JsonRpcError::internal(format!("list_lineage tool_calls: {e}")))?
    } else {
        std::collections::HashMap::new()
    };
    for (run_id, agg) in runs.into_values() {
        let summary = summaries
            .get(&run_id)
            .map(|s| json!({ "count": s.count, "failed": s.failed, "duration_ms": s.duration_ms }));
        candidates.push(Candidate {
            id: run_id,
            kind: "run",
            parent: agg.parent,
            visible: agg.visible.unwrap_or(false),
            value: {
                let mut v = Value::Object(agg.attrs);
                v["state"] = json!(agg.state);
                if let Some(s) = summary {
                    v["tool_call_summary"] = s;
                }
                v
            },
        });
    }
    let run_ids: Vec<String> = candidates
        .iter()
        .filter(|c| c.kind == "run")
        .map(|c| c.id.clone())
        .collect();

    if want("drafts") {
        let drafts = indexer
            .list_drafts_for_root(root)
            .await
            .map_err(|e| JsonRpcError::internal(format!("list_lineage drafts: {e}")))?;
        // A run not on this page (paged away, or never written by a
        // static-bearer runner) anchors its drafts at the root instead.
        let run_parent = |run_id: &Option<String>| -> String {
            match run_id {
                Some(r) if run_ids.contains(r) => r.clone(),
                _ => root.to_owned(),
            }
        };
        let mut sets: BTreeMap<String, (Vec<usize>, bool)> = BTreeMap::new();
        let mut draft_nodes = Vec::new();
        for d in &drafts {
            let vis = may_see(indexer, &caller, d).await?;
            let parent = match &d.changeset_id {
                Some(cs) => cs.clone(),
                None => run_parent(&d.run_id),
            };
            if let Some(cs) = &d.changeset_id {
                let entry = sets.entry(cs.clone()).or_insert_with(|| (Vec::new(), true));
                entry.0.push(draft_nodes.len());
                entry.1 &= vis;
            }
            draft_nodes.push(Candidate {
                id: d.draft_id.clone(),
                kind: "draft",
                parent: Some(parent),
                visible: vis,
                value: json!({
                    "state": d.status,
                    "target_page_id": d.target_page_id,
                    "author": d.author,
                    "decided_by": d.decided_by,
                    "event_id": d.event_id,
                    "changeset_id": d.changeset_id,
                    "run_id": d.run_id,
                    "created_at": d.created_at,
                }),
            });
        }
        for (cs, (members, all_visible)) in sets {
            let statuses: Vec<&str> = members
                .iter()
                .map(|i| draft_nodes[*i].value["state"].as_str().unwrap_or(""))
                .collect();
            let state = if statuses.contains(&"open") {
                "open"
            } else if statuses.iter().all(|s| *s == "promoted") {
                "promoted"
            } else if statuses.iter().all(|s| *s == "discarded") {
                "discarded"
            } else {
                "mixed"
            };
            let first = &drafts[members[0]];
            candidates.push(Candidate {
                id: cs.clone(),
                kind: "changeset",
                parent: Some(run_parent(&first.run_id)),
                // A changeset is decided whole, so it is seen whole.
                visible: all_visible,
                value: json!({
                    "state": state,
                    "drafts": members.len(),
                    "author": first.author,
                    "run_id": first.run_id,
                }),
            });
        }
        candidates.extend(draft_nodes);
    }

    // Prune: a node stands only on a visible chain. Parents not on this page
    // (paged away) are not held against it — their own page prunes them.
    let visible_by_id: HashMap<&str, bool> = candidates
        .iter()
        .map(|c| (c.id.as_str(), c.visible))
        .collect();
    let mut memo: HashMap<String, bool> = HashMap::new();
    fn effective(
        id: &str,
        root: &str,
        candidates: &[Candidate],
        visible_by_id: &HashMap<&str, bool>,
        memo: &mut HashMap<String, bool>,
    ) -> bool {
        if let Some(v) = memo.get(id) {
            return *v;
        }
        let own = visible_by_id.get(id).copied().unwrap_or(true);
        let parent_ok = candidates
            .iter()
            .find(|c| c.id == id)
            .and_then(|c| c.parent.clone())
            .is_none_or(|p| p == root || effective(&p, root, candidates, visible_by_id, memo));
        let v = own && parent_ok;
        memo.insert(id.to_owned(), v);
        v
    }
    let nodes: Vec<Value> = candidates
        .iter()
        .filter(|c| match c.kind {
            "event" => want("events"),
            "run" => want("runs"),
            _ => want("drafts"),
        })
        .filter(|c| effective(&c.id, root, &candidates, &visible_by_id, &mut memo))
        .map(|c| {
            let mut v = c.value.clone();
            v["id"] = json!(c.id);
            v["type"] = json!(c.kind);
            v["parent"] = json!(c.parent);
            if v.get("state").is_none() {
                v["state"] = json!(null);
            }
            v
        })
        .collect();

    let mut out = json!({ "root_event_id": root, "nodes": nodes });
    if let Some(c) = page.next_cursor {
        out["next_cursor"] = json!(c);
    }
    Ok(out)
}
