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
use escurel_obs::{Metrics, TelemetryConfig, init_telemetry};
use escurel_runner_core::{
    Admission, CascadeOutcome, ConfirmedEffect, DispatchConsumer, DispatchQueue, EnqueueOutcome,
    Governor, Ledger, LedgerDecision, LoopLimits, QuotaDecision, QuotaLimits, ReconcileError,
    RunFailure, RunStatus, RunnerConfig, TaskContext, Trigger, admit, classify_client_error,
    confirm_effect, drive_workflow, emit_cascade, package, recover_pending, recover_workflows,
    run_with_retry,
};
use escurel_runner_core::{DeadLetterReason, RunId};
use escurel_runner_harness::{AdkHarness, ClaudeHarness, CodexHarness, EchoHarness, Harness};
use escurel_types::{CaptureEventRequest, Event, ListInboxRequest};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use tokio::sync::Notify;

type HmacSha256 = Hmac<Sha256>;

/// In-flight quota slots keyed by `event_id` (#158). A slot is held for a
/// run's whole lifetime (queue → run → terminal) so the tenant's concurrency
/// budget reflects real in-flight work, not just the instant of admission.
type InflightSlots =
    Arc<std::sync::Mutex<std::collections::HashMap<String, escurel_runner_core::RunSlot>>>;

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
    /// The loop-control limits (#157) the gate enforces after idempotency:
    /// depth cap + per-root run budget. A trigger that would breach them is
    /// dead-lettered (with `cycle` checked against the lineage instance chain).
    limits: LoopLimits,
    /// The quota governor (#158): per-tenant runs/min + max-concurrent gates
    /// at admission. Over-quota triggers are throttled (held, not
    /// dead-lettered) so the event stays in the inbox for the poller backstop.
    governor: Governor,
    /// In-flight quota slots, keyed by `event_id`. The gate inserts a slot on
    /// admission (debiting the tenant's concurrency budget) and the dispatch
    /// loop removes it when the run terminates (releasing the budget). Held
    /// here so the slot's lifetime spans queue → run, not just the gate call.
    inflight: InflightSlots,
    /// The metrics registry rendered at `GET /metrics` (#158).
    metrics: Arc<Metrics>,
    /// Set once shutdown begins: the ingress paths stop admitting new triggers
    /// while in-flight runs drain.
    draining: Arc<std::sync::atomic::AtomicBool>,
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

    // The loop-control limits (#157) the dispatch gate enforces after
    // idempotency: depth cap + per-root run budget (cycle is checked against
    // the lineage instance chain, needing no limit).
    let limits = LoopLimits {
        max_depth: config.max_depth,
        max_runs_per_root: config.max_runs_per_root,
    };

    // The quota governor (#158): per-tenant runs/min + max-concurrent gates,
    // plus the global harness-subprocess semaphore. Shared between the
    // admission gate (rate/concurrency) and the dispatch loop (harness cap).
    let governor = Governor::new(QuotaLimits {
        runs_per_min: config.tenant_runs_per_min,
        max_concurrent: config.tenant_max_concurrent,
        max_harness_procs: config.max_harness_procs,
    });

    // The metrics registry rendered at /metrics (#158).
    let metrics = Arc::new(Metrics::new());
    metrics.set_up(true);

    // Drain flag: shutdown sets it so ingress stops admitting new triggers
    // while in-flight runs finish.
    let draining = Arc::new(std::sync::atomic::AtomicBool::new(false));

    // In-flight quota slots, shared gate → dispatch loop (#158).
    let inflight: InflightSlots = Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));

    // Crash recovery (#158): before opening for traffic, reconcile any
    // orphaned `pending` rows left by a previous crash. A confirmed effect is
    // marked processed; an unconfirmed row is reset to retriable so the poller
    // backstops it. Best-effort, bounded; only runs with a gateway client.
    if let (Some(_), Some(token)) = (config.tenant.clone(), config.token.clone()) {
        match Client::connect(&config.gateway_url, SecretString::from(token)).await {
            Ok(client) => {
                let report = recover_pending(&ledger, &client).await;
                if report.swept > 0 {
                    tracing::info!(
                        target: "escurel_runner",
                        swept = report.swept,
                        confirmed = report.confirmed,
                        reset = report.reset,
                        "crash recovery: reconciled orphaned pending runs on startup"
                    );
                }
                // Workflow-aware recovery: re-invoke the reducer for every
                // non-terminal workflow-run so a crash mid-barrier resumes from
                // KB state (§3.6 keys keep re-emission idempotent).
                match recover_workflows(&client, config.max_runs_per_root).await {
                    Ok(resumed) if resumed > 0 => tracing::info!(
                        target: "escurel_runner",
                        resumed,
                        "crash recovery: re-drove non-terminal workflow runs on startup"
                    ),
                    Ok(_) => {}
                    Err(e) => tracing::warn!(
                        target: "escurel_runner",
                        error = %e,
                        "crash recovery: workflow re-drive failed (non-fatal)"
                    ),
                }
            }
            Err(e) => tracing::warn!(
                target: "escurel_runner",
                error = %e,
                "crash recovery: could not build gateway client; skipping pending sweep"
            ),
        }
    }

    // The bounded dispatch queue both ingress paths converge on. The
    // consumer side runs the real package→harness→reconcile path (#151) when
    // a tenant + token are configured; without them the runner can't build a
    // gateway client, so it falls back to draining (terminal-marking) the
    // queue so the dedup seen-set still governs convergence.
    let (queue, consumer) = DispatchQueue::new(config.queue_cap, config.seen_cap);
    // Notified once the dispatch loop observes the queue closed AND finished
    // its in-flight run — the drain-complete signal SIGTERM waits on.
    let drained = Arc::new(Notify::new());
    match (config.tenant.clone(), config.token.clone()) {
        (Some(_), Some(token)) => {
            let harness = build_harness(&config);
            tokio::spawn(dispatch_loop(
                consumer,
                Arc::clone(&ledger),
                config.clone(),
                token,
                harness,
                governor.clone(),
                Arc::clone(&metrics),
                Arc::clone(&inflight),
                Arc::clone(&drained),
            ));
        }
        _ => {
            tracing::info!(
                target: "escurel_runner",
                "harness dispatch disabled (no tenant/token); draining queue instead"
            );
            let drained = Arc::clone(&drained);
            let ledger = Arc::clone(&ledger);
            tokio::spawn(async move {
                drain_loop(consumer, ledger).await;
                drained.notify_one();
            });
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
                limits,
                governor.clone(),
                Arc::clone(&metrics),
                Arc::clone(&inflight),
                Arc::clone(&draining),
            ));
        }
        _ => {
            tracing::info!(
                target: "escurel_runner",
                "inbox poller disabled: set ESCUREL_RUNNER_TENANT + ESCUREL_RUNNER_TOKEN to enable"
            );
        }
    }

    // The lint tick (compile-first-wiki G2): opt-in scheduled semantic-health
    // pass. Every `lint_interval` the runner synthesizes a `lint` invocation
    // with a deterministic per-window id so the reactive loop drives it exactly
    // once per window. Disabled unless ESCUREL_RUNNER_LINT_INTERVAL is set.
    match (
        config.lint_interval,
        config.tenant.clone(),
        config.token.clone(),
    ) {
        (Some(interval), Some(tenant), Some(token)) => {
            tokio::spawn(lint_tick_loop(
                config.gateway_url.clone(),
                tenant,
                token,
                interval,
                Arc::clone(&draining),
            ));
        }
        (Some(_), _, _) => tracing::warn!(
            target: "escurel_runner",
            "lint tick disabled: ESCUREL_RUNNER_LINT_INTERVAL set but tenant/token missing"
        ),
        _ => {}
    }

    let version = config.version.clone();
    let state = AppState {
        webhook_secret: config.webhook_secret.clone().map(Arc::from),
        queue: queue.clone(),
        ledger,
        limits,
        governor,
        metrics: Arc::clone(&metrics),
        inflight: Arc::clone(&inflight),
        draining: Arc::clone(&draining),
    };
    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/version", get(move || version_handler(version.clone())))
        .route("/metrics", get(metrics_handler))
        .route("/trigger", post(trigger))
        .route("/dlq", get(dlq_list))
        .route("/dlq/requeue", post(dlq_requeue))
        .route("/debug/seen", get(debug_seen))
        .route("/debug/ledger", get(debug_ledger))
        .route("/debug/run", get(debug_run))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(config.listen).await?;
    let local_addr = listener.local_addr()?;
    tracing::info!(addr = %local_addr, "escurel-runner listening");

    // Graceful shutdown (#158): on SIGTERM/SIGINT, stop the HTTP server from
    // accepting new connections AND flip the drain flag so the poller stops
    // enqueuing. Then drop the producer-side queue handle so the dispatch loop
    // sees the channel close, lets its current run finish, and signals
    // `drained` — bounded by the configured drain timeout.
    let drain_timeout = config.drain_timeout;
    axum::serve(listener, app)
        .with_graceful_shutdown(wait_for_shutdown(Arc::clone(&draining)))
        .await?;

    tracing::info!(
        target: "escurel_runner",
        "shutdown signalled; draining in-flight runs"
    );
    // Closing the producer side lets the dispatch loop's `recv()` return None
    // once its current run completes. The router (and its `AppState` clone of
    // the queue) was dropped when `serve` returned; the poller drops its clone
    // on the drain flag; this drops the last local one.
    drop(queue);
    let drain = tokio::time::timeout(drain_timeout, drained.notified()).await;
    match drain {
        Ok(()) => tracing::info!(target: "escurel_runner", "in-flight runs drained cleanly"),
        Err(_) => tracing::warn!(
            target: "escurel_runner",
            timeout_ms = drain_timeout.as_millis() as u64,
            "drain timeout elapsed; exiting (any still-pending run recovers on restart)"
        ),
    }

    tracing::info!("escurel-runner shut down cleanly");
    Ok(())
}

/// Render the Prometheus metrics registry (#158).
async fn metrics_handler(State(state): State<AppState>) -> impl IntoResponse {
    (
        StatusCode::OK,
        [("content-type", "text/plain; version=0.0.4")],
        state.metrics.render_prometheus(),
    )
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
    // 0. Shutdown drain (#158): stop admitting new triggers while draining so
    //    the event stays in the inbox for the next process to re-drive.
    if state.draining.load(std::sync::atomic::Ordering::Relaxed) {
        return StatusCode::SERVICE_UNAVAILABLE;
    }
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
    gate_and_enqueue(
        &state.ledger,
        &state.queue,
        &state.limits,
        &state.governor,
        &state.metrics,
        &state.inflight,
        trigger,
        "webhook",
    );
    StatusCode::ACCEPTED
}

/// The dispatch gate (lifecycle step 4). Consults the **durable run
/// ledger** — the authority that survives a restart — for idempotency
/// (#149), then enforces the **loop controls** (#157), then the in-memory
/// seen-set fast-path:
///
/// - `begin_run` returns [`LedgerDecision::Created`] → a fresh `pending` run
///   exists. Run the loop-control [`admit`] gate: if it denies (depth/cycle/
///   budget), **dead-letter** the just-created run with the reason and do NOT
///   enqueue — the cascade stops here. Otherwise enqueue the trigger (the
///   in-memory seen-set collapses any webhook/poll overlap).
/// - `AlreadyTerminal` (idempotency — `processed`/`dead_letter`) / `InFlight`
///   (dedup) → drop. (A prior `failed` run is re-claimed as `Created`.)
///
/// Returns `true` if the trigger was enqueued. Best-effort: a ledger error
/// is logged and the trigger dropped (the poller re-pulls on the next tick),
/// never panicking the process.
#[allow(clippy::too_many_arguments)]
fn gate_and_enqueue(
    ledger: &Ledger,
    queue: &DispatchQueue,
    limits: &LoopLimits,
    governor: &Governor,
    metrics: &Metrics,
    inflight: &InflightSlots,
    trigger: Trigger,
    via: &str,
) -> bool {
    match ledger.begin_run(&trigger) {
        Ok(LedgerDecision::Created(run_id)) => {
            // Loop controls: depth/cycle/budget. The `pending` row already
            // exists (idempotency), so a denial dead-letters THAT row — making
            // it idempotency-terminal so a re-delivery of the same event drops.
            match admit(&trigger, limits, ledger) {
                Ok(Admission::DeadLetter(reason)) => {
                    if let Err(e) = ledger.dead_letter(&run_id, reason) {
                        tracing::error!(
                            target: "escurel_runner",
                            via,
                            event_id = %trigger.event_id,
                            error = %e,
                            "gate: could not record dead-letter"
                        );
                    }
                    record_run_terminal(metrics, &trigger.tenant, "dead_letter");
                    tracing::warn!(
                        target: "escurel_runner",
                        via,
                        tenant = %trigger.tenant,
                        event_id = %trigger.event_id,
                        run_id = %run_id,
                        reason = %reason,
                        depth = trigger.lineage.depth,
                        root_event_id = %trigger.lineage.root_event_id,
                        "gate: run dead-lettered by loop control; cascade stopped"
                    );
                    return false;
                }
                Err(e) => {
                    // A ledger read failed mid-gate: leave the row pending and
                    // drop; the poller re-pulls and re-evaluates next tick.
                    tracing::error!(
                        target: "escurel_runner",
                        via,
                        event_id = %trigger.event_id,
                        error = %e,
                        "gate: loop-control check errored; dropping (poller retries)"
                    );
                    return false;
                }
                Ok(Admission::Admit) => {}
            }

            // Quota gate (#158): per-tenant runs/min + max-concurrent. An
            // over-quota trigger is THROTTLED — held, NOT dead-lettered. We
            // reset the just-created row to retriable `failed` so the poller
            // re-claims the still-inbox event next cycle (a `failed` row is not
            // idempotency-terminal, #157); the event itself stays in the inbox.
            match governor.try_admit(&trigger.tenant) {
                (QuotaDecision::Admit, Some(slot)) => {
                    inflight
                        .lock()
                        .expect("inflight slots mutex")
                        .insert(trigger.event_id.clone(), slot);
                }
                (QuotaDecision::Throttle(reason), _) => {
                    if let Err(e) = ledger.mark(&run_id, RunStatus::Failed) {
                        tracing::error!(
                            target: "escurel_runner",
                            via,
                            event_id = %trigger.event_id,
                            error = %e,
                            "gate: could not reset throttled run to retriable"
                        );
                    }
                    metrics.inc_runner_throttled(reason.as_str());
                    tracing::warn!(
                        target: "escurel_runner",
                        via,
                        tenant = %trigger.tenant,
                        event_id = %trigger.event_id,
                        reason = %reason.as_str(),
                        throttled_total = governor.throttled_total(),
                        "gate: trigger throttled by quota; held for the poller to re-drive"
                    );
                    return false;
                }
                (QuotaDecision::Admit, None) => return false,
            }

            let outcome = queue.enqueue(trigger.clone());
            // If the trigger did not actually reach the channel (a duplicate
            // already in flight, or backpressure), release the quota slot we
            // just took — the run won't dispatch under this slot. A `Full`
            // trigger is reset to retriable so the poller re-drives it.
            if !matches!(outcome, EnqueueOutcome::Enqueued) {
                inflight
                    .lock()
                    .expect("inflight slots mutex")
                    .remove(&trigger.event_id);
                if matches!(outcome, EnqueueOutcome::Full) {
                    let _ = ledger.mark(&run_id, RunStatus::Failed);
                }
            }
            tracing::info!(
                target: "escurel_runner",
                via,
                tenant = %trigger.tenant,
                event_id = %trigger.event_id,
                run_id = %run_id,
                outcome = ?outcome,
                "gate: run created + admitted; trigger enqueued"
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

/// Record a run reaching a terminal `status` on the metrics registry (#158).
/// Keeps cardinality sane: only tenant + status labels.
fn record_run_terminal(metrics: &Metrics, tenant: &str, status: &str) {
    metrics.inc_runner_run(tenant, status);
}

/// Operator DLQ list (#158): every dead-lettered run with its reason +
/// originating event/instance. An ops/debug surface (like `/debug/*`), not
/// part of the gateway-facing contract.
async fn dlq_list(State(state): State<AppState>) -> impl IntoResponse {
    match state.ledger.list_dead_letters() {
        Ok(rows) => {
            let entries: Vec<_> = rows
                .into_iter()
                .map(|r| {
                    serde_json::json!({
                        "run_id": r.run_id,
                        "tenant": r.tenant,
                        "event_id": r.event_id,
                        "instance_page_id": r.instance_page_id,
                        "produced_instance_page_id": r.produced_instance_page_id,
                        "reason": r.reason,
                    })
                })
                .collect();
            (
                StatusCode::OK,
                axum::Json(serde_json::json!({ "dead_letters": entries })),
            )
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(serde_json::json!({ "error": e.to_string() })),
        ),
    }
}

/// Operator DLQ requeue (#158): body `{ "run_id": "..." }` or `{ "tenant":
/// "...", "event_id": "..." }`. Clears the dead-letter terminal block so the
/// originating (still-inbox) event can be re-driven, and re-enqueues a fresh
/// trigger so the runner picks it up immediately (the poller would too).
async fn dlq_requeue(
    State(state): State<AppState>,
    axum::Json(body): axum::Json<serde_json::Value>,
) -> impl IntoResponse {
    let requeued = if let Some(run_id) = body.get("run_id").and_then(|v| v.as_str()) {
        state.ledger.requeue_dead_letter(run_id)
    } else if let (Some(tenant), Some(event_id)) = (
        body.get("tenant").and_then(|v| v.as_str()),
        body.get("event_id").and_then(|v| v.as_str()),
    ) {
        state
            .ledger
            .requeue_dead_letter_by_event(tenant, event_id)
            .map(|_| (tenant.to_owned(), event_id.to_owned()))
    } else {
        return (
            StatusCode::BAD_REQUEST,
            axum::Json(serde_json::json!({
                "error": "provide run_id, or tenant + event_id"
            })),
        );
    };

    match requeued {
        Ok((tenant, event_id)) => {
            // Re-enqueue a fresh trigger directly so the runner re-drives the
            // event immediately. The ledger row is now `pending` (re-claimed),
            // so we enqueue onto the dispatch queue under a fresh quota slot.
            let trigger = Trigger {
                tenant: tenant.clone(),
                event_id: event_id.clone(),
                label_skill: String::new(),
                instance_page_id: None,
                lineage: escurel_runner_core::Lineage::root(event_id.clone()),
                workflow: None,
            };
            // The row is already pending; enqueue onto the queue and take a
            // quota slot so the dispatch loop runs it.
            match state.governor.try_admit(&tenant) {
                (QuotaDecision::Admit, Some(slot)) => {
                    state
                        .inflight
                        .lock()
                        .expect("inflight slots mutex")
                        .insert(event_id.clone(), slot);
                    let _ = state.queue.enqueue(trigger);
                }
                _ => {
                    // Over quota right now: the poller will re-drive it.
                }
            }
            tracing::info!(
                target: "escurel_runner",
                tenant = %tenant,
                event_id = %event_id,
                "dlq: requeued dead-lettered run; cleared terminal block"
            );
            (
                StatusCode::OK,
                axum::Json(serde_json::json!({
                    "requeued": true,
                    "tenant": tenant,
                    "event_id": event_id,
                })),
            )
        }
        Err(e) => (
            StatusCode::NOT_FOUND,
            axum::Json(serde_json::json!({ "error": e.to_string() })),
        ),
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
    // `succeeded` = runs recorded `processed` (the confirmed-effect terminal
    // status #155 records). The #155 integration test reads this to assert a
    // run converged to success after the transient failure cleared.
    let succeeded = state
        .ledger
        .count_all_by_status(RunStatus::Processed)
        .unwrap_or(0);
    let failed = state
        .ledger
        .count_all_by_status(RunStatus::Failed)
        .unwrap_or(0);
    // `dead_letter` = runs blocked by a loop control (#157). The no-mock
    // integration test reads this to assert the cascade was stopped.
    let dead_letter = state
        .ledger
        .count_all_by_status(RunStatus::DeadLetter)
        .unwrap_or(0);
    axum::Json(serde_json::json!({
        "total": total,
        "terminal": terminal,
        "succeeded": succeeded,
        "failed": failed,
        "dead_letter": dead_letter,
    }))
}

/// Introspection endpoint over a single ledger run row, keyed by
/// `?tenant=<t>&event_id=<e>`. Returns the run's terminal status plus the
/// produced instance + its confirmed version (the #155 read-back result), so
/// the no-mock integration test can assert the run was recorded `succeeded`
/// WITH the produced instance + version straight from the real sqlite ledger.
/// Read-only; no secrets. Not part of the gateway-facing contract.
async fn debug_run(
    State(state): State<AppState>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let tenant = params.get("tenant").map(String::as_str).unwrap_or("");
    let event_id = params.get("event_id").map(String::as_str).unwrap_or("");
    match state.ledger.get_run(tenant, event_id) {
        Ok(Some(rec)) => (
            StatusCode::OK,
            axum::Json(serde_json::json!({
                "run_id": rec.run_id,
                "tenant": rec.tenant,
                "event_id": rec.event_id,
                "status": rec.status.as_str(),
                "instance_page_id": rec.produced_instance_page_id,
                "produced_version": rec.produced_version,
                // The loop-control dead-letter reason (#157), when dead-lettered.
                "reason": rec.reason,
            })),
        ),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            axum::Json(serde_json::json!({ "error": "run not found" })),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(serde_json::json!({ "error": e.to_string() })),
        ),
    }
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
#[allow(clippy::too_many_arguments)]
async fn dispatch_loop(
    mut consumer: DispatchConsumer,
    ledger: Arc<Ledger>,
    config: RunnerConfig,
    token: String,
    harness: Arc<dyn Harness>,
    governor: Governor,
    metrics: Arc<Metrics>,
    inflight: InflightSlots,
    drained: Arc<Notify>,
) {
    let client = match Client::connect(&config.gateway_url, SecretString::from(token)).await {
        Ok(client) => client,
        Err(e) => {
            tracing::error!(
                target: "escurel_runner",
                error = %e,
                "dispatch loop could not build a gateway client; dispatch disabled"
            );
            drained.notify_one();
            return;
        }
    };
    tracing::info!(
        target: "escurel_runner",
        harness = %harness.name(),
        "harness dispatch loop started"
    );

    while let Some(mut trigger) = consumer.recv().await {
        // Queue-depth observability (#158): sample after pulling this trigger.
        metrics.set_runner_queue_depth(0);
        // Cascade-depth high-water (#158).
        metrics.observe_runner_cascade_depth(trigger.lineage.depth as i64);

        let run_id = match ledger.get_run(&trigger.tenant, &trigger.event_id) {
            Ok(Some(record)) => RunId(record.run_id),
            Ok(None) => {
                tracing::warn!(
                    target: "escurel_runner",
                    event_id = %trigger.event_id,
                    "dispatch: no ledger row for trigger; skipping"
                );
                inflight
                    .lock()
                    .expect("inflight slots mutex")
                    .remove(&trigger.event_id);
                continue;
            }
            Err(e) => {
                tracing::warn!(
                    target: "escurel_runner",
                    event_id = %trigger.event_id,
                    error = %e,
                    "dispatch: ledger lookup failed; skipping"
                );
                inflight
                    .lock()
                    .expect("inflight slots mutex")
                    .remove(&trigger.event_id);
                continue;
            }
        };

        // One OTel trace per cascade lineage (#158): the ROOT hop mints a
        // trace id; deeper hops carry it forward via `provenance.runner`. The
        // run's root span uses this id, and the cascade emitter stamps the same
        // id onto the next hop's event so hop N+1 continues the SAME trace.
        if trigger.lineage.trace_id.is_none() {
            trigger.lineage.trace_id = Some(mint_trace_id());
        }
        let trace_id = trigger.lineage.trace_id.clone().unwrap_or_default();
        let run_span = tracing::info_span!(
            "runner.run",
            trace_id = %trace_id,
            root_event_id = %trigger.lineage.root_event_id,
            event_id = %trigger.event_id,
            depth = trigger.lineage.depth,
        );
        let _run_guard = run_span.enter();

        // Acquire a global harness-subprocess permit (#158): bounds concurrent
        // harness spawns across all tenants. Held across the whole run.
        let _harness_permit = governor.acquire_harness().await;

        // Reconcile with retry: package + run the harness + read back over
        // `/mcp` to CONFIRM the effect, retrying transient failures with
        // backoff up to the attempts cap (#155).
        let report = run_with_retry(&config, |attempt| {
            attempt_run(&trigger, &client, &config, harness.as_ref(), attempt)
        })
        .await;

        // Outcome → terminal status:
        // - confirmed effect      → `processed` (+ produced instance/version);
        // - clean no-op (converged) → `processed` with no produced instance —
        //   a converged cascade hop ends tidily, NOT `failed` (#156/#157);
        // - retries exhausted / bad output → `dead_letter` (#158), terminal;
        // - otherwise (permanent)  → `failed` (retriable; operator may re-drive).
        let result = match (&report.confirmed, report.converged_no_op, report.failure) {
            (Some(effect), _, _) => ledger.complete(
                &run_id,
                RunStatus::Processed,
                Some((effect.instance_page_id.as_str(), effect.version.as_str())),
            ),
            (None, true, _) => ledger.complete(&run_id, RunStatus::Processed, None),
            (None, false, Some(RunFailure::RetriesExhausted)) => {
                record_run_terminal(&metrics, &trigger.tenant, "dead_letter");
                ledger.dead_letter(&run_id, DeadLetterReason::RetriesExhausted)
            }
            (None, false, Some(RunFailure::BadOutput)) => {
                record_run_terminal(&metrics, &trigger.tenant, "dead_letter");
                ledger.dead_letter(&run_id, DeadLetterReason::BadOutput)
            }
            (None, false, _) => ledger.complete(&run_id, RunStatus::Failed, None),
        };
        if report.confirmed.is_none() && report.converged_no_op {
            record_run_terminal(&metrics, &trigger.tenant, "converged");
            tracing::info!(
                target: "escurel_runner",
                event_id = %trigger.event_id,
                run_id = %run_id,
                attempts = report.attempts,
                "dispatch: run was a clean no-op; recorded processed (converged, no cascade)"
            );
        }
        // Release the in-flight quota slot now this run reached a terminal.
        inflight
            .lock()
            .expect("inflight slots mutex")
            .remove(&trigger.event_id);
        match (&report.confirmed, result) {
            (Some(effect), Ok(())) => {
                record_run_terminal(&metrics, &trigger.tenant, "processed");
                tracing::info!(
                    target: "escurel_runner",
                    event_id = %trigger.event_id,
                    run_id = %run_id,
                    attempts = report.attempts,
                    instance = %effect.instance_page_id,
                    version = %effect.version,
                    "dispatch: run succeeded; recorded processed with produced instance + version"
                );
                // Dynamic workflows: a confirmed write whose trigger carries a
                // `provenance.workflow` block drives the reducer instead of the
                // cascade — the cascade is the width-≤1 special case, the
                // reducer the general one. It emits the plan's next batch of
                // step events (each a §3.6-idempotent, lineage-tagged
                // `capture_event`), guarded by the same `admit` controls.
                if trigger.workflow.is_some() {
                    match drive_workflow(
                        &client,
                        &trigger,
                        &run_id.0,
                        effect,
                        config.max_runs_per_root,
                    )
                    .await
                    {
                        Ok(outcome) => tracing::info!(
                            target: "escurel_runner",
                            event_id = %trigger.event_id,
                            run_id = %run_id,
                            emitted = outcome.emitted.len(),
                            "workflow: reducer emitted next-step events"
                        ),
                        Err(e) => tracing::warn!(
                            target: "escurel_runner",
                            event_id = %trigger.event_id,
                            error = %e,
                            "workflow: reducer pass failed (run already recorded processed)"
                        ),
                    }
                    continue;
                }
                // The "change → event" bridge (#156): a CONFIRMED successful
                // write may cascade a follow-on event describing the change.
                // The cascade decides (cross-skill change only) and tags the
                // emitted event with lineage; the new event re-enters the SAME
                // poll → trigger → package → harness → reconcile pipeline.
                // Fired only here — after a confirmed success — so a failed or
                // converged-no-op run never spuriously emits.
                match emit_cascade(&client, &trigger, &run_id.0, effect).await {
                    Ok(CascadeOutcome::Emitted {
                        event_id,
                        label_skill,
                    }) => tracing::info!(
                        target: "escurel_runner",
                        parent_event_id = %trigger.event_id,
                        parent_run_id = %run_id,
                        cascaded_event_id = %event_id,
                        label_skill = %label_skill,
                        depth = trigger.lineage.depth + 1,
                        "cascade: emitted lineage-tagged follow-on event"
                    ),
                    Ok(CascadeOutcome::NotCrossSkill) => tracing::debug!(
                        target: "escurel_runner",
                        event_id = %trigger.event_id,
                        "cascade: confirmed write is not a cross-skill change; no follow-on"
                    ),
                    Err(e) => tracing::warn!(
                        target: "escurel_runner",
                        event_id = %trigger.event_id,
                        error = %e,
                        "cascade: failed to emit follow-on event (run already recorded processed)"
                    ),
                }
            }
            (None, Ok(())) => {
                // Not confirmed, not converged: a dead-letter (retries/bad
                // output, already metered above) or a retriable `failed`.
                match report.failure {
                    Some(RunFailure::RetriesExhausted) => tracing::warn!(
                        target: "escurel_runner",
                        event_id = %trigger.event_id,
                        run_id = %run_id,
                        attempts = report.attempts,
                        reason = "retries_exhausted",
                        "dispatch: retries exhausted; dead-lettered (event left in inbox)"
                    ),
                    Some(RunFailure::BadOutput) => tracing::warn!(
                        target: "escurel_runner",
                        event_id = %trigger.event_id,
                        run_id = %run_id,
                        reason = "bad_output",
                        "dispatch: unparseable harness output; dead-lettered (event left in inbox)"
                    ),
                    _ => {
                        record_run_terminal(&metrics, &trigger.tenant, "failed");
                        tracing::warn!(
                            target: "escurel_runner",
                            event_id = %trigger.event_id,
                            run_id = %run_id,
                            attempts = report.attempts,
                            "dispatch: permanent failure; recorded failed (retriable re-drive)"
                        );
                    }
                }
            }
            (_, Err(e)) => tracing::warn!(
                target: "escurel_runner",
                event_id = %trigger.event_id,
                run_id = %run_id,
                error = %e,
                "dispatch: could not record run outcome"
            ),
        }
    }

    // The producer side closed (graceful shutdown): no more triggers and the
    // current run finished. Signal the drain-complete so SIGTERM can exit 0.
    tracing::info!(
        target: "escurel_runner",
        "dispatch loop drained (queue closed); signalling shutdown"
    );
    drained.notify_one();
}

/// Mint a fresh W3C-style trace id: 32 lowercase hex chars (128 bits). A
/// cascade-wide identifier shared by every hop of a lineage (#158).
fn mint_trace_id() -> String {
    let bits: u128 = ulid::Ulid::new().into();
    format!("{bits:032x}")
}

/// One full reconciler attempt (#155): package the trigger, run the harness,
/// then **read back over `/mcp`** to confirm the effect. Returns the
/// [`ConfirmedEffect`] on success, or a classified [`ReconcileError`] the
/// retry loop uses to decide retry-vs-fail-fast.
///
/// Classification of this attempt's failures:
/// - a packaging `/mcp` read failure → classified via
///   [`classify_client_error`] (transport/5xx transient, 4xx/protocol
///   permanent);
/// - an adapter-level harness error (`Spawn`/`Timeout`/`Io`) → **transient**
///   (a flapping subprocess/host may recover);
/// - a `NonZeroExit` or `BadOutcome` → **permanent** (the harness is broken
///   in a way a re-run won't fix);
/// - a clean-but-`Failed` harness outcome → **permanent** (it ran and decided
///   it could not do the work);
/// - read-back not yet converged → **transient** (the idempotent
///   `assign_event`/`update_page` re-run can finish a partial success).
async fn attempt_run(
    trigger: &Trigger,
    client: &Client,
    config: &RunnerConfig,
    harness: &dyn Harness,
    attempt: u32,
) -> Result<ConfirmedEffect, ReconcileError> {
    let task: TaskContext = package(trigger, client, config).await.map_err(|e| {
        tracing::warn!(
            target: "escurel_runner",
            event_id = %trigger.event_id,
            attempt,
            error = %e,
            "dispatch: packaging failed"
        );
        package_error_to_reconcile(e)
    })?;

    match harness.run(&task).await {
        Ok(outcome) => {
            tracing::info!(
                target: "escurel_runner",
                event_id = %trigger.event_id,
                harness = %harness.name(),
                attempt,
                ok = outcome.ok,
                tool_calls = outcome.tool_calls,
                produced_instance = ?outcome.produced_instance,
                summary = %outcome.summary,
                "dispatch: harness completed"
            );
            if !outcome.ok {
                // The harness ran cleanly but reported it could not complete
                // — re-running won't change that, so fail fast.
                return Err(ReconcileError::Permanent(format!(
                    "harness {} reported a failed outcome: {}",
                    harness.name(),
                    outcome.summary
                )));
            }
            // Clean no-op: the harness ran fine but produced no instance AND
            // the trigger had no pre-flagged target. This is a *converged*
            // cascade hop (e.g. an unassigned cascaded event the echo-harness
            // can't bind) — there is genuinely nothing to confirm. Terminate
            // CLEANLY (#156/#157) rather than burning retries on a read-back
            // that can never converge and recording `failed`. We only treat
            // the genuinely-nothing-to-do case (ok + no produced instance + no
            // pre-flagged target) as converged, so a real failure is never
            // masked.
            if outcome.produced_instance.is_none() && trigger.instance_page_id.is_none() {
                return Err(ReconcileError::Converged(format!(
                    "harness {} had nothing to do: {}",
                    harness.name(),
                    outcome.summary
                )));
            }
        }
        Err(e) => {
            tracing::warn!(
                target: "escurel_runner",
                event_id = %trigger.event_id,
                attempt,
                error = %e,
                "dispatch: harness run failed"
            );
            return Err(harness_error_to_reconcile(&e));
        }
    }

    // Don't trust the harness: read back over `/mcp` to confirm the event is
    // processed + bound and the instance's version advanced (#155).
    confirm_effect(client, trigger).await
}

/// Map a packaging error to a reconcile classification. A `/mcp` read failure
/// is classified by its underlying client error; the other variants (skill
/// not found, missing token) are permanent — a re-run can't conjure a missing
/// skill or token.
fn package_error_to_reconcile(e: escurel_runner_core::PackageError) -> ReconcileError {
    match e {
        escurel_runner_core::PackageError::Client { source, .. } => classify_client_error(&source),
        other => ReconcileError::Permanent(other.to_string()),
    }
}

/// Map an adapter-level harness error to a reconcile classification. Spawn /
/// timeout / I/O are transient (the host/subprocess may recover); a non-zero
/// exit is permanent; **unparseable output** is its own `BadOutput` variant so
/// the dispatch loop dead-letters it `bad_output` (#158).
fn harness_error_to_reconcile(e: &escurel_runner_harness::HarnessError) -> ReconcileError {
    use escurel_runner_harness::HarnessError as H;
    match e {
        H::Spawn { .. } | H::Timeout { .. } | H::Io { .. } => {
            ReconcileError::Transient(e.to_string())
        }
        H::BadOutcome { .. } => ReconcileError::BadOutput(e.to_string()),
        H::NonZeroExit { .. } => ReconcileError::Permanent(e.to_string()),
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
#[allow(clippy::too_many_arguments)]
async fn poll_loop(
    gateway_url: String,
    tenant: String,
    token: String,
    interval: std::time::Duration,
    queue: DispatchQueue,
    ledger: Arc<Ledger>,
    limits: LoopLimits,
    governor: Governor,
    metrics: Arc<Metrics>,
    inflight: InflightSlots,
    draining: Arc<std::sync::atomic::AtomicBool>,
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
        // Stop pulling new work once shutdown drain begins so the dispatch
        // loop's queue can close and in-flight runs finish. Dropping the
        // poller's queue clone here lets the channel reach `None`.
        if draining.load(std::sync::atomic::Ordering::Relaxed) {
            tracing::info!(
                target: "escurel_runner",
                "inbox poller stopping (drain); releasing queue handle"
            );
            return;
        }
        match client.list_inbox(ListInboxRequest::default()).await {
            Ok(resp) => {
                for event in &resp.events {
                    let trigger = Trigger::from_event(event, tenant.clone());
                    // Route through the same loop-control + quota gate the
                    // webhook uses: the durable ledger decides create-vs-drop,
                    // the depth/cycle/budget controls admit-or-dead-letter, and
                    // the quota gate throttles (holds) an over-quota trigger.
                    gate_and_enqueue(
                        &ledger, &queue, &limits, &governor, &metrics, &inflight, trigger, "poll",
                    );
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

/// The deterministic per-window lint invocation id: same `(tenant, window)`
/// ⇒ same id, so a mid-window restart or overlapping tick collapses via
/// `capture_event`'s `ON CONFLICT DO NOTHING` — at most one lint run per
/// window. `window = floor(epoch_secs / interval_secs)`.
fn lint_window_id(tenant: &str, window: u64) -> String {
    format!("lint-{tenant}-{window}")
}

/// The lint tick (compile-first-wiki G2). Every `interval` it synthesizes a
/// `lint` workflow invocation — a `capture_event` the runner sends to the
/// gateway, which re-enters the runner's own dispatch via the inbox/webhook.
/// The gateway stays automation-free: the *runner* owns the decision to act.
/// Best-effort and non-panicking, like the poller.
async fn lint_tick_loop(
    gateway_url: String,
    tenant: String,
    token: String,
    interval: std::time::Duration,
    draining: Arc<std::sync::atomic::AtomicBool>,
) {
    let client = match Client::connect(&gateway_url, SecretString::from(token)).await {
        Ok(client) => client,
        Err(e) => {
            tracing::error!(target: "escurel_runner", error = %e, "lint tick could not build a gateway client; disabled");
            return;
        }
    };
    let secs = interval.as_secs().max(1);
    tracing::info!(target: "escurel_runner", tenant = %tenant, interval_ms = interval.as_millis() as u64, "lint tick started");
    let mut ticker = tokio::time::interval(interval);
    loop {
        ticker.tick().await;
        if draining.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        // Wall-clock window (stable across restarts — the tick is I/O, not the
        // reducer, so reading the clock here is fine).
        let window = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() / secs)
            .unwrap_or(0);
        let event_id = lint_window_id(&tenant, window);
        let run_page = format!("markdown/instances/workflow-run/lint-{window}.md");
        let req = CaptureEventRequest {
            event_id: event_id.clone(),
            source: "runner-lint-tick".to_owned(),
            mime: "text/plain".to_owned(),
            label_skill: "lint".to_owned(),
            instance_page_id: run_page.clone(),
            title: "scheduled lint".to_owned(),
            body: "Scheduled semantic-health pass.".to_owned(),
            provenance: serde_json::json!({
                "workflow": { "run": run_page, "wf_skill": "lint", "phase": "invoke" }
            }),
            ..Default::default()
        };
        match client.capture_event(req).await {
            Ok(_) => tracing::info!(target: "escurel_runner", window, event_id = %event_id, "lint tick: invocation captured"),
            Err(e) => tracing::warn!(target: "escurel_runner", error = %e, "lint tick: capture_event failed; will retry next tick"),
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

/// Block until SIGTERM (the orchestrator's graceful-stop signal) or SIGINT
/// (Ctrl-C in a dev shell), then flip the `draining` flag so ingress stops
/// admitting new triggers and the poller releases its queue handle (#158). On
/// non-unix targets, only Ctrl-C.
#[cfg(unix)]
async fn wait_for_shutdown(draining: Arc<std::sync::atomic::AtomicBool>) {
    use tokio::signal::unix::{SignalKind, signal};

    let mut sigterm = signal(SignalKind::terminate()).expect("install SIGTERM handler");
    let mut sigint = signal(SignalKind::interrupt()).expect("install SIGINT handler");
    tokio::select! {
        _ = sigterm.recv() => {}
        _ = sigint.recv() => {}
    }
    draining.store(true, std::sync::atomic::Ordering::Relaxed);
}

#[cfg(not(unix))]
async fn wait_for_shutdown(draining: Arc<std::sync::atomic::AtomicBool>) {
    let _ = tokio::signal::ctrl_c().await;
    draining.store(true, std::sync::atomic::Ordering::Relaxed);
}
