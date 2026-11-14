//! Run transcripts (#822): the runner stores what a run's agent did with
//! `put_run_transcript`; a reader replays it with `fetch_blob{run_id}`.
//!
//! - **Write** is the run's own: a run-bound bearer whose `run_id` claim is
//!   the run named, or — for a runner on a static bearer, which has no run
//!   token — a non-machine admin bearer (`is_machine_caller` false: no run,
//!   skill or `act` claim). An agent token for another run is refused, admin
//!   or not. Bytes are capped at [`MAX_TRANSCRIPT_BYTES`] and stored KEYED BY
//!   THE RUN (`transcripts/<run_id>.jsonl`), never in the content-addressed
//!   `blobs/` namespace, and written once: a retried identical upload is a
//!   no-op, different bytes are `transcript_exists`.
//! - **Read** is authorised by the run's `run-finished` event
//!   (`run:<run_id>:finished`, an id only the runner's identity may file):
//!   readable under the event ACL. The bytes come from the run's own key,
//!   never from a blob id the event body names. A run the caller cannot read
//!   and a run that does not exist get the same answer.
//! - **Retention** (`ESCUREL_RUN_TRANSCRIPT_RETENTION_DAYS`): the gateway's
//!   periodic sweep deletes transcripts stored before the cutoff.

use serde::Deserialize;
use serde_json::{Value, json};

use escurel_index::{AclCaller, Indexer};

use super::{JsonRpcError, parse_args};

/// The largest transcript one run may store (2 MiB): a record is a few
/// hundred bytes and the runner keeps at most 4000 of them.
pub(super) const MAX_TRANSCRIPT_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Deserialize)]
pub(super) struct PutRunTranscriptArgs {
    run_id: String,
    /// JSONL, one transcript record per line.
    records: String,
}

pub(super) async fn tool_put_run_transcript(
    indexer: &Indexer,
    caller: AclCaller<'_>,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: PutRunTranscriptArgs = parse_args(args, "put_run_transcript")?;
    if a.run_id.trim().is_empty() {
        return Err(JsonRpcError::invalid_params(
            "put_run_transcript: `run_id` is required".to_owned(),
        ));
    }
    let own_run = caller.run_id == Some(a.run_id.as_str());
    if !own_run && !caller.may_keep_books() {
        return Err(JsonRpcError::forbidden(format!(
            "put_run_transcript: only run `{}`'s own bearer (or the runner's own credential: \
             escurel:runner or escurel:admin) may store its transcript",
            a.run_id
        ))
        .with_code("transcript_not_your_run", false));
    }
    if a.records.len() as u64 > MAX_TRANSCRIPT_BYTES {
        return Err(JsonRpcError::invalid_params(format!(
            "put_run_transcript: {} bytes, over the {MAX_TRANSCRIPT_BYTES}-byte cap",
            a.records.len()
        ))
        .with_code("transcript_too_large", false));
    }
    let records = a.records.lines().filter(|l| !l.trim().is_empty()).count() as u64;
    if let Some((n, _)) = a
        .records
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .find(|(_, l)| !serde_json::from_str::<Value>(l).is_ok_and(|v| v.is_object()))
    {
        return Err(JsonRpcError::invalid_params(format!(
            "put_run_transcript: line {} is not a JSON object (records are JSONL)",
            n + 1
        ))
        .with_code("transcript_malformed", false));
    }
    let stored = indexer
        .store_run_transcript(
            &a.run_id,
            bytes::Bytes::from(a.records.into_bytes()),
            records,
            MAX_TRANSCRIPT_BYTES,
        )
        .await
        .map_err(|e| match e {
            escurel_index::IndexerError::TranscriptExists { .. } => {
                JsonRpcError::invalid_params(format!("put_run_transcript: {e}"))
                    .with_code("transcript_exists", false)
            }
            escurel_index::IndexerError::InvalidTranscriptRunId(_) => {
                JsonRpcError::invalid_params(format!("put_run_transcript: {e}"))
            }
            other => JsonRpcError::internal(format!("put_run_transcript: {other}")),
        })?;
    Ok(json!({
        "records": stored.records,
        "bytes": stored.bytes,
        "sha256": stored.sha256,
    }))
}

/// `run_id`'s transcript bytes for `caller`, through the run's `run-finished`
/// event. A refusal names why: `run_not_found` (absent or not readable —
/// one answer), `transcript_absent`, `transcript_expired`.
pub(super) async fn readable_transcript(
    indexer: &Indexer,
    caller: &AclCaller<'_>,
    event_acl: crate::server::EventAclMode,
    run_id: &str,
) -> Result<bytes::Bytes, JsonRpcError> {
    let not_found = || {
        JsonRpcError::invalid_params(format!(
            "fetch_blob: no finished run `{run_id}` you may read"
        ))
        .with_code("run_not_found", false)
    };
    let Some(finished) = indexer
        .get_event(&format!("run:{run_id}:finished"))
        .await
        .map_err(|e| JsonRpcError::internal(format!("fetch_blob: {e}")))?
        .filter(|e| e.label_skill == "escurel:run" && e.title == "run-finished")
    else {
        return Err(not_found());
    };
    if event_acl != crate::server::EventAclMode::Off {
        let allowed = indexer
            .may_read_event(caller, &finished)
            .await
            .map_err(|e| JsonRpcError::internal(format!("fetch_blob acl: {e}")))?;
        if !allowed {
            if event_acl != crate::server::EventAclMode::Log {
                return Err(not_found());
            }
            tracing::warn!(
                subject = %caller.subject, run_id = %run_id,
                "event-ACL would hide this run's transcript (log mode) — showing"
            );
        }
    }
    if let Some(bytes) = indexer
        .read_run_transcript(run_id)
        .await
        .map_err(|e| JsonRpcError::internal(format!("fetch_blob: {e}")))?
    {
        return Ok(bytes);
    }
    // Nothing stored under the run. The event body only says which refusal fits: a run that
    // recorded a transcript lost it to retention; one that did not never had one.
    let body: Value = serde_json::from_str(&finished.body).unwrap_or(Value::Null);
    if body["transcript"].is_object() {
        Err(JsonRpcError::invalid_params(format!(
            "fetch_blob: run `{run_id}`'s transcript is past its retention"
        ))
        .with_code("transcript_expired", false))
    } else {
        Err(JsonRpcError::invalid_params(format!(
            "fetch_blob: run `{run_id}` finished without a transcript"
        ))
        .with_code("transcript_absent", false))
    }
}

/// Delete transcripts past their retention: the served tenant's own
/// `transcript_retention_days` when its spec sets one, else
/// `ESCUREL_RUN_TRANSCRIPT_RETENTION_DAYS`; a no-op when they are kept forever.
pub(crate) async fn sweep_run_transcripts(state: &crate::server::AppState) {
    let Some(retention) = transcript_retention(state).await else {
        return;
    };
    let Some(indexer) = state
        .indexer
        .as_ref()
        .map(escurel_index::IndexerHandle::current)
    else {
        return;
    };
    let Some(cutoff) = std::time::SystemTime::now().checked_sub(retention) else {
        return;
    };
    match indexer.sweep_run_transcripts(cutoff).await {
        Ok(0) => {}
        Ok(n) => tracing::info!(swept = n, "run transcripts past their retention deleted"),
        Err(e) => tracing::warn!(error = %e, "run transcript sweep failed"),
    }
}

/// The retention the sweep applies. The tenant spec is read on every tick, so
/// a `tenant_update` applies without a restart; an unreadable spec falls back
/// to the gateway's value rather than skipping the sweep.
async fn transcript_retention(state: &crate::server::AppState) -> Option<std::time::Duration> {
    let tenant_days = match (state.tenant_store.as_ref(), state.served_tenant.as_deref()) {
        (Some(store), Some(tenant)) => match store.get(tenant).await {
            Ok(spec) => spec.and_then(|s| s.transcript_retention_days),
            Err(e) => {
                tracing::warn!(error = %e, "transcript sweep: tenant spec unreadable; using the gateway retention");
                None
            }
        },
        _ => None,
    };
    match tenant_days {
        Some(0) => None,
        Some(days) => Some(std::time::Duration::from_secs(u64::from(days) * 86_400)),
        None => state.run_transcript_retention,
    }
}
