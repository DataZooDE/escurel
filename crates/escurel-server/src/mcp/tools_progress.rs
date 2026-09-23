//! `report_progress` — an agent's plan, as a snapshot (knowledge-workbench
//! backend P1, BRD FR-P-1/3).
//!
//! The harness calls this with its WHOLE plan every time a step changes;
//! the gateway files it as a `run-progress` system event under `escurel:run`
//! for the run the caller's token names. Only the token can say which run —
//! the runner minted `run_id` / `root_event_id` into it (PR4) — so a token
//! that belongs to no run is refused whatever its role, and the write goes
//! through the indexer directly rather than `capture_event`'s admin gate:
//! the agent token happens to be admin today, and this must not depend on
//! that. Idempotent per snapshot (the event id is a hash of the plan), and
//! bounded per run (the last [`KEEP_SNAPSHOTS`] survive).

use escurel_index::{EventKind, EventListFilter, Indexer, NewEvent};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{JsonRpcError, parse_args};

/// How many `run-progress` snapshots a run keeps (FR-P-3). `run-finished`
/// carries the final plan, so history past this is redundant for the
/// consumer and unbounded for the store.
pub(super) const KEEP_SNAPSHOTS: usize = 50;
/// The default for `ESCUREL_RUN_PROGRESS_KEEP`.
pub const DEFAULT_RUN_PROGRESS_KEEP: usize = KEEP_SNAPSHOTS;
const MAX_STEPS: usize = 100;
const MAX_STEP_CHARS: usize = 200;
const MAX_NOTE_BYTES: usize = 2048;
const STATUSES: [&str; 4] = ["pending", "in_progress", "completed", "blocked"];

#[derive(Deserialize)]
pub(super) struct ReportProgressArgs {
    #[serde(default)]
    plan: Vec<PlanStepArg>,
    #[serde(default)]
    current: Option<String>,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Deserialize, serde::Serialize)]
struct PlanStepArg {
    step: String,
    status: String,
}

pub(super) async fn tool_report_progress(
    indexer: &Indexer,
    caller: escurel_index::AclCaller<'_>,
    events_tx: &tokio::sync::broadcast::Sender<std::sync::Arc<escurel_index::EventInfo>>,
    keep: usize,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: ReportProgressArgs = parse_args(args, "report_progress")?;
    // Fail closed on the run, independent of the role: without a run there
    // is nothing to report progress OF, and an admin bearer is not a run.
    let Some(run_id) = caller.run_id.filter(|r| !r.is_empty()) else {
        return Err(JsonRpcError::invalid_params(
            "report_progress: requires a run-bound token (the per-run agent bearer \
             carries `run_id`); there is no run to report progress of"
                .to_owned(),
        ));
    };
    if a.plan.len() > MAX_STEPS {
        return Err(JsonRpcError::invalid_params(format!(
            "report_progress: at most {MAX_STEPS} steps"
        )));
    }
    for s in &a.plan {
        if s.step.trim().is_empty() || s.step.chars().count() > MAX_STEP_CHARS {
            return Err(JsonRpcError::invalid_params(format!(
                "report_progress: a step is 1..={MAX_STEP_CHARS} characters"
            )));
        }
        if !STATUSES.contains(&s.status.as_str()) {
            return Err(JsonRpcError::invalid_params(format!(
                "report_progress: step status must be one of {STATUSES:?}, got `{}`",
                s.status
            )));
        }
    }
    if a.note.as_deref().is_some_and(|n| n.len() > MAX_NOTE_BYTES) {
        return Err(JsonRpcError::invalid_params(format!(
            "report_progress: `note` is at most {MAX_NOTE_BYTES} bytes"
        )));
    }

    // Where to attach: the run's own `run-started` names the target page.
    // Absent (the runner has not announced the run, or does not emit
    // events) the snapshot stays unassigned — hidden bookkeeping, still
    // findable by `run_id`.
    let started = indexer
        .list_events_filtered_page(
            &EventListFilter {
                run_id: Some(run_id.to_owned()),
                include_system: true,
                ..Default::default()
            },
            true,
            EVENTS_SCAN,
            None,
        )
        .await
        .map_err(|e| JsonRpcError::internal(format!("report_progress: {e}")))?;
    let target = started
        .events
        .iter()
        .find(|e| e.title == "run-started")
        .and_then(|e| e.instance_page_id.clone());

    let body = json!({ "plan": a.plan, "current": a.current, "note": a.note });
    let body_text = body.to_string();
    // Idempotent per snapshot: the same plan reported twice is one event.
    let digest = {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(body_text.as_bytes()))
    };
    let event_id = format!("run-progress:{run_id}:{}", &digest[..16]);
    let mut runner = json!({
        "run_id": run_id,
        "reported_by": caller.subject,
    });
    if let Some(root) = caller.root_event_id {
        runner["root_event_id"] = json!(root);
    }
    let stored = indexer
        .capture_event(NewEvent {
            event_id: Some(event_id),
            // Microsecond `at`: the prune orders snapshots by it, and a
            // burst of reports lands inside one second.
            at: Some(escurel_index::now_rfc3339_micros()),
            source: "report_progress".to_owned(),
            mime: "application/json".to_owned(),
            label_skill: "escurel:run".to_owned(),
            instance_page_id: target,
            title: "run-progress".to_owned(),
            body: body_text,
            provenance: Some(json!({ "runner": runner, "captured_by": caller.subject })),
            kind: EventKind::System,
            root_event_id: caller.root_event_id.map(str::to_owned),
            run_id: Some(run_id.to_owned()),
        })
        .await
        .map_err(|e| JsonRpcError::internal(format!("report_progress: {e}")))?;
    indexer
        .prune_run_progress(run_id, keep)
        .await
        .map_err(|e| JsonRpcError::internal(format!("report_progress prune: {e}")))?;
    let steps = a.plan.len();
    let _ = events_tx.send(std::sync::Arc::new(stored.clone()));
    Ok(json!({
        "ok": true,
        "event_id": stored.event_id,
        "run_id": run_id,
        "steps": steps,
    }))
}

/// How far back to look for the run's `run-started`: a run has a handful
/// of lifecycle events plus at most [`KEEP_SNAPSHOTS`] + 1 progress rows.
const EVENTS_SCAN: usize = KEEP_SNAPSHOTS + 16;
