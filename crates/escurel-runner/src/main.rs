//! The deployable `escurel-runner` process.
//!
//! This skeleton (#145) loads [`RunnerConfig`] from the environment,
//! installs the substrate JSON-log contract via `escurel-obs`, and
//! serves a dependency-free `GET /healthz` (liveness) + `GET /version`
//! on the configured listener, draining gracefully on SIGTERM / Ctrl-C.
//!
//! The inbox poller, dispatch queue, and harness dispatch arrive in
//! later work-items of the `escurel-agent-runner` epic (see
//! `docs/contract/agent-orchestration.md`). This issue (#146) adds the
//! `POST /trigger` webhook listener: it parses the gateway's serialized
//! `Event`, normalises it into a `Trigger`, and returns `202` without
//! blocking (the gateway has a 5s timeout) — the dispatch queue lands
//! in #148.

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use escurel_obs::{TelemetryConfig, init_telemetry};
use escurel_runner_core::{RunnerConfig, Trigger};
use escurel_types::Event;

/// Header carrying the gateway's shared webhook secret (#147 adds the
/// authoritative `tenant_id`; the secret is the ingress trust anchor).
const WEBHOOK_SECRET_HEADER: &str = "X-Escurel-Webhook-Secret";

/// Header carrying the tenant for an inbound webhook. Provisional: the
/// gateway POST does not send a tenant today; #147 wires the
/// authoritative value into the payload/header. Until then the listener
/// reads this header if present, else falls back to a configured/empty
/// default.
const TENANT_HEADER: &str = "X-Escurel-Tenant";

/// Shared listener state. Cheap to clone (only the optional secret).
#[derive(Clone)]
struct AppState {
    /// Optional shared secret required on `POST /trigger`.
    webhook_secret: Option<Arc<str>>,
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

    let version = config.version.clone();
    let state = AppState {
        webhook_secret: config.webhook_secret.clone().map(Arc::from),
    };
    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/version", get(move || version_handler(version.clone())))
        .route("/trigger", post(trigger))
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

/// Webhook listener (lifecycle step 2→3). Verifies the optional shared
/// secret, parses the gateway's serialized `Event`, normalises it into a
/// `Trigger`, hands it off (logged for now — the dispatch queue is
/// #148), and returns `202 Accepted` immediately so the gateway's POST
/// never blocks.
///
/// `HeaderMap` is extracted before `Json` so the secret can be checked;
/// note `Json` still parses the body first, so a malformed body yields
/// `400`/`422` regardless of the secret. Auth is enforced for the
/// common (well-formed) path, which is the gateway's only path.
async fn trigger(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(event): Json<Event>,
) -> StatusCode {
    if let Some(expected) = state.webhook_secret.as_deref() {
        let presented = headers
            .get(WEBHOOK_SECRET_HEADER)
            .and_then(|v| v.to_str().ok());
        if presented != Some(expected) {
            tracing::warn!(
                target: "escurel_runner",
                "POST /trigger rejected: missing or mismatched webhook secret"
            );
            return StatusCode::UNAUTHORIZED;
        }
    }

    // Tenant identity is provisional until #147 wires the authoritative
    // `tenant_id` into the gateway POST: read `X-Escurel-Tenant` if the
    // sender supplied it, else an empty tenant.
    let tenant = headers
        .get(TENANT_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();

    let trigger = Trigger::from_event(&event, tenant);
    // Hand-off point for #148's bounded dispatch queue; for now we log
    // the normalised trigger's fields and acknowledge immediately.
    tracing::info!(
        target: "escurel_runner",
        tenant = %trigger.tenant,
        event_id = %trigger.event_id,
        label_skill = %trigger.label_skill,
        instance_page_id = ?trigger.instance_page_id,
        root_event_id = %trigger.lineage.root_event_id,
        depth = trigger.lineage.depth,
        "POST /trigger accepted; trigger normalised"
    );

    StatusCode::ACCEPTED
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
