//! The deployable `escurel-runner` process.
//!
//! This skeleton (#145) loads [`RunnerConfig`] from the environment,
//! installs the substrate JSON-log contract via `escurel-obs`, and
//! serves a dependency-free `GET /healthz` (liveness) + `GET /version`
//! on the configured listener, draining gracefully on SIGTERM / Ctrl-C.
//!
//! The inbox poller, dispatch queue, and harness dispatch arrive in
//! later work-items of the `escurel-agent-runner` epic (see
//! `docs/contract/agent-orchestration.md`). #146 added the `POST
//! /trigger` webhook listener; #147 hardens its ingress: the shared
//! secret is now an **HMAC-SHA256 signature over the raw request body**
//! (header `X-Escurel-Webhook-Signature: sha256=<hex>`), verified on the
//! raw bytes *before* JSON parsing, and the authoritative `tenant_id` is
//! read from the payload (the gateway stamps it). The listener parses the
//! gateway's serialized `Event`, normalises it into a `Trigger`, and
//! returns `202` without blocking (the gateway has a 5s timeout).
//!
//! #148 adds the **bounded dispatch queue** ([`DispatchQueue`]) and the
//! **inbox poller**. Both the webhook handler and the poller enqueue onto
//! the *same* queue; a shared dedup seen-set collapses the overlap
//! (effectively-once processing over at-least-once delivery). The poller
//! is the self-healing fallback for missed webhooks: every
//! `ESCUREL_RUNNER_POLL_INTERVAL` it calls `list_inbox` on the gateway and
//! enqueues each event. A small `GET /debug/seen` introspection endpoint
//! exposes the seen-set so ops (and the no-mock integration test) can
//! observe the queue's effect; the harness-side consumer arrives in a
//! later work-item, so for now a drain task empties the queue.

use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use escurel_client::{Client, SecretString};
use escurel_obs::{TelemetryConfig, init_telemetry};
use escurel_runner_core::{DispatchConsumer, DispatchQueue, EnqueueOutcome, RunnerConfig, Trigger};
use escurel_types::{Event, ListInboxRequest};
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Header carrying the gateway's HMAC-SHA256 signature of the raw POST
/// body, in the form `sha256=<lowercase-hex>` (#147). The secret is the
/// ingress trust anchor; verifying the signature over the raw bytes
/// before parsing fixes the earlier extractor-ordering flag.
const WEBHOOK_SIGNATURE_HEADER: &str = "X-Escurel-Webhook-Signature";

/// Shared listener state. Cheap to clone (an `Arc`-backed secret + a
/// cloneable dispatch-queue producer handle).
#[derive(Clone)]
struct AppState {
    /// Optional shared secret required on `POST /trigger`. When `Some`,
    /// the request must carry a valid HMAC-SHA256 signature of the body.
    webhook_secret: Option<Arc<str>>,
    /// The bounded dispatch queue both ingress paths converge on.
    queue: DispatchQueue,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = RunnerConfig::from_env()?;

    // Hold the telemetry guard for the whole process lifetime so the
    // OTLP exporter (if any) is flushed on shutdown. `init_telemetry`
    // installs a process-global subscriber; errors here are fatal.
    let _telemetry = init_telemetry(TelemetryConfig {
        app: "escurel-runner".to_owned(),
        env: config.env.clone(),
        version: config.version.clone(),
        otlp_endpoint: std::env::var("ESCUREL_OTLP_ENDPOINT").ok(),
        json_logs: true,
    })?;

    // The bounded dispatch queue both ingress paths converge on. The
    // consumer side is drained here until the harness dispatcher (a later
    // work-item) takes it over; draining keeps the channel from filling so
    // the dedup seen-set — not channel backpressure — governs convergence.
    let (queue, consumer) = DispatchQueue::new(config.queue_cap, config.seen_cap);
    tokio::spawn(drain_loop(consumer));

    // The inbox poller: the self-healing fallback for missed webhooks.
    // Enabled only when both a tenant and a token are configured.
    match (config.tenant.clone(), config.token.clone()) {
        (Some(tenant), Some(token)) => {
            tokio::spawn(poll_loop(
                config.gateway_url.clone(),
                tenant,
                token,
                config.poll_interval,
                queue.clone(),
            ));
        }
        _ => {
            tracing::info!(
                target: "escurel_runner",
                "inbox poller disabled: set ESCUREL_RUNNER_TENANT + ESCUREL_RUNNER_TOKEN to enable"
            );
        }
    }

    let version = config.version.clone();
    let state = AppState {
        webhook_secret: config.webhook_secret.clone().map(Arc::from),
        queue,
    };
    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/version", get(move || version_handler(version.clone())))
        .route("/trigger", post(trigger))
        .route("/debug/seen", get(debug_seen))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(config.listen).await?;
    let local_addr = listener.local_addr()?;
    tracing::info!(addr = %local_addr, "escurel-runner listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(wait_for_shutdown())
        .await?;

    tracing::info!("escurel-runner shut down cleanly");
    Ok(())
}

/// Liveness probe. Dependency-free per CLAUDE.md principle 4.
async fn healthz() -> impl IntoResponse {
    (StatusCode::OK, "OK")
}

/// Reports the build version string.
async fn version_handler(version: String) -> impl IntoResponse {
    (StatusCode::OK, version)
}

/// Webhook listener (lifecycle step 2→3). Verifies the optional HMAC
/// signature **over the raw request body bytes** (before any JSON
/// parsing), then parses the gateway's serialized `Event`, normalises it
/// into a `Trigger` (with the authoritative `tenant_id` read from the
/// payload), hands it off (logged for now — the dispatch queue is #148),
/// and returns `202 Accepted` immediately so the gateway's POST never
/// blocks.
///
/// The body is extracted as raw `Bytes` so the signature is verified on
/// exactly what the gateway signed. When no secret is configured (dev),
/// no signature is required.
async fn trigger(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> StatusCode {
    // 1. Authenticate the raw body BEFORE parsing it (#147).
    if let Some(secret) = state.webhook_secret.as_deref() {
        let presented = headers
            .get(WEBHOOK_SIGNATURE_HEADER)
            .and_then(|v| v.to_str().ok());
        if !verify_signature(secret, &body, presented) {
            tracing::warn!(
                target: "escurel_runner",
                "POST /trigger rejected: missing or invalid webhook signature"
            );
            return StatusCode::UNAUTHORIZED;
        }
    }

    // 2. Parse the authenticated bytes as the gateway's serialized event.
    let event: Event = match serde_json::from_slice(&body) {
        Ok(event) => event,
        Err(e) => {
            tracing::warn!(
                target: "escurel_runner",
                error = %e,
                "POST /trigger rejected: malformed event body"
            );
            return StatusCode::BAD_REQUEST;
        }
    };

    // 3. The authoritative tenant rides in the payload (#147). Read it
    //    from the raw JSON (it is not a field of `Event`); fall back to
    //    empty when absent (dev / legacy senders).
    let tenant = serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| {
            v.get("tenant_id")
                .and_then(|t| t.as_str())
                .map(str::to_owned)
        })
        .unwrap_or_default();

    let trigger = Trigger::from_event(&event, tenant);
    // Converge onto the shared dispatch queue (#148). Dedup collapses any
    // event the poller already delivered. Either way we acknowledge 202
    // immediately so the gateway's POST never blocks.
    let outcome = state.queue.enqueue(trigger.clone());
    tracing::info!(
        target: "escurel_runner",
        tenant = %trigger.tenant,
        event_id = %trigger.event_id,
        label_skill = %trigger.label_skill,
        outcome = ?outcome,
        "POST /trigger accepted; trigger enqueued"
    );

    StatusCode::ACCEPTED
}

/// Introspection endpoint: the dedup seen-set's `event_id`s as JSON
/// `{"event_ids": [...]}`. A runner ops/observability surface (also the
/// no-mock observable the #148 integration test reads). Read-only; no
/// secrets. Not part of the gateway-facing contract.
async fn debug_seen(State(state): State<AppState>) -> impl IntoResponse {
    let event_ids = state.queue.seen_event_ids();
    axum::Json(serde_json::json!({ "event_ids": event_ids }))
}

/// Drain the dispatch queue. A placeholder consumer until the harness
/// dispatcher work-item lands: it pulls each trigger off so the bounded
/// channel keeps draining (the dedup seen-set, not channel backpressure,
/// is what governs the webhook/poll convergence under test).
async fn drain_loop(mut consumer: DispatchConsumer) {
    while let Some(trigger) = consumer.recv().await {
        tracing::debug!(
            target: "escurel_runner",
            event_id = %trigger.event_id,
            "dispatch queue: drained trigger (harness dispatch is a later work-item)"
        );
    }
}

/// The inbox poller (lifecycle step 2 backstop). Every `interval` it calls
/// `list_inbox` on the gateway with a tenant-scoped bearer, normalises each
/// `Event` into a `Trigger`, and enqueues it. Dedup collapses anything a
/// webhook already delivered. Best-effort: a failed poll is logged and the
/// next tick retries — the poller's whole job is to be the self-healing
/// fallback, so it must never panic the process.
async fn poll_loop(
    gateway_url: String,
    tenant: String,
    token: String,
    interval: std::time::Duration,
    queue: DispatchQueue,
) {
    let client = match Client::connect(&gateway_url, SecretString::from(token)).await {
        Ok(client) => client,
        Err(e) => {
            tracing::error!(
                target: "escurel_runner",
                error = %e,
                "inbox poller could not build a gateway client; poller disabled"
            );
            return;
        }
    };
    tracing::info!(
        target: "escurel_runner",
        gateway = %gateway_url,
        tenant = %tenant,
        interval_ms = interval.as_millis() as u64,
        "inbox poller started"
    );

    let mut ticker = tokio::time::interval(interval);
    loop {
        ticker.tick().await;
        match client.list_inbox(ListInboxRequest::default()).await {
            Ok(resp) => {
                for event in &resp.events {
                    let trigger = Trigger::from_event(event, tenant.clone());
                    match queue.enqueue(trigger) {
                        EnqueueOutcome::Enqueued => tracing::debug!(
                            target: "escurel_runner",
                            event_id = %event.event_id,
                            "poller enqueued inbox event"
                        ),
                        EnqueueOutcome::Duplicate => tracing::trace!(
                            target: "escurel_runner",
                            event_id = %event.event_id,
                            "poller skipped already-seen event (dedup)"
                        ),
                        EnqueueOutcome::Full => tracing::warn!(
                            target: "escurel_runner",
                            event_id = %event.event_id,
                            "dispatch queue full; dropping poll (next tick retries)"
                        ),
                    }
                }
            }
            Err(e) => tracing::warn!(
                target: "escurel_runner",
                error = %e,
                "inbox poll failed; will retry next tick"
            ),
        }
    }
}

/// Verify a `sha256=<hex>` HMAC-SHA256 signature over `body` under
/// `secret`. Returns `false` for a missing/malformed header or any
/// mismatch. The compare is constant-time via `Mac::verify_slice`.
fn verify_signature(secret: &str, body: &[u8], presented: Option<&str>) -> bool {
    let Some(presented) = presented else {
        return false;
    };
    let Some(hex) = presented.strip_prefix("sha256=") else {
        return false;
    };
    let Some(expected) = decode_hex(hex) else {
        return false;
    };
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts a key of any size");
    mac.update(body);
    mac.verify_slice(&expected).is_ok()
}

/// Decode a lowercase/uppercase hex string into bytes. Returns `None`
/// for odd length or any non-hex digit.
fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

/// Block until SIGTERM (Nomad's graceful-stop signal) or SIGINT
/// (Ctrl-C in a dev shell). On non-unix targets, only Ctrl-C.
#[cfg(unix)]
async fn wait_for_shutdown() {
    use tokio::signal::unix::{SignalKind, signal};

    let mut sigterm = signal(SignalKind::terminate()).expect("install SIGTERM handler");
    let mut sigint = signal(SignalKind::interrupt()).expect("install SIGINT handler");
    tokio::select! {
        _ = sigterm.recv() => {}
        _ = sigint.recv() => {}
    }
}

#[cfg(not(unix))]
async fn wait_for_shutdown() {
    let _ = tokio::signal::ctrl_c().await;
}
