//! `POST /hooks/schedule/<skill>` (#827): a signed webhook fires a skill that
//! opts in with `schedule_webhook: true`, as the runner's schedule loop fires
//! one on its cron.
//!
//! - Mounted only when `ESCUREL_SCHEDULE_WEBHOOK_SECRET` is set; the body must
//!   carry `X-Escurel-Webhook-Signature: sha256=<hex>` over its raw bytes
//!   under that secret (the scheme the runner's `/trigger` verifies). The
//!   signature is checked first, so a caller without the secret cannot probe
//!   which skills exist.
//! - `X-Escurel-Delivery` names the delivery; the trigger is
//!   `hook:<skill>:<delivery>`, and first writer wins on it, so a redelivery
//!   is the same event (`200 {replayed: true}`).
//! - `schedule_budget.runs_per_day` counts schedule windows and webhook
//!   deliveries together (`escurel_types::schedule::counts_toward_budget`);
//!   past it, a `run-skipped` record (`hook:<skill>:<delivery>:skipped`) is
//!   filed instead, as the schedule loop does.
//!
//! The captured event is an ordinary inbox event: the runner admits and runs
//! it like any other.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::{TimeZone, Utc};
use escurel_index::{EventKind, EventListFilter, Indexer, NewEvent, SkillInfo};
use escurel_types::schedule::{HOOK_SOURCE, counts_toward_budget, hook_event_id, is_delivery_id};
use serde_json::{Value, json};

use crate::server::AppState;

/// The header naming the delivery; required.
pub(crate) const DELIVERY_HEADER: &str = "X-Escurel-Delivery";
/// The largest body a webhook may carry: it becomes the event body.
pub(crate) const MAX_BODY_BYTES: usize = 64 * 1024;
/// The label of the `run-skipped` record (the runner's run-event label).
const RUN_EVENT_LABEL: &str = "escurel:run";
/// Page size when counting a day's triggers.
const BUDGET_PAGE: usize = 200;
/// A schedule trigger may be captured up to the runner's catch-up grace after
/// its window opens; the count reads this far into the next day so a late
/// capture of today's window is not missed.
const LATE_CAPTURE_SLACK: chrono::Duration = chrono::Duration::hours(1);

fn reply(status: StatusCode, body: Value) -> Response {
    (status, Json(body)).into_response()
}

fn refuse(status: StatusCode, code: &str, message: &str) -> Response {
    reply(status, json!({ "error": code, "message": message }))
}

pub(crate) async fn schedule_hook(
    State(state): State<AppState>,
    Path(skill): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let resp = handle(&state, &skill, &headers, &body).await;
    state
        .metrics
        .inc_request("/hooks/schedule", resp.status().as_u16());
    resp
}

async fn handle(state: &AppState, skill: &str, headers: &HeaderMap, body: &[u8]) -> Response {
    let Some(secret) = state.schedule_webhook_secret.as_deref() else {
        return refuse(
            StatusCode::NOT_FOUND,
            "not_found",
            "no schedule webhook here",
        );
    };
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    if !escurel_types::webhook_signature::verify(
        secret,
        body,
        header(escurel_types::webhook_signature::SIGNATURE_HEADER),
    ) {
        tracing::warn!(skill = %skill, "schedule webhook refused: missing or invalid signature");
        return refuse(
            StatusCode::UNAUTHORIZED,
            "bad_signature",
            "the body's X-Escurel-Webhook-Signature does not verify",
        );
    }
    let Some(delivery) = header(DELIVERY_HEADER).filter(|d| is_delivery_id(d)) else {
        return refuse(
            StatusCode::BAD_REQUEST,
            "bad_delivery",
            "X-Escurel-Delivery is required: 1-128 of A-Z a-z 0-9 . _ -",
        );
    };
    let Ok(text) = std::str::from_utf8(body) else {
        return refuse(StatusCode::BAD_REQUEST, "bad_body", "the body is not UTF-8");
    };
    if state.reader_mode {
        return refuse(
            StatusCode::SERVICE_UNAVAILABLE,
            "read_only_replica",
            "this replica is a ducklake reader; send the webhook to the writer",
        );
    }
    if state
        .tenant_suspended
        .load(std::sync::atomic::Ordering::Relaxed)
    {
        return refuse(
            StatusCode::FORBIDDEN,
            "tenant_suspended",
            "the tenant is suspended",
        );
    }
    let Some(indexer) = state
        .indexer
        .as_ref()
        .map(escurel_index::IndexerHandle::current)
    else {
        return refuse(
            StatusCode::SERVICE_UNAVAILABLE,
            "no_indexer",
            "no indexer wired",
        );
    };
    if let Some(resp) = crate::server::quarantine_refusal(&indexer) {
        return resp;
    }
    match fire(state, &indexer, skill, delivery, text, headers).await {
        Ok(resp) => resp,
        Err(e) => {
            tracing::warn!(skill = %skill, error = %e, "schedule webhook failed");
            refuse(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "the trigger could not be stored",
            )
        }
    }
}

async fn fire(
    state: &AppState,
    indexer: &Indexer,
    skill: &str,
    delivery: &str,
    text: &str,
    headers: &HeaderMap,
) -> Result<Response, escurel_index::IndexerError> {
    // Absent and not opted in answer alike: the endpoint says nothing about a
    // skill it would not fire.
    let Some(info) = indexer
        .list_skills()
        .await?
        .into_iter()
        .find(|s| s.id == skill && s.schedule_webhook)
    else {
        return Ok(refuse(
            StatusCode::NOT_FOUND,
            "skill_not_triggerable",
            "no skill with `schedule_webhook: true` has this id",
        ));
    };
    if info.backend.kind == escurel_index::BackendKind::Workflow {
        return Ok(refuse(
            StatusCode::UNPROCESSABLE_ENTITY,
            "workflow_not_supported",
            "a workflow skill cannot be fired by a webhook yet; its `schedule:` still fires it",
        ));
    }
    let event_id = hook_event_id(skill, delivery);
    if indexer.get_event(&event_id).await?.is_some() {
        return Ok(reply(
            StatusCode::OK,
            json!({ "event_id": event_id, "replayed": true }),
        ));
    }
    if let Some(reason) = over_budget(indexer, &info, &event_id).await? {
        let skipped = skipped_record(&info, &event_id, delivery, reason);
        let id = skipped.event_id.clone();
        let stored = indexer.capture_event(skipped).await?;
        announce(state, indexer, stored);
        return Ok(reply(
            StatusCode::OK,
            json!({ "event_id": event_id, "skipped": true, "reason": reason, "record": id }),
        ));
    }
    let json_body = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("application/json"));
    let stored = indexer
        .capture_event(NewEvent {
            event_id: Some(event_id.clone()),
            at: Some(escurel_index::now_rfc3339_micros()),
            source: HOOK_SOURCE.to_owned(),
            mime: if json_body {
                "application/json"
            } else {
                "text/plain"
            }
            .to_owned(),
            label_skill: skill.to_owned(),
            instance_page_id: None,
            title: format!("webhook {skill}"),
            body: text.to_owned(),
            provenance: Some(json!({
                "webhook": { "delivery": delivery },
                "captured_by": "escurel:schedule-webhook",
            })),
            kind: EventKind::User,
            root_event_id: None,
            run_id: None,
        })
        .await?;
    announce(state, indexer, stored);
    tracing::info!(skill = %skill, event_id = %event_id, "schedule webhook fired");
    Ok(reply(StatusCode::ACCEPTED, json!({ "event_id": event_id })))
}

/// Why `info`'s budget refuses another trigger today, if it does.
async fn over_budget(
    indexer: &Indexer,
    info: &SkillInfo,
    own: &str,
) -> Result<Option<&'static str>, escurel_index::IndexerError> {
    let Some(budget) = info.schedule_budget else {
        return Ok(None);
    };
    if budget.invalid {
        return Ok(Some("invalid_budget"));
    }
    let now = Utc::now();
    let day = now.date_naive();
    let start = Utc.from_utc_datetime(&day.and_time(chrono::NaiveTime::MIN));
    let filter = EventListFilter {
        label_skill: Some(info.id.clone()),
        since: Some(start),
        until: Some(start + chrono::Duration::days(1) + LATE_CAPTURE_SLACK),
        ..Default::default()
    };
    let mut count = 0;
    let mut cursor: Option<String> = None;
    loop {
        let page = indexer
            .list_events_filtered_page(&filter, true, BUDGET_PAGE, cursor.as_deref())
            .await?;
        count += page
            .events
            .iter()
            .filter(|e| {
                e.event_id != own
                    && counts_toward_budget(&info.id, &e.event_id, &e.source, e.at.as_deref(), day)
            })
            .count();
        if count >= budget.runs_per_day as usize {
            return Ok(Some("budget"));
        }
        match page.next_cursor {
            Some(next) if Some(&next) != cursor.as_ref() => cursor = Some(next),
            _ => return Ok(None),
        }
    }
}

/// The `run-skipped` record filed in place of a trigger past the budget, in
/// the shape the schedule loop files (`reason`, `runs_per_day`, `skill`,
/// `trigger_event_id`), with the delivery in place of the window.
fn skipped_record(info: &SkillInfo, trigger: &str, delivery: &str, reason: &str) -> NewEvent {
    NewEvent {
        event_id: Some(format!("{trigger}:skipped")),
        at: Some(escurel_index::now_rfc3339_micros()),
        source: HOOK_SOURCE.to_owned(),
        mime: "application/json".to_owned(),
        label_skill: RUN_EVENT_LABEL.to_owned(),
        instance_page_id: None,
        title: "run-skipped".to_owned(),
        body: json!({
            "reason": reason,
            "runs_per_day": info.schedule_budget.map_or(0, |b| b.runs_per_day),
            "skill": info.id,
            "trigger_event_id": trigger,
            "delivery": delivery,
        })
        .to_string(),
        provenance: Some(json!({ "webhook": { "delivery": delivery } })),
        kind: EventKind::System,
        root_event_id: None,
        run_id: None,
    }
}

/// Tell the runner and live subscribers, as `capture_event` does.
fn announce(state: &AppState, indexer: &Indexer, stored: escurel_index::EventInfo) {
    crate::mcp::announce_captured(state.webhook.as_ref(), &state.events_tx, indexer, stored);
}
