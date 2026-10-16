//! `report_runner_status` — a runner's health as ONE overwritten record.
//!
//! A runner used to file an `escurel:runner-status` event per tick; on a
//! shared Postgres table that is a stream whose only useful row is the
//! newest (the lab's reached ~58k rows and every poll read them). The runner
//! now reports here instead: the gateway overwrites the single row
//! `runner-status:<runner_id>` (see [`Indexer::upsert_runner_status`]), so
//! the table stops growing and the label's newest-first read still finds the
//! record first.
//!
//! The same bookkeeper gate as a reserved-label `capture_event`: the
//! runner's own admin identity, not a run-bound token (the runner mints its
//! harnesses admin run tokens, so `is_admin` alone is not enough). The tool
//! announces nothing: no webhook and no WS fan-out, because nothing acts on
//! a heartbeat.

use escurel_index::{AclCaller, Indexer, IndexerError};
use serde::Deserialize;
use serde_json::{Value, json};

use super::tools_write::{is_machine_caller, stamp_captured_by};
use super::{JsonRpcError, parse_args};

/// The runner's state at the moment of the report.
const STATES: [&str; 4] = ["started", "changed", "heartbeat", "stopping"];
/// A snapshot is a few hundred bytes; this bounds a misbehaving caller.
const MAX_BODY_BYTES: usize = 64 * 1024;

#[derive(Deserialize)]
pub(super) struct ReportRunnerStatusArgs {
    runner_id: String,
    state: String,
    status_interval_s: u64,
    #[serde(default)]
    started_at: Option<String>,
    #[serde(default)]
    last_change_at: Option<String>,
    /// The runner's snapshot, a JSON object as text (the `capture_event`
    /// body the heartbeat carried).
    body: String,
}

pub(super) async fn tool_report_runner_status(
    indexer: &Indexer,
    caller: AclCaller<'_>,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: ReportRunnerStatusArgs = parse_args(args, "report_runner_status")?;
    if !caller.is_admin || is_machine_caller(&caller) {
        return Err(JsonRpcError::invalid_params(
            "report_runner_status: a runner's status is written by the runner's own admin \
             identity; a caller files events with `capture_event`"
                .to_owned(),
        ));
    }
    if !STATES.contains(&a.state.as_str()) {
        return Err(JsonRpcError::invalid_params(format!(
            "report_runner_status: `state` must be one of {STATES:?}, got `{}`",
            a.state
        )));
    }
    if a.status_interval_s == 0 {
        return Err(JsonRpcError::invalid_params(
            "report_runner_status: `status_interval_s` is the reporting interval in seconds (> 0)"
                .to_owned(),
        ));
    }
    if a.body.len() > MAX_BODY_BYTES {
        return Err(JsonRpcError::invalid_params(format!(
            "report_runner_status: `body` is at most {MAX_BODY_BYTES} bytes"
        )));
    }
    let Ok(Value::Object(mut body)) = serde_json::from_str::<Value>(&a.body) else {
        return Err(JsonRpcError::invalid_params(
            "report_runner_status: `body` must be a JSON object (as text)".to_owned(),
        ));
    };
    // The report's own fields are authoritative over what the snapshot says.
    body.insert("runner_id".to_owned(), json!(a.runner_id));
    body.insert("state".to_owned(), json!(a.state));
    body.insert("status_interval_s".to_owned(), json!(a.status_interval_s));
    if let Some(t) = a.started_at {
        body.insert("started_at".to_owned(), json!(t));
    }
    if let Some(t) = a.last_change_at {
        body.insert("last_change_at".to_owned(), json!(t));
    }
    let provenance = stamp_captured_by(
        Some(json!({ "runner_status": { "runner_id": a.runner_id, "tenant": indexer.tenant() } })),
        caller.subject,
        caller.actor,
    )
    .unwrap_or_else(|| json!({}));
    let stored = indexer
        .upsert_runner_status(
            &a.runner_id,
            None,
            &a.state,
            &Value::Object(body).to_string(),
            &provenance,
        )
        .await
        .map_err(|e| match e {
            IndexerError::InvalidRunnerStatus(m) => {
                JsonRpcError::invalid_params(format!("report_runner_status: {m}"))
            }
            other => JsonRpcError::internal(format!("report_runner_status: {other}")),
        })?;
    Ok(json!({
        "event_id": stored.event.event_id,
        "created": stored.created,
        "at": stored.event.at,
    }))
}
