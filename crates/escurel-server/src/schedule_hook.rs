//! `POST /hooks/schedule/<skill>` (#827): a signed webhook fires a skill that
//! opts in with `schedule_webhook: true`, as the runner's schedule loop fires
//! one on its cron.
//!
//! - Mounted only when `ESCUREL_SCHEDULE_WEBHOOK_SECRET` is set. The request
//!   carries `X-Escurel-Webhook-Signature: sha256=<hex>`, an HMAC under that
//!   secret over `escurel-schedule-v1\n<skill>\n<delivery>\n<timestamp>\n<body>`
//!   (`escurel_types::webhook_signature::schedule_message`), and
//!   `X-Escurel-Timestamp` (Unix seconds) within ±5 minutes of the gateway's
//!   clock. The signature is checked first, so a caller without the secret
//!   cannot probe which skills exist; binding the delivery id and the time
//!   into it means a captured request cannot be replayed as new deliveries.
//! - `X-Escurel-Delivery` names the delivery; the trigger is
//!   `hook:<skill>:<delivery>`, and first writer wins on it, so a redelivery
//!   is the same event (`200 {replayed: true}`) — and a delivery once skipped
//!   for the budget stays skipped.
//! - `schedule_budget.runs_per_day` counts schedule windows and webhook
//!   deliveries together (`escurel_types::schedule::counts_toward_budget`).
//!   Count and capture are serialised per skill in this (the writer) gateway,
//!   so concurrent deliveries cannot overshoot it. Past it, the day's first
//!   skipped delivery files a `run-skipped` record
//!   (`hook:<skill>:<delivery>:skipped`), as the schedule loop does; later
//!   ones that day are answered `skipped` and not stored, so a flood of
//!   signed deliveries cannot grow the event table.
//!
//! The captured event is an ordinary inbox event: the runner admits and runs
//! it like any other.

use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::{TimeZone, Utc};
use escurel_index::{EventKind, EventListFilter, Indexer, NewEvent, SkillInfo};
use escurel_types::schedule::{HOOK_SOURCE, counts_toward_budget, hook_event_id, is_delivery_id};
use escurel_types::webhook_signature::{SIGNATURE_HEADER, TIMESTAMP_HEADER, schedule_message};
use serde_json::{Value, json};

use crate::server::AppState;

/// The header naming the delivery; required.
pub(crate) const DELIVERY_HEADER: &str = "X-Escurel-Delivery";
/// The largest body a webhook may carry: it becomes the event body.
pub(crate) const MAX_BODY_BYTES: usize = 64 * 1024;
/// The label of the `run-skipped` record (the runner's run-event label).
const RUN_EVENT_LABEL: &str = "escurel:run";
/// How far a delivery's `X-Escurel-Timestamp` may stray from the gateway's clock.
const MAX_CLOCK_SKEW_SECS: i64 = 5 * 60;
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
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let resp = match body {
        Ok(body) => handle(&state, &skill, &headers, &body).await,
        // Before the signature: a body cut at the limit cannot be verified anyway.
        Err(rejection) if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE => refuse(
            StatusCode::PAYLOAD_TOO_LARGE,
            "bad_body",
            "the body is over 64 KiB",
        ),
        Err(rejection) => refuse(StatusCode::BAD_REQUEST, "bad_body", &rejection.body_text()),
    };
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
    let delivery = header(DELIVERY_HEADER).unwrap_or("");
    let Some(timestamp) = header(TIMESTAMP_HEADER) else {
        return refuse(
            StatusCode::UNAUTHORIZED,
            "bad_signature",
            "X-Escurel-Timestamp (Unix seconds) is required and signed",
        );
    };
    let message = schedule_message(skill, delivery, timestamp, body);
    if !escurel_types::webhook_signature::verify(secret, &message, header(SIGNATURE_HEADER)) {
        tracing::warn!(skill = %skill, "schedule webhook refused: missing or invalid signature");
        return refuse(
            StatusCode::UNAUTHORIZED,
            "bad_signature",
            "X-Escurel-Webhook-Signature does not verify over \
             escurel-schedule-v1\\n<skill>\\n<delivery>\\n<timestamp>\\n<body>",
        );
    }
    let fresh = timestamp
        .parse::<i64>()
        .is_ok_and(|t| (Utc::now().timestamp() - t).abs() <= MAX_CLOCK_SKEW_SECS);
    if !fresh {
        tracing::warn!(skill = %skill, timestamp, "schedule webhook refused: stale timestamp");
        return refuse(
            StatusCode::UNAUTHORIZED,
            "stale_timestamp",
            "X-Escurel-Timestamp is more than 5 minutes from the gateway's clock",
        );
    }
    if !is_delivery_id(delivery) {
        return refuse(
            StatusCode::BAD_REQUEST,
            "bad_delivery",
            "X-Escurel-Delivery is required: 1-128 of A-Z a-z 0-9 . _ -",
        );
    }
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

/// One lock per skill: a delivery's budget count and its capture happen under
/// it, so concurrent deliveries cannot all see the same count and overshoot.
/// Per process — this is the writer gateway, the only one that captures.
fn skill_lock(skill: &str) -> std::sync::Arc<tokio::sync::Mutex<()>> {
    static LOCKS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<tokio::sync::Mutex<()>>>>,
    > = std::sync::OnceLock::new();
    LOCKS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .entry(skill.to_owned())
        .or_default()
        .clone()
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
    let Some(info) = indexer.skill(skill).await?.filter(|s| s.schedule_webhook) else {
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
    let lock = skill_lock(skill);
    let _serialised = lock.lock().await;
    if indexer.get_event(&event_id).await?.is_some() {
        return Ok(reply(
            StatusCode::OK,
            json!({ "event_id": event_id, "replayed": true }),
        ));
    }
    // Skipped once, skipped for good: a redelivery after midnight or after the budget was raised
    // was already answered `skipped`, and must not turn into a run now.
    let skipped_id = format!("{event_id}:skipped");
    if indexer.get_event(&skipped_id).await?.is_some() {
        return Ok(reply(
            StatusCode::OK,
            json!({ "event_id": event_id, "skipped": true, "replayed": true, "record": skipped_id }),
        ));
    }
    if let Some(reason) = over_budget(indexer, &info, &event_id).await? {
        // The day's first skip is recorded; later ones are answered and not stored.
        let record = if skipped_today(indexer, skill).await? {
            Value::Null
        } else {
            let stored = indexer
                .capture_event(skipped_record(&info, &event_id, delivery, reason))
                .await?;
            announce(state, indexer, stored);
            json!(skipped_id)
        };
        return Ok(reply(
            StatusCode::OK,
            json!({ "event_id": event_id, "skipped": true, "reason": reason, "record": record }),
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

/// Whether a webhook delivery for `skill` was already recorded as skipped
/// today (UTC).
async fn skipped_today(
    indexer: &Indexer,
    skill: &str,
) -> Result<bool, escurel_index::IndexerError> {
    let day = Utc::now().date_naive();
    let start = Utc.from_utc_datetime(&day.and_time(chrono::NaiveTime::MIN));
    let filter = EventListFilter {
        label_skill: Some(RUN_EVENT_LABEL.to_owned()),
        // `run-skipped` is a system event.
        include_system: true,
        since: Some(start),
        until: Some(start + chrono::Duration::days(1)),
        ..Default::default()
    };
    // `hook:<skill>:` — every delivery of this skill, and only of it.
    let prefix = hook_event_id(skill, "");
    let mut cursor: Option<String> = None;
    loop {
        let page = indexer
            .list_events_filtered_page(&filter, true, BUDGET_PAGE, cursor.as_deref())
            .await?;
        if page.events.iter().any(|e| {
            e.source == HOOK_SOURCE
                && e.event_id.starts_with(&prefix)
                && e.event_id.ends_with(":skipped")
        }) {
            return Ok(true);
        }
        match page.next_cursor {
            Some(next) if Some(&next) != cursor.as_ref() => cursor = Some(next),
            _ => return Ok(false),
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
