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
use escurel_runner_core::{
    DispatchConsumer, DispatchQueue, EnqueueOutcome, Ledger, LedgerDecision, RunStatus,
    RunnerConfig, TaskContext, Trigger, package,
};
use escurel_runner_harness::{AdkHarness, ClaudeHarness, CodexHarness, EchoHarness, Harness};
use escurel_types::{Event, ListEventsRequest, ListInboxRequest};
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
    /// The durable run ledger — the idempotency authority (#149). The gate
    /// consults it before enqueueing so a re-delivered event is dropped.
    ledger: Arc<Ledger>,
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

    // The durable run ledger — its own SQLite file, the idempotency
    // authority that survives a process restart (#149). Opening it is
    // fatal: without the ledger the gate cannot enforce effectively-once.
    let ledger = Arc::new(Ledger::open(&config.ledger_path)?);
    tracing::info!(
        target: "escurel_runner",
        path = %config.ledger_path,
        "run ledger opened"
    );

    // The bounded dispatch queue both ingress paths converge on. The
    // consumer side runs the real package→harness→reconcile path (#151) when
    // a tenant + token are configured; without them the runner can't build a
    // gateway client, so it falls back to draining (terminal-marking) the
    // queue so the dedup seen-set still governs convergence.
    let (queue, consumer) = DispatchQueue::new(config.queue_cap, config.seen_cap);
    match (config.tenant.clone(), config.token.clone()) {
        (Some(_), Some(token)) => {
            let harness = build_harness(&config);
            tokio::spawn(dispatch_loop(
                consumer,
                Arc::clone(&ledger),
                config.clone(),
                token,
                harness,
            ));
        }
        _ => {
            tracing::info!(
                target: "escurel_runner",
                "harness dispatch disabled (no tenant/token); draining queue instead"
            );
            tokio::spawn(drain_loop(consumer, Arc::clone(&ledger)));
        }
    }

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
                Arc::clone(&ledger),
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
        ledger,
    };
    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/version", get(move || version_handler(version.clone())))
        .route("/trigger", post(trigger))
        .route("/debug/seen", get(debug_seen))
        .route("/debug/ledger", get(debug_ledger))
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
    // Loop-control gate (lifecycle step 4): the durable ledger is the
    // idempotency authority; the in-memory seen-set is a cheap fast-path in
    // front of it. Either way we acknowledge 202 immediately so the
    // gateway's POST never blocks.
    gate_and_enqueue(&state.ledger, &state.queue, trigger, "webhook");
    StatusCode::ACCEPTED
}

/// The dispatch gate (#149 idempotency half of lifecycle step 4). Consults
/// the **durable run ledger** — the authority that survives a restart —
/// then the in-memory seen-set fast-path:
///
/// - `begin_run` returns [`LedgerDecision::Created`] → a fresh `pending`
///   run exists; enqueue the trigger (the in-memory seen-set collapses any
///   webhook/poll overlap inside the same process).
/// - `AlreadyTerminal` (idempotency) / `InFlight` (dedup) → drop.
///
/// Returns `true` if the trigger was enqueued. Best-effort: a ledger error
/// is logged and the trigger dropped (the poller re-pulls on the next tick),
/// never panicking the process.
fn gate_and_enqueue(ledger: &Ledger, queue: &DispatchQueue, trigger: Trigger, via: &str) -> bool {
    match ledger.begin_run(&trigger) {
        Ok(LedgerDecision::Created(run_id)) => {
            let outcome = queue.enqueue(trigger.clone());
            tracing::info!(
                target: "escurel_runner",
                via,
                tenant = %trigger.tenant,
                event_id = %trigger.event_id,
                run_id = %run_id,
                outcome = ?outcome,
                "gate: run created; trigger enqueued"
            );
            matches!(outcome, EnqueueOutcome::Enqueued)
        }
        Ok(decision) => {
            tracing::debug!(
                target: "escurel_runner",
                via,
                event_id = %trigger.event_id,
                decision = ?decision,
                "gate: dropped re-delivery (idempotency/dedup)"
            );
            false
        }
        Err(e) => {
            tracing::error!(
                target: "escurel_runner",
                via,
                event_id = %trigger.event_id,
                error = %e,
                "gate: ledger error; dropping trigger (poller will retry)"
            );
            false
        }
    }
}

/// Introspection endpoint: the dedup seen-set's `event_id`s as JSON
/// `{"event_ids": [...]}`. A runner ops/observability surface (also the
/// no-mock observable the #148 integration test reads). Read-only; no
/// secrets. Not part of the gateway-facing contract.
async fn debug_seen(State(state): State<AppState>) -> impl IntoResponse {
    let event_ids = state.queue.seen_event_ids();
    axum::Json(serde_json::json!({ "event_ids": event_ids }))
}

/// Introspection endpoint over the **durable run ledger**: per-tenant run
/// counts as JSON `{"total": N, "terminal": M}`. The no-mock #149
/// integration test reads this to assert "exactly one terminal run row"
/// after a doubly-delivered event. Read-only; no secrets. Not part of the
/// gateway-facing contract. The single-tenant runner reports tenant-agnostic
/// totals (`terminal` = all rows that are not `pending`).
async fn debug_ledger(State(state): State<AppState>) -> impl IntoResponse {
    let total = state.ledger.count_all_runs().unwrap_or(0);
    let terminal = total.saturating_sub(
        state
            .ledger
            .count_all_by_status(RunStatus::Pending)
            .unwrap_or(0),
    );
    axum::Json(serde_json::json!({ "total": total, "terminal": terminal }))
}

/// Resolve the absolute path of the `escurel-echo-harness` sibling binary.
/// Deployments ship both binaries side by side, so it lives next to the
/// running `escurel-runner`; fall back to a bare name (`PATH` lookup) if the
/// current-exe directory can't be determined.
fn echo_harness_path() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("escurel-echo-harness")))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "escurel-echo-harness".to_owned())
}

/// Build the configured harness adapter. `echo` is the deterministic real
/// harness (#151); `claude` drives the real Claude Code CLI (#152); `codex`
/// drives the real Codex CLI (#153); `adk` drives an external adk-rust runner
/// binary (#154). Unknown selectors fall back to `echo` with a warning so a
/// typo never silently disables dispatch.
fn build_harness(config: &RunnerConfig) -> Arc<dyn Harness> {
    match config.harness.as_str() {
        "echo" => Arc::new(EchoHarness::new(echo_harness_path())),
        "claude" => Arc::new(
            ClaudeHarness::new(config.claude_bin.clone()).with_model(config.claude_model.clone()),
        ),
        "codex" => Arc::new(
            CodexHarness::new(config.codex_bin.clone()).with_model(config.codex_model.clone()),
        ),
        "adk" => {
            Arc::new(AdkHarness::new(config.adk_bin.clone()).with_model(config.adk_model.clone()))
        }
        other => {
            tracing::warn!(
                target: "escurel_runner",
                selector = %other,
                "unknown ESCUREL_RUNNER_HARNESS; falling back to echo"
            );
            Arc::new(EchoHarness::new(echo_harness_path()))
        }
    }
}

/// The real dispatch loop (lifecycle steps 5-7): consume each `Trigger`,
/// `package` it ("skill body = instructions, `/mcp` = tools"), run the
/// selected `harness` (a real subprocess that makes the escurel writes via
/// its own `/mcp` calls), then **reconcile minimally** — read back that the
/// triggering event is now `processed` on the gateway — and mark the durable
/// ledger run terminal (`processed` on success, `failed` otherwise).
///
/// The full reconciler/retry policy is #155; this keeps the reconcile minimal
/// but REAL: the event genuinely becomes processed through the harness's
/// `/mcp` calls, and the ledger reflects the confirmed outcome.
async fn dispatch_loop(
    mut consumer: DispatchConsumer,
    ledger: Arc<Ledger>,
    config: RunnerConfig,
    token: String,
    harness: Arc<dyn Harness>,
) {
    let client = match Client::connect(&config.gateway_url, SecretString::from(token)).await {
        Ok(client) => client,
        Err(e) => {
            tracing::error!(
                target: "escurel_runner",
                error = %e,
                "dispatch loop could not build a gateway client; dispatch disabled"
            );
            return;
        }
    };
    tracing::info!(
        target: "escurel_runner",
        harness = %harness.name(),
        "harness dispatch loop started"
    );

    while let Some(trigger) = consumer.recv().await {
        let run_id = match ledger.get_run(&trigger.tenant, &trigger.event_id) {
            Ok(Some(record)) => escurel_runner_core::RunId(record.run_id),
            Ok(None) => {
                tracing::warn!(
                    target: "escurel_runner",
                    event_id = %trigger.event_id,
                    "dispatch: no ledger row for trigger; skipping"
                );
                continue;
            }
            Err(e) => {
                tracing::warn!(
                    target: "escurel_runner",
                    event_id = %trigger.event_id,
                    error = %e,
                    "dispatch: ledger lookup failed; skipping"
                );
                continue;
            }
        };

        let status = run_one(&trigger, &client, &config, harness.as_ref()).await;
        if let Err(e) = ledger.mark(&run_id, status) {
            tracing::warn!(
                target: "escurel_runner",
                event_id = %trigger.event_id,
                run_id = %run_id,
                error = %e,
                "dispatch: could not mark run terminal"
            );
        }
    }
}

/// Package + run a single trigger and decide its terminal ledger status.
///
/// Returns [`RunStatus::Processed`] only when the harness ran cleanly **and**
/// the minimal reconcile confirms the triggering event is now `processed` on
/// the gateway; otherwise [`RunStatus::Failed`] (the #155 retry policy may
/// revive it later).
async fn run_one(
    trigger: &Trigger,
    client: &Client,
    config: &RunnerConfig,
    harness: &dyn Harness,
) -> RunStatus {
    let task: TaskContext = match package(trigger, client, config).await {
        Ok(task) => task,
        Err(e) => {
            tracing::warn!(
                target: "escurel_runner",
                event_id = %trigger.event_id,
                error = %e,
                "dispatch: packaging failed"
            );
            return RunStatus::Failed;
        }
    };

    match harness.run(&task).await {
        Ok(outcome) => {
            tracing::info!(
                target: "escurel_runner",
                event_id = %trigger.event_id,
                harness = %harness.name(),
                ok = outcome.ok,
                tool_calls = outcome.tool_calls,
                produced_instance = ?outcome.produced_instance,
                summary = %outcome.summary,
                "dispatch: harness completed"
            );
            if !outcome.ok {
                return RunStatus::Failed;
            }
        }
        Err(e) => {
            tracing::warn!(
                target: "escurel_runner",
                event_id = %trigger.event_id,
                error = %e,
                "dispatch: harness run failed"
            );
            return RunStatus::Failed;
        }
    }

    // Minimal reconcile (#151): confirm the event is now processed on the
    // gateway via the harness's `/mcp` writes. The full reconciler is #155.
    if reconcile_event_processed(client, trigger).await {
        RunStatus::Processed
    } else {
        tracing::warn!(
            target: "escurel_runner",
            event_id = %trigger.event_id,
            "dispatch: harness ran but event not confirmed processed; marking failed"
        );
        RunStatus::Failed
    }
}

/// Read back (over the gateway's own `/mcp`) that the triggering event is now
/// `processed`. For a trigger that targets an instance the event joined that
/// instance's `list_events` history; for an unassigned trigger the harness
/// chose the instance, so we fall back to "the event is no longer in the
/// inbox".
async fn reconcile_event_processed(client: &Client, trigger: &Trigger) -> bool {
    if let Some(instance_page_id) = &trigger.instance_page_id {
        match client
            .list_events(ListEventsRequest {
                instance_page_id: instance_page_id.clone(),
                limit: 100,
            })
            .await
        {
            Ok(resp) => {
                return resp
                    .events
                    .iter()
                    .any(|e| e.event_id == trigger.event_id && e.status == "processed");
            }
            Err(e) => {
                tracing::warn!(
                    target: "escurel_runner",
                    event_id = %trigger.event_id,
                    error = %e,
                    "reconcile: list_events failed"
                );
                return false;
            }
        }
    }

    // No pre-flagged instance: confirm the event left the inbox.
    match client.list_inbox(ListInboxRequest { limit: 100 }).await {
        Ok(resp) => !resp.events.iter().any(|e| e.event_id == trigger.event_id),
        Err(e) => {
            tracing::warn!(
                target: "escurel_runner",
                event_id = %trigger.event_id,
                error = %e,
                "reconcile: list_inbox failed"
            );
            false
        }
    }
}

/// Drain the dispatch queue. A placeholder consumer until the harness
/// dispatcher + reconciler work-items land: it pulls each trigger off and —
/// standing in for the reconciler (lifecycle step 7) — moves the run to a
/// terminal `processed` status in the durable ledger. That terminal row is
/// what makes a later re-delivery of the same event idempotent (#149).
async fn drain_loop(mut consumer: DispatchConsumer, ledger: Arc<Ledger>) {
    while let Some(trigger) = consumer.recv().await {
        match ledger.get_run(&trigger.tenant, &trigger.event_id) {
            Ok(Some(record)) => {
                let run_id = escurel_runner_core::RunId(record.run_id);
                if let Err(e) = ledger.mark(&run_id, RunStatus::Processed) {
                    tracing::warn!(
                        target: "escurel_runner",
                        event_id = %trigger.event_id,
                        error = %e,
                        "drain: could not mark run processed"
                    );
                } else {
                    tracing::debug!(
                        target: "escurel_runner",
                        event_id = %trigger.event_id,
                        run_id = %run_id,
                        "drain: run reconciled (placeholder); marked processed"
                    );
                }
            }
            Ok(None) => tracing::warn!(
                target: "escurel_runner",
                event_id = %trigger.event_id,
                "drain: no ledger row for drained trigger"
            ),
            Err(e) => tracing::warn!(
                target: "escurel_runner",
                event_id = %trigger.event_id,
                error = %e,
                "drain: ledger lookup failed"
            ),
        }
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
    ledger: Arc<Ledger>,
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
                    // Route through the same loop-control gate the webhook
                    // uses: the durable ledger decides create-vs-drop.
                    gate_and_enqueue(&ledger, &queue, trigger, "poll");
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
