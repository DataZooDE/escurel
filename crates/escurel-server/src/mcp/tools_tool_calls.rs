//! `get_run_tool_calls` (knowledge-workbench backend P3-2): a run's `/mcp`
//! calls, oldest first, paged by `after` (the row's `seq`). Denial as
//! absence: a run the caller may not read — its `run-started` row is the
//! gate, as for `report_progress` — or one that does not exist answers an
//! empty page, never a refusal.

use escurel_index::{AclCaller, EventListFilter, Indexer, TOOL_CALLS_MAX_LIMIT};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{JsonRpcError, parse_args};

#[derive(Deserialize)]
pub(super) struct GetRunToolCallsArgs {
    #[serde(default)]
    run_id: String,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    after: Option<i64>,
}

/// How far into the run's own events to look for its `run-started`.
const RUN_SCAN: usize = 128;

/// May `caller` read this run? True when its `run-started` row is readable,
/// or when the run has no events yet but the caller IS the run (its own
/// bearer names it).
async fn may_read_run(
    indexer: &Indexer,
    caller: &AclCaller<'_>,
    event_acl: crate::server::EventAclMode,
    run_id: &str,
) -> Result<bool, JsonRpcError> {
    if caller.run_id == Some(run_id) {
        return Ok(true);
    }
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
        .map_err(|e| JsonRpcError::internal(format!("get_run_tool_calls: {e}")))?;
    let Some(started) = page
        .events
        .iter()
        .find(|e| e.label_skill == "escurel:run" && e.title == "run-started")
    else {
        // No record of the run at all: nothing to read (admin included —
        // the calls are the run's, and the run is not there).
        return Ok(caller.is_admin);
    };
    if event_acl == crate::server::EventAclMode::Off {
        return Ok(true);
    }
    let allowed = indexer
        .may_read_event(caller, started)
        .await
        .map_err(|e| JsonRpcError::internal(format!("get_run_tool_calls acl: {e}")))?;
    if !allowed && event_acl == crate::server::EventAclMode::Log {
        // Log mode is audit-only everywhere else (`list_events`,
        // `list_lineage`): warn, and show — a rollout on log mode must not
        // lose this read (codex second-opinion review of P3).
        tracing::warn!(
            subject = %caller.subject, run_id = %run_id,
            "event-ACL would hide this run's tool calls (log mode) — showing"
        );
        return Ok(true);
    }
    Ok(allowed)
}

pub(super) async fn tool_get_run_tool_calls(
    indexer: &Indexer,
    caller: AclCaller<'_>,
    event_acl: crate::server::EventAclMode,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: GetRunToolCallsArgs = parse_args(args, "get_run_tool_calls")?;
    if a.run_id.trim().is_empty() {
        return Err(JsonRpcError::invalid_params(
            "get_run_tool_calls: `run_id` is required".to_owned(),
        ));
    }
    let limit = a.limit.unwrap_or(100).clamp(1, TOOL_CALLS_MAX_LIMIT);
    if !may_read_run(indexer, &caller, event_acl, &a.run_id).await? {
        return Ok(json!({ "run_id": a.run_id, "calls": [], "next_after": null }));
    }
    let page = indexer
        .list_run_tool_calls(&a.run_id, limit, a.after)
        .await
        .map_err(|e| JsonRpcError::internal(format!("get_run_tool_calls: {e}")))?;
    let calls: Vec<Value> = page
        .calls
        .iter()
        .map(|c| {
            json!({
                "seq": c.seq,
                "tool": c.tool,
                "status": c.status,
                "error_code": c.error_code,
                "duration_ms": c.duration_ms,
                "request_bytes": c.request_bytes,
                "response_bytes": c.response_bytes,
                "subject": c.subject,
                "at": c.at,
            })
        })
        .collect();
    Ok(json!({ "run_id": a.run_id, "calls": calls, "next_after": page.next_after }))
}
