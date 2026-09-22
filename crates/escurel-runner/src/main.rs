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
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use escurel_client::{AssignEventRequest, Client, ListEventsRequest, SecretString};
use escurel_obs::{Metrics, TelemetryConfig, init_telemetry};
use escurel_runner_core::{
    Admission, Autonomy, CascadeOutcome, ConfirmedEffect, DispatchConsumer, DispatchQueue,
    EnqueueOutcome, Governor, Ledger, LedgerDecision, LoopLimits, QuotaDecision, QuotaLimits,
    ReconcileError, RunFailure, RunStatus, RunnerConfig, TaskContext, Trigger, admit,
    classify_client_error, confirm_draft, confirm_effect, drive_workflow, emit_cascade,
    operation_has_terminal_status, package, recover_pending, recover_workflows, run_with_retry,
};
use escurel_runner_core::{DeadLetterReason, RunId, StepTerminal};
use escurel_runner_harness::{
    AgyHarness, ClaudeHarness, CodexHarness, DelegateHarness, EchoHarness, GeminiHarness, Harness,
    MuseHarness, RefusingHarness,
};
use escurel_types::{CaptureEventRequest, Event, ListInboxRequest};
use hmac::{Hmac, Mac};
use serde_json::json;
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
    /// The live runs' cancel handles (workbench backend P2-3a). `POST
    /// /debug/cancel` and the `escurel:run-control` subscriber stop a run
    /// through it.
    cancels: escurel_runner_core::CancelRegistry,
    /// Where and as whom to read the gateway when a requeue rebuilds a
    /// trigger from its event (`None` tokens = the dev/legacy path with no
    /// credentials: the bare trigger is enqueued as before).
    gateway_url: String,
    tokens: Option<Arc<escurel_runner_core::TokenSource>>,
    /// When the poller last completed a tick (epoch ms; 0 = never), for the
    /// status report's `last_poll_age_ms`.
    last_poll_ms: Arc<std::sync::atomic::AtomicU64>,
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
    /// The runner's own tenant (async-ops Phase 1, `/trigger` binding). This
    /// runner process is single-tenant by deployment (one runner per customer
    /// stack), so the authoritative tenant of an inbound webhook is THIS, never
    /// the request body — a party holding the webhook secret must not be able to
    /// name another tenant. `None` only on the dev/legacy path with no tenant
    /// configured, where the body value is accepted as before.
    tenant: Option<Arc<str>>,
    /// The runner's own subject — the identity a runner-emitted event is
    /// captured as. An inbound `/trigger` event's cascade lineage is trusted for
    /// loop control only when it was captured by this subject (runner-lineage-
    /// forge fix); `None` (static token with no readable `sub`) → trust-all.
    lineage_trust_subject: Option<Arc<str>>,
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

    // The runner's gateway credential, built once. A static
    // ESCUREL_RUNNER_TOKEN still wins; otherwise, given an issuer and a
    // signing key, the runner mints and re-mints its own — a pasted bearer
    // expires silently, and a runner whose token has lapsed still answers
    // /healthz while quietly filing nothing.
    //
    // The key is read HERE rather than carried on `RunnerConfig`, so it
    // cannot reach a log through that struct's derived `Debug`.
    let tokens: Option<Arc<escurel_runner_core::TokenSource>> = match config.token_source(
        std::env::var("ESCUREL_RUNNER_AUTH_SIGNING_KEY")
            .ok()
            .as_deref(),
    ) {
        Some(Ok(source)) => Some(Arc::new(source)),
        Some(Err(e)) => {
            // Refusing to start beats starting without a credential: the
            // second is indistinguishable from an empty inbox.
            tracing::error!(
                target: "escurel_runner",
                error = %e,
                "ESCUREL_RUNNER_AUTH_SIGNING_KEY is configured but unusable; refusing to start"
            );
            std::process::exit(2);
        }
        None => None,
    };

    // In-flight quota slots, shared gate → dispatch loop (#158).
    let inflight: InflightSlots = Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
    let cancels = escurel_runner_core::CancelRegistry::new();
    let last_poll_ms: Arc<std::sync::atomic::AtomicU64> =
        Arc::new(std::sync::atomic::AtomicU64::new(0));

    // Crash recovery (#158): before opening for traffic, reconcile any
    // orphaned `pending` rows left by a previous crash. A confirmed effect is
    // marked processed; an unconfirmed row is reset to retriable so the poller
    // backstops it. Best-effort, bounded; only runs with a gateway client.
    if let (Some(_), Some(source)) = (config.tenant.clone(), tokens.clone())
        && let Ok(token) = source.current()
    {
        match Client::connect(&config.gateway_url, SecretString::from(token)).await {
            Ok(client) => {
                let report = recover_pending(&ledger, &client, config.emit_run_events).await;
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
    match (config.tenant.clone(), tokens.clone()) {
        (Some(_), Some(source)) => {
            let harness = build_harness(&config);
            tokio::spawn(dispatch_loop(
                consumer,
                Arc::clone(&ledger),
                config.clone(),
                source,
                harness,
                governor.clone(),
                Arc::clone(&metrics),
                Arc::clone(&inflight),
                cancels.clone(),
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

    // The promotion tail (workbench backend P2-1): a human landing a held
    // write is the cascade the run could not emit itself. Same enablement
    // as the poller.
    if let (Some(tenant), Some(source)) = (config.tenant.clone(), tokens.clone()) {
        tokio::spawn(promotion_tail_loop(
            config.gateway_url.clone(),
            tenant,
            source,
            config.poll_interval,
            Arc::clone(&ledger),
            Arc::clone(&draining),
        ));
    }

    // The inbox poller: the self-healing fallback for missed webhooks.
    // Enabled only when both a tenant and a token are configured.
    match (config.tenant.clone(), tokens.clone()) {
        (Some(tenant), Some(source)) => {
            tokio::spawn(poll_loop(
                config.gateway_url.clone(),
                tenant,
                source,
                config.poll_interval,
                queue.clone(),
                Arc::clone(&ledger),
                limits,
                governor.clone(),
                Arc::clone(&metrics),
                Arc::clone(&inflight),
                Arc::clone(&draining),
                Arc::clone(&last_poll_ms),
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
    match (config.lint_interval, config.tenant.clone(), tokens.clone()) {
        (Some(interval), Some(tenant), Some(source)) => {
            tokio::spawn(lint_tick_loop(
                config.gateway_url.clone(),
                tenant,
                source,
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
        cancels: cancels.clone(),
        gateway_url: config.gateway_url.clone(),
        tokens: tokens.clone(),
        last_poll_ms: Arc::clone(&last_poll_ms),
        draining: Arc::clone(&draining),
        tenant: config.tenant.clone().map(Arc::from),
        lineage_trust_subject: tokens.as_ref().and_then(|t| t.subject()).map(Arc::from),
    };
    // The run-control subscriber (workbench backend P2-3b) and the status
    // reporter (P2-4). Same enablement as the poller and the promotion tail.
    if let (Some(tenant), Some(source)) = (config.tenant.clone(), tokens.clone()) {
        tokio::spawn(control_tail_loop(
            state.clone(),
            config.gateway_url.clone(),
            tenant.clone(),
            source.clone(),
            config.poll_interval,
        ));
        tokio::spawn(status_loop(
            state.clone(),
            config.gateway_url.clone(),
            tenant,
            source,
            StatusIdentity {
                runner_id: config.runner_id.clone(),
                version: config.version.clone(),
                harness: config.harness.clone(),
            },
            config.status_interval,
        ));
    }
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
        .route("/debug/cancel", post(debug_cancel))
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

    // 3. The authoritative tenant is THIS runner's own (async-ops Phase 1):
    //    the process is single-tenant by deployment, so the tenant is never
    //    taken from the request body — a party holding the webhook secret must
    //    not be able to drive another tenant's runs through this runner. The
    //    body's `tenant_id` is only cross-checked: if present and disagreeing
    //    with the runner's tenant, the delivery was mis-routed → reject.
    let body_tenant = serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| {
            v.get("tenant_id")
                .and_then(|t| t.as_str())
                .map(str::to_owned)
        });
    let tenant = match resolve_trigger_tenant(state.tenant.as_deref(), body_tenant.as_deref()) {
        Ok(tenant) => tenant,
        Err(claimed) => {
            tracing::warn!(
                target: "escurel_runner",
                own_tenant = ?state.tenant,
                claimed_tenant = %claimed,
                "POST /trigger rejected: body tenant_id does not match this runner's tenant"
            );
            return StatusCode::FORBIDDEN;
        }
    };

    let trigger = match state.lineage_trust_subject.as_deref() {
        Some(subj) => Trigger::from_event_gated(&event, tenant, subj),
        None => Trigger::from_event(&event, tenant),
    };
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
///   enqueue — the cascade stops here. Otherwise enqueue the trigger.
/// - `AlreadyTerminal` (idempotency — `processed`/`dead_letter`) / `InFlight`
///   (dedup) → drop. (A prior `failed` run is re-claimed as `Created`.)
///
/// The seen-set collapses a webhook/poll overlap, but it is only a cache in
/// front of the ledger and it is **never** cleared on completion — so the
/// `Created` arm drops this event's entry before enqueueing. Reaching
/// `Created` is proof that no run for the event is in flight, so an entry
/// still present there is stale by construction; left in place it vetoed
/// every #157 re-claim, and the re-claimed row then sat `pending` for ever.
///
/// Returns `true` if the trigger was enqueued. Best-effort: a ledger error
/// is logged and the trigger dropped (the poller re-pulls on the next tick),
/// never panicking the process. A trigger that is admitted but never reaches
/// the channel is reset to retriable `failed`, never left `pending`: the row
/// exists before the enqueue, and a `pending` row with nothing queued to move
/// it cannot be re-driven by anything.
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
    // Fail-closed (async-ops F1, generalised for the workbench backend): a
    // `kind: system` event and anything under the reserved `escurel:` label
    // namespace is bookkeeping ABOUT a run — an operation-status record, a
    // `run-started` / `run-finished`, a review transition, runner health —
    // never a dispatchable run. Drop it BEFORE `begin_run` so it creates no
    // ledger row: the runner writes one such event per transition, and
    // without this each would spawn (and dead-letter) a run — a runner
    // dispatching its own `run-finished` hands itself a job per run, forever.
    // The gateway hides these from the inbox the poller reads; the webhook
    // path does not, which is why this is the single chokepoint both routes
    // pass through. (`OPERATION_STATUS_LABEL` is `escurel:run-status`, one
    // member of the namespace.)
    if trigger.is_system || trigger.label_skill.starts_with("escurel:") {
        tracing::debug!(
            target: "escurel_runner",
            via,
            event_id = %trigger.event_id,
            label_skill = %trigger.label_skill,
            reason = if trigger.is_system { "system_kind" } else { "reserved_label" },
            "gate: dropping system / reserved-label event (not dispatchable)"
        );
        return false;
    }
    // A paused tenant (a `pause` control, workbench backend P2-3b) admits
    // nothing — checked BEFORE the ledger claim so a held event leaves no
    // row behind: it is not a run that failed, it is work not yet started.
    // The event stays in the inbox; the poller re-offers it after `resume`.
    if governor.is_paused(&trigger.tenant) {
        // Records the `paused` throttle (the only effect on a paused tenant).
        let _ = governor.try_admit(&trigger.tenant);
        tracing::debug!(
            target: "escurel_runner",
            via,
            tenant = %trigger.tenant,
            event_id = %trigger.event_id,
            "gate: tenant paused; holding (event stays in inbox)"
        );
        return false;
    }
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

            // Drop any stale seen-set entry for this event BEFORE enqueueing.
            //
            // Reaching `Created` proves no run for this event is in flight:
            // `begin_run` is one IMMEDIATE transaction with `ON CONFLICT DO
            // NOTHING`, so a concurrent delivery gets `InFlight` /
            // `AlreadyTerminal` and only one caller is ever handed `Created`.
            // So a seen-set hit here cannot be a live duplicate — it can only
            // be the residue of an earlier dispatch of the same event, and the
            // set is only ever cleared by an operator requeue, never on
            // completion.
            //
            // Without this, the #157 re-claim could not dispatch at all: the
            // ledger reset a `failed` row to `pending` and minted a fresh run
            // id, then `enqueue` answered `Duplicate` on the stale id and
            // nothing ran. That left the row `pending` for ever — unmovable,
            // because every later delivery read `pending` and dropped as
            // `InFlight` — with the `failed` verdict erased by the re-claim.
            queue.forget(&trigger.event_id);
            let outcome = queue.enqueue(trigger.clone());
            // If the trigger did not actually reach the channel, release the
            // quota slot we just took — the run won't dispatch under this
            // slot — and leave the row RETRIABLE so the poller re-drives it.
            //
            // Every non-`Enqueued` outcome is reset, not just `Full`: the row
            // is `pending` at this point and nothing is queued to move it, so
            // any outcome we leave unreset is a wedged run.
            if !matches!(outcome, EnqueueOutcome::Enqueued) {
                inflight
                    .lock()
                    .expect("inflight slots mutex")
                    .remove(&trigger.event_id);
                let _ = ledger.mark(&run_id, RunStatus::Failed);
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
        // Worth a line of its own, at info: a duplicate is a MODEL CALL not
        // made and a page not written twice, and it is the one drop that is
        // about the content rather than about the delivery. It names the run
        // that already did the work so "why did nothing happen?" is one
        // lookup.
        Ok(LedgerDecision::DuplicateContent(prior)) => {
            tracing::info!(
                target: "escurel_runner",
                via,
                tenant = %trigger.tenant,
                event_id = %trigger.event_id,
                instance = ?trigger.instance_page_id,
                already_run = %prior,
                "gate: identical content is already folded into this instance; not running again"
            );
            record_run_terminal(metrics, &trigger.tenant, "dead_letter");
            false
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

/// What the last attempt's harness reported, for `run-finished`.
#[derive(Debug, Default, Clone)]
struct AttemptSink {
    summary: String,
    tool_calls: u32,
    autonomy: Option<&'static str>,
    /// The last attempt's harness error, for `run-finished`'s reason.
    last_error: Option<String>,
}

/// Log + count a refused run lifecycle event. The projection is
/// best-effort by contract: the ledger decided, the event only describes.
fn record_run_event(metrics: &Metrics, kind: &str, result: Result<(), escurel_client::Error>) {
    if let Err(e) = result {
        metrics.inc_runner_run_event_failed(kind);
        tracing::warn!(
            target: "escurel_runner",
            kind,
            error = %e,
            "run event refused by the gateway (best-effort); run unaffected"
        );
    }
}

/// Resolve the authoritative tenant for an inbound `POST /trigger` (async-ops
/// Phase 1). The runner is single-tenant by deployment, so its own configured
/// tenant is authoritative and the request body can never *name* a tenant — it
/// may only match. Returns the tenant to use, or `Err(claimed)` (the offending
/// body value) when the body names a different tenant than this runner serves.
///
/// - runner tenant set, body absent or equal → the runner's tenant;
/// - runner tenant set, body differs → `Err` (mis-routed / forged delivery);
/// - no runner tenant (dev/legacy) → the body value, or empty.
fn resolve_trigger_tenant(own: Option<&str>, body_tenant: Option<&str>) -> Result<String, String> {
    match own {
        Some(own) => match body_tenant {
            Some(claimed) if claimed != own => Err(claimed.to_owned()),
            _ => Ok(own.to_owned()),
        },
        None => Ok(body_tenant.unwrap_or_default().to_owned()),
    }
}

/// Drive the workflow reducer on a run's terminal transition, dead-lettering
/// the run if the reducer pass errors (async-ops Phase 0.3, Bug B).
///
/// Called on EVERY terminal transition of a `trigger.workflow` run — not just a
/// confirmed non-held write — so a converged no-op, a held draft, or a failed
/// step advances (or terminates) the parent operation instead of wedging it at
/// `running`. On an `Advanced`/`Held` terminal the run's own effect already
/// landed and was recorded `processed`; if the reducer then fails, the run is
/// dead-lettered (`ReducerFailed`) so the stall surfaces to the DLQ rather than
/// masquerading as a clean success. On a `Failed` terminal the run is already
/// terminal, so a reducer error is only logged.
#[allow(clippy::too_many_arguments)]
async fn drive_workflow_or_deadletter(
    client: &escurel_client::Client,
    ledger: &Ledger,
    metrics: &Metrics,
    trigger: &Trigger,
    run_id: &escurel_runner_core::RunId,
    effect: Option<&escurel_runner_core::ConfirmedEffect>,
    terminal: StepTerminal,
    max_runs_per_root: u64,
    outbound_url: Option<&str>,
    outbound_bearer: Option<&str>,
) {
    match drive_workflow(
        client,
        trigger,
        &run_id.0,
        effect,
        terminal,
        max_runs_per_root,
    )
    .await
    {
        Ok(outcome) => {
            tracing::info!(
                target: "escurel_runner",
                event_id = %trigger.event_id,
                run_id = %run_id,
                terminal = ?terminal,
                emitted = outcome.emitted.len(),
                "workflow: reducer drove operation on terminal transition"
            );
            // Channel delivery (async-ops Phase 3): a terminal operation with a
            // stored conversation reference is delivered to the channel's
            // proactive seam. Fire-and-forget, best-effort — never derails the
            // run; at-least-once, the courier dedups on operation_id.
            if let (Some(delivery), Some(url)) = (outcome.delivery, outbound_url) {
                deliver_terminal(url, outbound_bearer, &delivery).await;
            }
        }
        Err(e) => {
            let progressed = matches!(terminal, StepTerminal::Advanced | StepTerminal::Held);
            if progressed {
                if let Err(dl) = ledger.dead_letter(run_id, DeadLetterReason::ReducerFailed) {
                    tracing::error!(
                        target: "escurel_runner",
                        event_id = %trigger.event_id,
                        run_id = %run_id,
                        error = %dl,
                        "workflow: could not dead-letter run after a reducer failure"
                    );
                } else {
                    record_run_terminal(metrics, &trigger.tenant, "dead_letter");
                }
                // F-4: record a terminal operation status so the run board and
                // the DLQ agree — otherwise the operation stays at whatever it
                // last reported (typically `running`) while a DLQ row exists.
                if let Some(wf) = &trigger.workflow {
                    escurel_runner_core::record_status_best_effort(
                        client,
                        &wf.run,
                        &trigger.event_id,
                        escurel_runner_core::OperationStatus::Failed,
                        &wf.phase,
                        "reducer_failed",
                        None,
                    )
                    .await;
                }
                tracing::warn!(
                    target: "escurel_runner",
                    event_id = %trigger.event_id,
                    run_id = %run_id,
                    error = %e,
                    "workflow: reducer pass failed; run dead-lettered (Bug B)"
                );
            } else {
                // A `Failed` terminal is already terminal in the ledger; the
                // reducer error is at most a missed best-effort status write.
                tracing::warn!(
                    target: "escurel_runner",
                    event_id = %trigger.event_id,
                    run_id = %run_id,
                    error = %e,
                    "workflow: reducer status pass on a failed run errored (run already terminal)"
                );
            }
        }
    }
}

/// Deliver a terminal operation result to the channel courier's proactive seam
/// (async-ops Phase 3). Best-effort fire-and-forget: a `POST <outbound_url>`
/// with `{operation_id, status, conversation_ref}`. A delivery failure is
/// logged, never propagated — the run already reached its terminal, and the
/// delivery is at-least-once (the courier dedups on `operation_id`).
async fn deliver_terminal(
    outbound_url: &str,
    outbound_bearer: Option<&str>,
    delivery: &escurel_runner_core::TerminalDelivery,
) {
    let mut body = serde_json::json!({
        "operation_id": delivery.operation_id,
        "status": delivery.status,
        "conversation_ref": delivery.conversation_ref,
    });
    // A selective PROGRESS delivery carries a human note; hand it to the receiver
    // as the `result` it already renders, so the courier posts the note verbatim
    // ("⏳ Working on …"). A terminal delivery has no note — the receiver renders
    // the operation's own result (or a terse status fallback) as before.
    if let Some(note) = &delivery.note {
        body["result"] = serde_json::json!({ "text": note });
    }
    // The CHANNEL's tenant, as recorded when the operation started. Present
    // only when the operation recorded one — omitted rather than null, so a
    // courier can distinguish "no binding available" (started before this
    // existed, or off-chat) from "a binding that says nothing".
    if let Some(tenant) = &delivery.channel_tenant {
        body["channel_tenant"] = serde_json::json!(tenant);
    }
    // A succeeded operation's produced-artifact reference (fleet #801, option D):
    // a delegated step returns only this, so the receiver resolves + renders it
    // into the reply when there is no already-rendered `result`. Omitted (not
    // null) when the operation produced no artifact.
    if let Some(result_ref) = &delivery.result_ref {
        body["result_ref"] = result_ref.clone();
    }
    // The agent's delivery receiver (`AGENT_ASYNC_CALLBACK_BEARER`) refuses a
    // callback with no/ wrong bearer (401). Attach it when configured; a sink
    // that requires none (a pull-only deploy, a test stub) leaves it unset.
    let mut req = reqwest::Client::new().post(outbound_url).json(&body);
    if let Some(bearer) = outbound_bearer {
        req = req.bearer_auth(bearer);
    }
    match req.send().await {
        Ok(resp) if resp.status().is_success() => tracing::info!(
            target: "escurel_runner",
            operation = %delivery.operation_id,
            status = %delivery.status,
            "delivery: terminal operation delivered to channel courier"
        ),
        Ok(resp) => tracing::warn!(
            target: "escurel_runner",
            operation = %delivery.operation_id,
            http_status = resp.status().as_u16(),
            "delivery: courier rejected the terminal delivery (best-effort; not retried here)"
        ),
        Err(e) => tracing::warn!(
            target: "escurel_runner",
            operation = %delivery.operation_id,
            error = %e,
            "delivery: could not reach the channel courier (best-effort)"
        ),
    }
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
/// Re-enqueue an event whose ledger row was just reset to `pending` (a DLQ
/// requeue, a `retry` control) so the runner re-drives it now rather than
/// at the next poll. Over quota, the poller re-drives it instead.
///
/// The trigger is rebuilt from the REAL event (re-read by id, through the
/// lineage trust gate the poller applies) when the gateway can be read:
/// a bare trigger with no label cannot be packaged (`resolve("[[]]")`
/// fails), which used to turn every requeue into a `failed` row the
/// poller then re-claimed — one extra failed run per requeue, and the
/// answer to "which run did the retry start?" was wrong.
async fn enqueue_requeued(state: &AppState, tenant: &str, event_id: &str) {
    // The row is pending under a fresh run id. If anything below fails to
    // queue it, put it back to retriable `failed` so the poller re-drives
    // it: a `pending` row with nothing queued is dropped as in-flight by
    // every later poll and wedges forever (codex second-opinion review of
    // P2, P1).
    let park = |why: &str| {
        if let Ok(Some(rec)) = state.ledger.get_run(tenant, event_id) {
            let _ = state
                .ledger
                .mark(&escurel_runner_core::RunId(rec.run_id), RunStatus::Failed);
        }
        tracing::info!(
            target: "escurel_runner",
            tenant = %tenant,
            event_id = %event_id,
            why,
            "requeue: not queued now; left retriable for the poller"
        );
    };
    let trigger = match &state.tokens {
        Some(tokens) => {
            // With credentials the trigger is rebuilt from the REAL event
            // (a bare one cannot be packaged); a read that fails leaves the
            // row for the poller rather than queueing something that would
            // fail at once.
            let Some(client) = connect_now(&state.gateway_url, tokens).await else {
                park("gateway unreachable");
                return;
            };
            let event = client
                .list_events(ListEventsRequest {
                    event_id: Some(event_id.to_owned()),
                    ..Default::default()
                })
                .await
                .ok()
                .and_then(|page| page.events.into_iter().next());
            let Some(event) = event else {
                park("event unreadable");
                return;
            };
            let mut t = match tokens.subject() {
                Some(subj) => Trigger::from_event_gated(&event, tenant.to_owned(), &subj),
                None => Trigger::from_event(&event, tenant.to_owned()),
            };
            // A requeue is an OPERATOR saying "run this again". Carrying a
            // content hash would let the content dedup refuse the one
            // request that is explicitly a re-run.
            t.content_hash = None;
            t
        }
        // The credential-less dev path keeps the bare trigger it always had.
        None => Trigger {
            manual: None,
            is_system: false,
            tenant: tenant.to_owned(),
            event_id: event_id.to_owned(),
            label_skill: String::new(),
            instance_page_id: None,
            lineage: escurel_runner_core::Lineage::root(event_id.to_owned()),
            workflow: None,
            content_hash: None,
        },
    };
    // Evict from the in-memory seen-set FIRST. `enqueue` drops a
    // trigger whose event_id it has seen, so without this the
    // requeue below is a no-op for the life of the process — the
    // ledger says pending, the DLQ says clean, and nothing runs.
    state.queue.forget(event_id);
    match state.governor.try_admit(tenant) {
        (QuotaDecision::Admit, Some(slot)) => {
            state
                .inflight
                .lock()
                .expect("inflight slots mutex")
                .insert(event_id.to_owned(), slot);
            if !matches!(
                state.queue.enqueue(trigger),
                escurel_runner_core::EnqueueOutcome::Enqueued
            ) {
                state
                    .inflight
                    .lock()
                    .expect("inflight slots mutex")
                    .remove(event_id);
                park("queue full");
                return;
            }
        }
        _ => {
            park("over quota");
            return;
        }
    }
    tracing::info!(
        target: "escurel_runner",
        tenant = %tenant,
        event_id = %event_id,
        "requeued run; cleared terminal block"
    );
}

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
            enqueue_requeued(&state, &tenant, &event_id).await;
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
    let cancelled = state
        .ledger
        .count_all_by_status(RunStatus::Cancelled)
        .unwrap_or(0);
    let planned = state
        .ledger
        .count_all_by_status(RunStatus::Planned)
        .unwrap_or(0);
    axum::Json(serde_json::json!({
        "total": total,
        "terminal": terminal,
        "cancelled": cancelled,
        "planned": planned,
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
/// `POST /debug/cancel {run_id} | {tenant, event_id}, reason?` — stop a live
/// run (workbench backend P2-3a). 200 `{run_id, cancelled: true}` when the
/// run was live and is now being stopped; 404 when no such run is live
/// (unknown, or already at a terminal — the ledger's answer stands). The
/// `escurel:run-control` subscriber (P2-3b) goes through the same registry.
async fn debug_cancel(
    State(state): State<AppState>,
    axum::Json(body): axum::Json<serde_json::Value>,
) -> impl IntoResponse {
    let reason = body
        .get("reason")
        .and_then(|v| v.as_str())
        .filter(|r| !r.is_empty());
    let run_id = if let Some(run_id) = body.get("run_id").and_then(|v| v.as_str()) {
        Some(run_id.to_owned())
    } else if let (Some(tenant), Some(event_id)) = (
        body.get("tenant").and_then(|v| v.as_str()),
        body.get("event_id").and_then(|v| v.as_str()),
    ) {
        match state.ledger.get_run(tenant, event_id) {
            Ok(Some(rec)) => Some(rec.run_id),
            _ => None,
        }
    } else {
        return (
            StatusCode::BAD_REQUEST,
            axum::Json(serde_json::json!({ "error": "provide run_id, or tenant + event_id" })),
        );
    };
    match run_id {
        Some(run_id) if state.cancels.cancel(&run_id, reason) => {
            tracing::info!(target: "escurel_runner", run_id = %run_id, reason = ?reason, "run cancel requested");
            (
                StatusCode::OK,
                axum::Json(serde_json::json!({ "run_id": run_id, "cancelled": true })),
            )
        }
        _ => (
            StatusCode::NOT_FOUND,
            axum::Json(serde_json::json!({ "error": "no such live run" })),
        ),
    }
}

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
/// drives the real Codex CLI (#153); `agy` drives the Antigravity CLI for
/// `autonomy: auto` runs; `gemini` drives Gemini over HTTP in process — the
/// one a container can run. An unknown selector REFUSES TO START rather than
/// falling back to `echo`: a typo'd `ESCUREL_RUNNER_HARNESS` that quietly became
/// `echo` would dispatch — writing echo's deterministic stand-in text into the
/// tenant's knowledge base and marking real events processed. Refusing to boot
/// is the recoverable failure (the same posture the `gemini` arm takes for a
/// missing key).
fn build_harness(config: &RunnerConfig) -> Arc<dyn Harness> {
    match build_harness_named(config, &config.harness) {
        Some(h) => h,
        None => {
            tracing::error!(
                target: "escurel_runner",
                selector = %config.harness,
                "unknown ESCUREL_RUNNER_HARNESS; refusing to start rather than falling back to \
                 the echo harness, which would write stand-in content into a real corpus"
            );
            std::process::exit(2);
        }
    }
}

/// The harness a workflow step declared, or the runner's own when it declared
/// none — the `harness:` key parsed at plan and phase level since the workflow
/// spec landed and, until now, propagated by nobody.
///
/// **An unbuildable declared harness FAILS CLOSED — it does not fall back to
/// the default.** A plan naming a harness this runner cannot build
/// (`build_harness_named` → `None`, e.g. `delegate` on a runner without it) must
/// NOT silently run the default: the default is a DIFFERENT harness, and running
/// `echo`/`gemini` for a step that asked for `delegate` would write a fabricated
/// stand-in result into a real corpus and mark the event processed. Instead the
/// step gets a [`RefusingHarness`] whose refusal maps to a PERMANENT reconcile
/// failure, so it dead-letters with a reason naming the harness the deployment
/// lacks — the operator fixes the selector, not the retry budget (the same
/// posture the `gemini` arm takes when its key is missing). A blank declaration,
/// or one equal to the runner's own harness, still resolves to the default.
fn resolve_harness(
    config: &RunnerConfig,
    default: &Arc<dyn Harness>,
    trigger: &Trigger,
    skill_harness: Option<&str>,
) -> Arc<dyn Harness> {
    let declared = trigger
        .workflow
        .as_ref()
        .map(|wf| wf.harness.as_str())
        .filter(|h| !h.is_empty() && *h != default.name());
    // A manual start's ask (workbench backend P2-5), honoured only within
    // the allow-list: a name outside it fails the run closed — running the
    // default in its place would do something the requester did not ask.
    // Priority: a workflow step's declaration, then the manual ask, then
    // the skill page's own `harness:` (P2-7) — the last two within the
    // allow-list.
    let manual = trigger
        .manual
        .as_ref()
        .and_then(|m| m.harness.as_deref())
        .filter(|h| !h.is_empty())
        .or(skill_harness.filter(|h| !h.is_empty()));
    if declared.is_none()
        && let Some(name) = manual
    {
        if !config.harness_allow.iter().any(|a| a == name) {
            tracing::warn!(
                target: "escurel_runner",
                event_id = %trigger.event_id,
                asked = %name,
                allow = ?config.harness_allow,
                "manual start asks for a harness outside ESCUREL_RUNNER_HARNESS_ALLOW; refusing"
            );
            return Arc::new(RefusingHarness::not_allowed(name));
        }
        if name == default.name() {
            return Arc::clone(default);
        }
        return match build_harness_named(config, name) {
            Some(h) => h,
            None => Arc::new(RefusingHarness::new(name)),
        };
    }
    let Some(name) = declared else {
        return Arc::clone(default);
    };
    match build_harness_named(config, name) {
        Some(h) => h,
        None => {
            tracing::error!(
                target: "escurel_runner",
                event_id = %trigger.event_id,
                declared = %name,
                "the workflow declares a harness this runner cannot build; failing the step \
                 closed rather than running the default harness, which would fabricate a result"
            );
            Arc::new(RefusingHarness::new(name))
        }
    }
}

/// Build one adapter by name, or `None` when the name is unknown.
fn build_harness_named(config: &RunnerConfig, name: &str) -> Option<Arc<dyn Harness>> {
    let built: Arc<dyn Harness> = match name {
        "echo" => Arc::new(EchoHarness::new(echo_harness_path())),
        // The A2A delegate harness (async-ops Phase 4 slice 3c): stateless —
        // the agent endpoint + capability + per-requester delegation token ride
        // on the TaskContext the packager builds. A delegate step with no
        // delegation params (unconfigured deploy / static-bearer runner) fails
        // closed at the harness, never delegating with the runner's identity.
        n if n == escurel_runner_core::DELEGATE_HARNESS => Arc::new(DelegateHarness::new()),
        "claude" => Arc::new(
            ClaudeHarness::new(config.claude_bin.clone()).with_model(config.claude_model.clone()),
        ),
        "codex" => Arc::new(
            CodexHarness::new(config.codex_bin.clone()).with_model(config.codex_model.clone()),
        ),
        // A CLI harness for a HOST with agy installed and logged in — not for
        // the cluster, which has neither. It runs `autonomy: auto` skills
        // only: see `AgyHarness`, which refuses a narrowed surface rather
        // than pretending to enforce one.
        "agy" => Arc::new(
            AgyHarness::new(config.agy_bin.clone())
                .with_model(config.agy_model.clone())
                .with_home(config.agy_home.clone()),
        ),
        // Muse Code, on a HOST with `muse` installed and logged in. Like
        // `agy` it runs `autonomy: auto` skills only: `muse exec` has no MCP
        // tool allow-list, so `MuseHarness` refuses a narrowed surface rather
        // than pretending to enforce one. Buildable since Muse 1.1.1 became
        // an MCP client (#451 recorded the 1.0.1 negative).
        "muse" => Arc::new(
            MuseHarness::new(config.muse_bin.clone())
                .with_model(config.muse_model.clone())
                .with_real_config_home(
                    config
                        .muse_config_home
                        .clone()
                        .map(std::path::PathBuf::from)
                        .or_else(|| {
                            std::env::var_os("XDG_CONFIG_HOME")
                                .map(std::path::PathBuf::from)
                                .or_else(|| {
                                    std::env::var_os("HOME")
                                        .map(|h| std::path::PathBuf::from(h).join(".config"))
                                })
                        }),
                ),
        ),
        // The one harness a container can run: HTTP to the model, no CLI, no
        // node runtime, no interactive login.
        "gemini" => match config.gemini_api_key.clone() {
            Some(key) => {
                let mut h = GeminiHarness::new(key)
                    .with_model(config.gemini_model.clone())
                    .with_base_url(config.gemini_base_url.clone());
                if let Some(turns) = config.harness_max_turns {
                    h = h.with_max_turns(turns);
                }
                Arc::new(h)
            }
            // Deliberately NOT the echo fallback below. A misconfigured
            // `gemini` selector that quietly became `echo` would keep
            // dispatching — writing echo's deterministic stand-in text into
            // the tenant's knowledge base and marking real events processed.
            // Refusing to start is the recoverable failure.
            None => {
                tracing::error!(
                    target: "escurel_runner",
                    "ESCUREL_RUNNER_HARNESS=gemini needs ESCUREL_GEMINI_API_KEY; refusing to \
                     start rather than falling back to the echo harness, which would write \
                     stand-in content into a real corpus"
                );
                std::process::exit(2);
            }
        },
        _ => return None,
    };
    Some(built)
}

/// A gateway client built with a CURRENT bearer.
///
/// Called wherever a loop is about to use its client, not once at boot: a
/// minted token is re-minted before it lapses, and a client holding an
/// expired one fails every call while the process stays healthy. Rebuilding
/// is cheap — the client is an HTTP client and a string.
async fn connect_now(
    gateway_url: &str,
    tokens: &escurel_runner_core::TokenSource,
) -> Option<Client> {
    let token = match tokens.current() {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(target: "escurel_runner", error = %e, "could not mint a gateway bearer");
            return None;
        }
    };
    match Client::connect(gateway_url, SecretString::from(token)).await {
        Ok(client) => Some(client),
        Err(e) => {
            tracing::error!(target: "escurel_runner", error = %e, "could not build a gateway client");
            None
        }
    }
}

/// Block until the gateway answers a real call, or the process is draining.
///
/// The call is `list_skills`: a read every runner is already allowed to make
/// (it is in `ALLOWED_TOOLS`), cheap, and — unlike building a client — it
/// proves the far end is serving rather than merely addressable.
///
/// Backoff climbs to a ceiling rather than hammering: the thing being waited
/// for takes minutes by design, and a tight loop against a booting DuckLake
/// index is load on exactly the process that needs the CPU.
async fn await_gateway(
    config: &RunnerConfig,
    tokens: &escurel_runner_core::TokenSource,
    drained: &Arc<Notify>,
) {
    let mut backoff = Duration::from_secs(1);
    let ceiling = Duration::from_secs(30);
    let mut waited = Duration::ZERO;
    loop {
        if let Some(client) = connect_now(&config.gateway_url, tokens).await
            && client
                .list_skills(escurel_types::ListSkillsRequest::default())
                .await
                .is_ok()
        {
            if waited > Duration::ZERO {
                tracing::info!(
                    target: "escurel_runner",
                    waited_ms = waited.as_millis() as u64,
                    "gateway answered; dispatch starting"
                );
            }
            return;
        }
        // Said once, at the wait's start: a line every second for sixteen
        // minutes buries whatever else the boot has to say.
        if waited == Duration::ZERO {
            tracing::info!(
                target: "escurel_runner",
                gateway = %config.gateway_url,
                "gateway not answering yet; holding dispatch rather than \
                 spending run attempts on it"
            );
        }
        // A SIGTERM during the wait must not be ignored — draining is the one
        // thing more urgent than starting.
        tokio::select! {
            () = tokio::time::sleep(backoff) => {}
            () = drained.notified() => {
                drained.notify_one();
                return;
            }
        }
        waited += backoff;
        backoff = (backoff * 2).min(ceiling);
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
    tokens: Arc<escurel_runner_core::TokenSource>,
    harness: Arc<dyn Harness>,
    governor: Governor,
    metrics: Arc<Metrics>,
    inflight: InflightSlots,
    cancels: escurel_runner_core::CancelRegistry,
    drained: Arc<Notify>,
) {
    // A PROBE, not the client this loop will use.
    //
    // Failing to build one at boot is a configuration fault worth refusing
    // on. Keeping one is a different thing entirely: a minted bearer lives 30
    // minutes, so a client hoisted out of this loop starts 401ing half an
    // hour after every restart and never recovers — measured in the cluster,
    // where the runner answered /healthz for hours while every dispatch
    // failed `ExpiredSignature`. The client is rebuilt per trigger below.
    if connect_now(&config.gateway_url, &tokens).await.is_none() {
        // `connect_now` already logged which half failed.
        tracing::error!(
            target: "escurel_runner",
            "dispatch loop could not build a gateway client; dispatch disabled"
        );
        drained.notify_one();
        return;
    }
    // **Wait for the gateway to ANSWER before spending anything on it.**
    //
    // Building a client proves the config, not the dependency: it mints a
    // bearer and constructs an HTTP client without touching the far end. The
    // gateway rebuilds a DuckLake index over Google Drive at boot and its own
    // platform budgets 29 minutes for that; measured in the lab, 16.
    //
    // A runner started in the same rollout found six real inbox events
    // immediately and dead-lettered every one of them within seconds —
    // `max_attempts` is 3 with a short backoff, which is a sensible policy
    // for a run that FAILED and the wrong one entirely for a dependency that
    // has not started yet. Three attempts over a few seconds against
    // something allowed half an hour to boot.
    //
    // So the loop does not begin until one call has succeeded. Triggers wait
    // in the bounded queue and the inbox poller backstops whatever the queue
    // drops; nothing is consumed, nothing is dead-lettered, and the events
    // are still there when the gateway is. After first contact this stops
    // mattering: a gateway that has answered once and then fails is a genuine
    // transient failure, which is exactly what the retry policy is for.
    await_gateway(&config, &tokens, &drained).await;

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

        // Fresh credential for THIS run (see the probe above).
        let client = match connect_now(&config.gateway_url, &tokens).await {
            Some(client) => client,
            None => {
                tracing::warn!(
                    target: "escurel_runner",
                    event_id = %trigger.event_id,
                    "dispatch: no gateway client for this trigger; leaving it for the poller"
                );
                inflight
                    .lock()
                    .expect("inflight slots mutex")
                    .remove(&trigger.event_id);
                continue;
            }
        };

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
        // The run's identity rides ON the per-run agent token (workbench
        // backend P1): the gateway stamps a draft's `run_id` / `root_event_id`
        // from it and authorises `report_progress` by it, so it is a claim the
        // runner signs, never a header the harness could set.
        // Cancellable from here to the terminal (workbench backend P2-3a).
        let cancel = cancels.register(run_id.as_str(), config.cancel_grace);
        let run_claims = escurel_runner_core::RunClaims {
            run_id: run_id.0.clone(),
            root_event_id: trigger.lineage.root_event_id.clone(),
            trace_id: Some(trace_id.clone()),
        };
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

        // ── async-ops: the workflow INVOCATION event is a control kick, not
        //    harness work ────────────────────────────────────────────────────
        // `start_operation` stamps the kick with a server-owned `phase: "invoke"`
        // targeting the run board. Dispatching it to the caller-scoped harness
        // makes the harness try to FOLD the event into the run board — a control
        // instance it must not write (the run board is the reducer's, and
        // `WORKFLOW_STEP_TOOLS` even denies the harness `assign_event`) — so the
        // event was never marked `processed` and EVERY operation dead-lettered
        // "event not yet processed". The echo suite missed it: echo's requester
        // owns the board and folds it cleanly, so only a real caller-scoped run
        // (the requester ≠ the runner) exposed it.
        //
        // Drive the reducer as the runner (admin): the "invocation pass" builds
        // an empty run state, emits the plan's first phase, and records
        // `running` (or `succeeded` + delivery for a one-phase plan); then the
        // runner (admin) assigns the invocation event to the run board so it is
        // `processed` and never re-triggers. Only the emitted phase STEPS run
        // under the caller-scoped harness, and those write their own produced
        // instances, which the requester owns.
        if let Some(wf) = trigger.workflow.clone().filter(|w| w.phase == "invoke") {
            // If a concurrent driver (crash recovery) already drove this
            // operation to a terminal state, do NOT re-drive it back to
            // `running`; just close the kick. (Normal operation has a single
            // driver — this only bites when a fresh runner's startup recovery
            // races the invocation of a board that already exists.)
            if operation_has_terminal_status(&client, &wf.run).await {
                let _ = client
                    .assign_event(AssignEventRequest {
                        event_id: trigger.event_id.clone(),
                        instance_page_id: wf.run.clone(),
                    })
                    .await;
                let _ = ledger.complete(&run_id, RunStatus::Processed, None);
                record_run_terminal(&metrics, &trigger.tenant, "processed");
                inflight
                    .lock()
                    .expect("inflight slots mutex")
                    .remove(&trigger.event_id);
                cancels.finish(run_id.as_str());
                continue;
            }
            match drive_workflow(
                &client,
                &trigger,
                &run_id.0,
                None,
                StepTerminal::Advanced,
                config.max_runs_per_root,
            )
            .await
            {
                Ok(outcome) => {
                    // A one-phase plan completes on the invocation pass and
                    // carries a terminal delivery; a multi-phase plan is now
                    // `running` with `delivery: None`.
                    if let (Some(delivery), Some(url)) =
                        (outcome.delivery, config.outbound_url.as_deref())
                    {
                        deliver_terminal(url, config.outbound_bearer.as_deref(), &delivery).await;
                    }
                    // Close the kick: mark the invocation event processed on the
                    // run board (admin) so the poller stops re-delivering it.
                    if let Err(e) = client
                        .assign_event(AssignEventRequest {
                            event_id: trigger.event_id.clone(),
                            instance_page_id: wf.run.clone(),
                        })
                        .await
                    {
                        tracing::warn!(
                            target: "escurel_runner",
                            event_id = %trigger.event_id,
                            run_id = %run_id,
                            error = %e,
                            "workflow: could not mark the invocation event processed"
                        );
                    }
                    let _ = ledger.complete(&run_id, RunStatus::Processed, None);
                    record_run_terminal(&metrics, &trigger.tenant, "processed");
                    tracing::info!(
                        target: "escurel_runner",
                        event_id = %trigger.event_id,
                        run_id = %run_id,
                        emitted = outcome.emitted.len(),
                        "workflow: invocation drove the reducer; first phase emitted"
                    );
                }
                Err(e) => {
                    let _ = ledger.dead_letter(&run_id, DeadLetterReason::ReducerFailed);
                    record_run_terminal(&metrics, &trigger.tenant, "dead_letter");
                    tracing::warn!(
                        target: "escurel_runner",
                        event_id = %trigger.event_id,
                        run_id = %run_id,
                        error = %e,
                        "workflow: invocation reducer pass failed; run dead-lettered"
                    );
                }
            }
            inflight
                .lock()
                .expect("inflight slots mutex")
                .remove(&trigger.event_id);
            cancels.finish(run_id.as_str());
            continue;
        }

        // Reconcile with retry: package + run the harness + read back over
        // `/mcp` to CONFIRM the effect, retrying transient failures with
        // backoff up to the attempts cap (#155).
        // The harness for THIS trigger, resolved once: a workflow step may
        // declare its own (`harness:` on the phase, else on the plan) and the
        // runner's configured one answers for everything else. Outside the
        // retry closure because a retry is the same step, not a new choice.
        // The skill's own asks (workbench backend P2-7): a `harness:` on the
        // skill page is honoured like a manual one, within the allow-list.
        let skill_harness = escurel_runner_core::skill_contract(&client, &trigger.label_skill)
            .await
            .and_then(|c| c.harness);
        let step_harness = resolve_harness(&config, &harness, &trigger, skill_harness.as_deref());
        // The run's lifecycle as `escurel:run` events (workbench backend
        // P1): a projection of the ledger for the humans watching the run,
        // best-effort — a refusal is logged and counted, never acted on.
        let run_ctx = escurel_runner_core::RunEventCtx {
            run_id: run_id.0.clone(),
            root_event_id: trigger.lineage.root_event_id.clone(),
            trigger_event_id: trigger.event_id.clone(),
            parent_run_id: trigger.lineage.parent_run_id.clone(),
            depth: trigger.lineage.depth,
            lineage_path: trigger.lineage.lineage_path.clone(),
            trace_id: Some(trace_id.clone()),
            harness: step_harness.name().to_owned(),
            model: None,
            max_attempts: config.max_attempts,
            target_page_id: trigger.instance_page_id.clone(),
            manual: trigger
                .manual
                .as_ref()
                .map(escurel_runner_core::ManualStart::to_value),
        };
        let emit_events = config.emit_run_events;
        if emit_events {
            record_run_event(&metrics, "started", run_ctx.emit_started(&client).await);
        }
        // What the last attempt reported — the harness summary, its tool-call
        // count and the packaged autonomy — for `run-finished`.
        let attempt_sink = std::sync::Mutex::new(AttemptSink::default());
        let (client_ref, ctx_ref, metrics_ref, sink_ref) =
            (&client, &run_ctx, &metrics, &attempt_sink);
        let report = run_with_retry(&config, |attempt| {
            // BOUNDED. The gateway client times out one request at 60s, but a
            // run is not one request — a dozen model turns, each with tool
            // calls, times the attempts cap. A run that stalled sat `pending`
            // holding an in-flight quota slot, not terminal enough to block a
            // re-delivery and not re-drivable either, because the poller's
            // seen-set already holds its event id.
            //
            // A timeout is a TRANSIENT failure: the retry policy already
            // knows what to do with one, and a stalled attempt is exactly
            // what a retry is for.
            // The harness for THIS trigger: a workflow step may declare its
            // own, and the runner's configured one answers for everything
            // else.
            let fut = attempt_run(
                &trigger,
                &client,
                &config,
                &tokens,
                step_harness.as_ref(),
                attempt,
                Some(&run_claims),
                &cancel,
                sink_ref,
            );
            let bound = config.run_timeout;
            async move {
                let started_at = escurel_runner_core::now_ts();
                let result = match tokio::time::timeout(bound, fut).await {
                    Ok(result) => result,
                    Err(_) => Err(ReconcileError::Transient(format!(
                        "run attempt exceeded {}s and was abandoned",
                        bound.as_secs()
                    ))),
                };
                if emit_events {
                    let (outcome, error) = match &result {
                        Ok(_) => ("ok", None),
                        Err(ReconcileError::Converged(r)) => ("converged", Some(r.clone())),
                        Err(ReconcileError::Cancelled(r)) => ("cancelled", Some(r.clone())),
                        Err(e) if e.to_string().contains("abandoned") => {
                            ("timeout", Some(e.to_string()))
                        }
                        Err(e) => ("failed", Some(e.to_string())),
                    };
                    let report = escurel_runner_core::AttemptReport {
                        attempt,
                        started_at,
                        ended_at: escurel_runner_core::now_ts(),
                        outcome,
                        error,
                    };
                    record_run_event(
                        metrics_ref,
                        "attempt",
                        ctx_ref.emit_attempt(client_ref, &report).await,
                    );
                }
                result
            }
        })
        .await;

        // Outcome → terminal status:
        // - confirmed effect      → `processed` (+ produced instance/version);
        // - clean no-op (converged) → `processed` with no produced instance —
        //   a converged cascade hop ends tidily, NOT `failed` (#156/#157);
        // - retries exhausted / bad output → `dead_letter` (#158), terminal;
        // - otherwise (permanent)  → `failed` (retriable; operator may re-drive).
        // A plan-mode run's clean no-op is `planned`, not `processed`
        // (workbench backend P2-5b): nothing landed, on purpose.
        let planned = trigger.manual.as_ref().is_some_and(|m| m.mode == "plan");
        let result = match (&report.confirmed, report.converged_no_op, report.failure) {
            (Some(effect), _, _) => ledger.complete(
                &run_id,
                RunStatus::Processed,
                Some((effect.instance_page_id.as_str(), effect.version.as_str())),
            ),
            (None, true, _) if planned => {
                record_run_terminal(&metrics, &trigger.tenant, "planned");
                ledger.complete(&run_id, RunStatus::Planned, None)
            }
            (None, true, _) => ledger.complete(&run_id, RunStatus::Processed, None),
            (None, false, Some(RunFailure::RetriesExhausted)) => {
                record_run_terminal(&metrics, &trigger.tenant, "dead_letter");
                ledger.dead_letter(&run_id, DeadLetterReason::RetriesExhausted)
            }
            (None, false, Some(RunFailure::BadOutput)) => {
                record_run_terminal(&metrics, &trigger.tenant, "dead_letter");
                ledger.dead_letter(&run_id, DeadLetterReason::BadOutput)
            }
            (None, false, Some(RunFailure::Cancelled)) => {
                record_run_terminal(&metrics, &trigger.tenant, "cancelled");
                ledger.complete(&run_id, RunStatus::Cancelled, None)
            }
            (None, false, _) => ledger.complete(&run_id, RunStatus::Failed, None),
        };
        // The run is no longer live; the requester's reason, if cancelled.
        let cancel_reason = cancels
            .finish(run_id.as_str())
            .map(|r| r.unwrap_or_else(|| "cancelled".to_owned()));
        if emit_events {
            let finish = match (&report.confirmed, report.converged_no_op, report.failure) {
                (Some(effect), _, _) => escurel_runner_core::RunFinish::Processed {
                    produced: Some((effect.instance_page_id.clone(), effect.version.clone())),
                    held: effect.held,
                },
                (None, true, _) if planned => escurel_runner_core::RunFinish::Planned,
                (None, true, _) => escurel_runner_core::RunFinish::Processed {
                    produced: None,
                    held: false,
                },
                (None, false, Some(RunFailure::RetriesExhausted)) => {
                    escurel_runner_core::RunFinish::DeadLetter {
                        reason: "retries_exhausted".to_owned(),
                    }
                }
                (None, false, Some(RunFailure::BadOutput)) => {
                    escurel_runner_core::RunFinish::DeadLetter {
                        reason: "bad_output".to_owned(),
                    }
                }
                (None, false, Some(RunFailure::Cancelled)) => {
                    escurel_runner_core::RunFinish::Cancelled {
                        reason: cancel_reason
                            .clone()
                            .unwrap_or_else(|| "cancelled".to_owned()),
                    }
                }
                (None, false, _) => escurel_runner_core::RunFinish::Failed {
                    reason: "permanent".to_owned(),
                    error: attempt_sink.lock().ok().and_then(|s| s.last_error.clone()),
                },
            };
            let sink = attempt_sink.lock().map(|s| s.clone()).unwrap_or_default();
            record_run_event(
                &metrics,
                "finished",
                run_ctx
                    .emit_finished(
                        &client,
                        report.attempts,
                        &finish,
                        &sink.summary,
                        sink.tool_calls,
                        sink.autonomy,
                    )
                    .await,
            );
        }
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
                // A HELD write ends here. It is a real, read-back effect —
                // recorded in the ledger with the draft's hash as its version
                // — but nothing has landed, so there is nothing for a
                // follow-on agent to react to and nothing for a workflow
                // reducer to advance. Cascading it would spend a whole
                // lineage's budget on a change a human may still refuse, and
                // would do it BEFORE they were asked.
                if effect.held {
                    // No second `record_run_terminal`: this arm already
                    // counted the run `processed`, and counting it twice
                    // would quietly inflate the metric the absorption curve
                    // is read from.
                    tracing::info!(
                        target: "escurel_runner",
                        event_id = %trigger.event_id,
                        run_id = %run_id,
                        target = %effect.instance_page_id,
                        draft_sha256 = %effect.version,
                        "dispatch: run produced a DRAFT awaiting a human; no cascade"
                    );
                    // A held draft in a WORKFLOW step pauses the operation for
                    // human approval (async-ops Phase 0.3): drive the reducer
                    // with a `Held` terminal so it records `awaiting_human` and
                    // does not advance the plan past the unapproved draft.
                    if trigger.workflow.is_some() {
                        drive_workflow_or_deadletter(
                            &client,
                            &ledger,
                            &metrics,
                            &trigger,
                            &run_id,
                            None,
                            StepTerminal::Held,
                            config.max_runs_per_root,
                            config.outbound_url.as_deref(),
                            config.outbound_bearer.as_deref(),
                        )
                        .await;
                    }
                    continue;
                }
                // Dynamic workflows: a confirmed write whose trigger carries a
                // `provenance.workflow` block drives the reducer instead of the
                // cascade — the cascade is the width-≤1 special case, the
                // reducer the general one. It emits the plan's next batch of
                // step events (each a §3.6-idempotent, lineage-tagged
                // `capture_event`), guarded by the same `admit` controls.
                if trigger.workflow.is_some() {
                    drive_workflow_or_deadletter(
                        &client,
                        &ledger,
                        &metrics,
                        &trigger,
                        &run_id,
                        Some(effect),
                        StepTerminal::Advanced,
                        config.max_runs_per_root,
                        config.outbound_url.as_deref(),
                        config.outbound_bearer.as_deref(),
                    )
                    .await;
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
                    Ok(CascadeOutcome::NotAllowed { produced_skill }) => tracing::info!(
                        target: "escurel_runner",
                        event_id = %trigger.event_id,
                        produced_skill = %produced_skill,
                        "cascade: the skill's `actions` do not include the produced skill; no follow-on"
                    ),
                    Ok(CascadeOutcome::DepthCapped { max_depth }) => tracing::info!(
                        target: "escurel_runner",
                        event_id = %trigger.event_id,
                        max_depth,
                        "cascade: the skill's `cascade.max_depth` is reached; no follow-on"
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
                // Dynamic workflows (async-ops Phase 0.3, Bug A): a non-success
                // terminal must still advance or terminate the parent operation
                // — previously only a confirmed write drove the reducer, so
                // these wedged the operation at `running` forever. A converged
                // no-op advances the plan; every real failure resolves via the
                // failed phase's authored `on_exhausted` policy.
                //
                // On crew F-2: this arm is only reached once a run has hit a
                // LEDGER TERMINAL (dead-letter or fail-fast `failed`). A merely
                // *transient* error is retried WITHIN the run by the reconciler
                // (up to MAX_ATTEMPTS) and either clears (→ a confirmed write,
                // handled above) or exhausts (→ `RetriesExhausted`); it never
                // arrives here mid-retry. `Permanent` is a fail-fast terminal
                // with no automatic recovery (the seen-set blocks the poller's
                // re-claim within a process), so it too terminates the
                // operation rather than wedging it at `running`. The reason slug
                // rides into the status provenance (F-5). Additive to the
                // metrics/logging below.
                if trigger.workflow.is_some() {
                    let terminal = if report.converged_no_op {
                        Some(StepTerminal::Advanced)
                    } else {
                        match report.failure {
                            Some(RunFailure::RetriesExhausted) => {
                                Some(StepTerminal::Failed("retries_exhausted"))
                            }
                            Some(RunFailure::BadOutput) => Some(StepTerminal::Failed("bad_output")),
                            Some(RunFailure::Permanent) => Some(StepTerminal::Failed("permanent")),
                            // A cancelled step fails its operation with the
                            // reason a human can act on (P2-3a).
                            Some(RunFailure::Cancelled) => Some(StepTerminal::Failed("cancelled")),
                            // No failure and not converged: not a real terminal
                            // (should not occur) — leave the operation running.
                            None => None,
                        }
                    };
                    if let Some(terminal) = terminal {
                        drive_workflow_or_deadletter(
                            &client,
                            &ledger,
                            &metrics,
                            &trigger,
                            &run_id,
                            None,
                            terminal,
                            config.max_runs_per_root,
                            config.outbound_url.as_deref(),
                            config.outbound_bearer.as_deref(),
                        )
                        .await;
                    }
                }
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
                    Some(RunFailure::Cancelled) => tracing::info!(
                        target: "escurel_runner",
                        event_id = %trigger.event_id,
                        run_id = %run_id,
                        reason = ?cancel_reason,
                        "dispatch: run cancelled; recorded cancelled (event left in inbox, no cascade)"
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
// Eight inputs describe one attempt of one run (its identity, its harness,
// where to report what it did); bundling them would only move the count.
#[allow(clippy::too_many_arguments)]
async fn attempt_run(
    trigger: &Trigger,
    client: &Client,
    config: &RunnerConfig,
    tokens: &escurel_runner_core::TokenSource,
    harness: &dyn Harness,
    attempt: u32,
    run: Option<&escurel_runner_core::RunClaims>,
    cancel: &escurel_runner_core::Cancel,
    sink: &std::sync::Mutex<AttemptSink>,
) -> Result<ConfirmedEffect, ReconcileError> {
    let mut task: TaskContext = package(trigger, client, config, Some(tokens), run)
        .await
        .map_err(|e| {
            tracing::warn!(
                target: "escurel_runner",
                event_id = %trigger.event_id,
                attempt,
                error = %e,
                "dispatch: packaging failed"
            );
            package_error_to_reconcile(e)
        })?;
    task.cancel = Some(cancel.clone());
    // Cancelled while packaging (or between attempts): don't start a harness.
    if cancel.is_cancelled() {
        return Err(ReconcileError::Cancelled(
            "cancelled before the harness started".to_owned(),
        ));
    }

    // The instance as it stands BEFORE the agent runs, so a write can be told
    // from a run that touched nothing. Only for a pre-flagged auto run, which
    // is the only shape `assign_confirmed_write` acts on.
    let version_before = match (&trigger.instance_page_id, task.autonomy) {
        (Some(page), Autonomy::Auto) => escurel_runner_core::instance_version(client, page).await?,
        _ => None,
    };

    // Carried past the match so the read-back below can tell "the harness
    // named a page" from "it named nothing" — the latter is only meaningful
    // once the gateway has also been asked.
    let (harness_produced, harness_reported_failure, harness_result_ref) =
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
                // A planning run (P2-5b) reported its plan and stopped: a
                // clean terminal with nothing to confirm, never a write to
                // reconcile.
                if task.plan_mode {
                    if let Ok(mut s) = sink.lock() {
                        s.summary = outcome.summary.clone();
                        s.tool_calls = outcome.tool_calls;
                    }
                    return Err(ReconcileError::Converged(
                        "planned: the harness reported its plan and stopped".to_owned(),
                    ));
                }
                if let Ok(mut s) = sink.lock() {
                    s.summary = outcome.summary.clone();
                    s.tool_calls = outcome.tool_calls;
                    s.autonomy = Some(match task.autonomy {
                        Autonomy::Auto => "auto",
                        Autonomy::Review => "review",
                        Autonomy::Confirm => "confirm",
                    });
                }
                // A self-reported FAILURE is not evidence either.
                //
                // This used to return here, before the read-back — which
                // contradicted the rule stated eight lines below and enforced
                // everywhere else in this function: the gateway is the authority,
                // never the harness's own account of itself. Measured in the
                // cluster on 2026-09-06: a Gemini run created a draft
                // (`create_draft` → `status: ok`), kept talking, hit the turn cap,
                // and reported failure. The run was recorded `failed
                // (retriable re-drive)` for work that had landed — and a re-drive
                // would have produced a SECOND draft for a page whose rule is
                // one draft per page.
                //
                // So carry the report past the read-back and let it decide. If
                // the gateway confirms an effect, the run succeeded whatever the
                // model said; if it confirms nothing, this becomes the permanent
                // failure it always was, with the harness's own words attached.
                let reported_failure = (!outcome.ok).then(|| outcome.summary.clone());
                // NB: "produced no instance + no pre-flagged target" is NOT by
                // itself a no-op, and treating it as one was a real bug. Both
                // real LLM adapters hardcode `produced_instance: None` — their
                // envelopes do not name the page the model wrote — so this fired
                // on every unflagged `claude`/`codex` run, including ones that
                // had just written a page and assigned the event. The run was
                // recorded `processed` with no effect, and never cascaded. Only
                // the `echo` stub reports a produced instance, which is why the
                // suite never saw it.
                //
                // The gateway is the authority, so ask it first (below) and
                // decide afterwards. `result_ref` is the harness's own — a produced
                // artifact the gateway read-back cannot observe — so it rides
                // through to the confirmed effect verbatim.
                (
                    outcome.produced_instance,
                    reported_failure,
                    outcome.result_ref,
                )
            }
            Err(e) => {
                tracing::warn!(
                    target: "escurel_runner",
                    event_id = %trigger.event_id,
                    attempt,
                    error = %e,
                    "dispatch: harness run failed"
                );
                if let Ok(mut s) = sink.lock() {
                    s.last_error = Some(e.to_string());
                }
                return Err(harness_error_to_reconcile(&e));
            }
        };

    // Don't trust the harness: read back over `/mcp` to confirm the event is
    // processed + bound and the instance's version advanced (#155). For an
    // unflagged trigger this now resolves the instance the agent chose, via
    // the by-event lookup — `assign_event` recorded the binding.
    // WHAT to confirm depends on what the skill allowed the run to do. A
    // review run leaves the event in the inbox on purpose, so the landed-write
    // read-back would never converge — it would burn every retry and
    // dead-letter each held write.
    //
    // First, finish the bookkeeping the agent may not have: bind the event to
    // the page the runner flagged, when the gateway confirms that page was
    // actually written. See `assign_confirmed_write` for why this is the
    // runner's job and how narrow it is.
    if task.autonomy == Autonomy::Auto {
        match escurel_runner_core::assign_confirmed_write(
            client,
            trigger,
            version_before.as_deref(),
        )
        .await
        {
            Ok(true) => tracing::info!(
                target: "escurel_runner",
                event_id = %trigger.event_id,
                harness = %harness.name(),
                attempt,
                "dispatch: the write landed and the agent did not assign; runner bound the event"
            ),
            Ok(false) => {}
            // Not fatal on its own: the read-back below decides. If the
            // assignment was genuinely needed it will report the event
            // unprocessed, which is the same transient it always was.
            Err(e) => tracing::warn!(
                target: "escurel_runner",
                event_id = %trigger.event_id,
                error = %e,
                "dispatch: could not bind the event after a confirmed write"
            ),
        }
    }

    let confirmed = match task.autonomy {
        Autonomy::Auto => confirm_effect(client, trigger).await,
        Autonomy::Review | Autonomy::Confirm => confirm_draft(client, trigger).await,
    };
    match confirmed {
        Ok(mut effect) => {
            // Carry the harness's produced-artifact reference onto the confirmed
            // effect (async-ops Phase 4): the gateway read-back cannot observe it,
            // so it comes only from the harness's own outcome. The driver stamps
            // it onto the terminal `succeeded` status event.
            effect.result_ref = harness_result_ref;
            if let Some(summary) = &harness_reported_failure {
                // Worth a line: the model said it failed and the gateway
                // disagrees. The gateway wins, and someone should know the
                // harness is stopping short of its own success.
                tracing::info!(
                    target: "escurel_runner",
                    event_id = %trigger.event_id,
                    harness = %harness.name(),
                    attempt,
                    summary = %summary,
                    "dispatch: harness reported failure but the gateway confirms the effect; \
                     taking the gateway's word"
                );
            }
            Ok(effect)
        }
        // The harness said it failed and the gateway confirms nothing. Now
        // the report is the answer, and re-running will not change it.
        Err(_) if harness_reported_failure.is_some() => Err(ReconcileError::Permanent(format!(
            "harness {} reported a failed outcome and the gateway confirms no effect: {}",
            harness.name(),
            harness_reported_failure.unwrap_or_default()
        ))),
        // Read-back could not confirm anything, the harness reported no
        // produced instance, and nothing was pre-flagged: the agent ran
        // cleanly and genuinely did nothing. Terminate CLEANLY (#156/#157)
        // rather than burning retries on a read-back that will not converge
        // and then recording `failed` — a converged cascade hop ends here.
        //
        // This is deliberately the LAST resort, not the first check: it fires
        // only once the gateway has been asked and had no effect to report.
        Err(ReconcileError::Transient(reason))
            if harness_produced.is_none() && trigger.instance_page_id.is_none() =>
        {
            Err(ReconcileError::Converged(format!(
                "harness ran cleanly and the gateway reports no effect ({reason})"
            )))
        }
        // The same rule for a REVIEW run, where "produced" means something
        // different and the two clauses above do not fire.
        //
        // Under `autonomy: review` the only effect that counts is a DRAFT,
        // and `confirm_draft` has just asked the gateway and been told there
        // is none. A `produced_instance` from the harness is then not
        // evidence of an effect: an agent that read a page and concluded
        // nothing needed changing names that page, because naming it is how
        // it says what the event was about. Requiring `harness_produced` to
        // be absent therefore never converges a review no-op — it retries it.
        //
        // Measured on lab, 2026-09-12. The agent answered "Event already
        // covered by existing note; no page modification needed" and named
        // the note. The run was retried, and on the second attempt the agent
        // did the only thing that would satisfy the read-back: it created a
        // draft of a page it had just said needed no change. A human then
        // found that card in their review queue, indistinguishable from real
        // work until they opened it.
        //
        // Asking twice and taking the second answer is not a retry; it is
        // pressure. An agent given a tool and asked again will use it.
        Err(ReconcileError::Transient(reason)) if task.autonomy == Autonomy::Review => {
            Err(ReconcileError::Converged(format!(
                "review run: the harness ran cleanly and the gateway holds no \
                 draft for this event ({reason})"
            )))
        }
        Err(e) => Err(e),
    }
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
/// timeout / I/O / upstream are transient (the host, subprocess or upstream
/// may recover); a non-zero
/// exit is permanent; **unparseable output** is its own `BadOutput` variant so
/// the dispatch loop dead-letters it `bad_output` (#158).
fn harness_error_to_reconcile(e: &escurel_runner_harness::HarnessError) -> ReconcileError {
    use escurel_runner_harness::HarnessError as H;
    match e {
        // `Upstream` is transient by the same reasoning as `Spawn`: a 429
        // from the model API or a gateway that blinked is exactly what the
        // retry policy exists for. A genuinely permanent misconfiguration
        // (a revoked key) will exhaust `max_attempts` and dead-letter with
        // the upstream's own message attached, which is the diagnosis.
        H::Spawn { .. } | H::Timeout { .. } | H::Io { .. } | H::Upstream { .. } => {
            ReconcileError::Transient(e.to_string())
        }
        H::BadOutcome { .. } => ReconcileError::BadOutput(e.to_string()),
        H::Cancelled { .. } => ReconcileError::Cancelled(e.to_string()),
        // Permanent, and deliberately so: the harness refused this task
        // before running it, and a retry re-runs the same refusal. The
        // dead-letter carries the reason, which names the harness that can
        // run it — the operator changes the selector, not the retry budget.
        H::NonZeroExit { .. } | H::Unsupported { .. } => ReconcileError::Permanent(e.to_string()),
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
    tokens: Arc<escurel_runner_core::TokenSource>,
    interval: std::time::Duration,
    queue: DispatchQueue,
    ledger: Arc<Ledger>,
    limits: LoopLimits,
    governor: Governor,
    metrics: Arc<Metrics>,
    inflight: InflightSlots,
    draining: Arc<std::sync::atomic::AtomicBool>,
    last_poll_ms: Arc<std::sync::atomic::AtomicU64>,
) {
    // A boot probe only — the per-tick client is built inside the loop.
    // A hoisted one carries a 30-minute minted bearer for the life of the
    // process, which is how the poller came to log `ExpiredSignature` on
    // every tick for hours while /healthz stayed green.
    if connect_now(&gateway_url, &tokens).await.is_none() {
        tracing::error!(
            target: "escurel_runner",
            "inbox poller could not build a gateway client; poller disabled"
        );
        return;
    }
    tracing::info!(
        target: "escurel_runner",
        gateway = %gateway_url,
        tenant = %tenant,
        interval_ms = interval.as_millis() as u64,
        "inbox poller started"
    );

    // The runner's own identity: the cascade lineage on an inbox event is
    // trusted for loop control only when the event was captured by THIS subject
    // (a runner-emitted hop), so a caller cannot forge a shallow depth/root/path
    // (runner-lineage-forge fix). `None` (a static token with no readable `sub`)
    // falls back to trust-all — the pre-fix behaviour.
    let self_subject = tokens.subject();
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
        let Some(client) = connect_now(&gateway_url, &tokens).await else {
            // Already logged. The next tick is the retry; the poller's whole
            // job is to be the self-healing fallback.
            continue;
        };
        last_poll_ms.store(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
            std::sync::atomic::Ordering::Relaxed,
        );
        match client.list_inbox(ListInboxRequest::default()).await {
            Ok(resp) => {
                for event in &resp.events {
                    let trigger = match self_subject.as_deref() {
                        Some(subj) => Trigger::from_event_gated(event, tenant.clone(), subj),
                        None => Trigger::from_event(event, tenant.clone()),
                    };
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

/// A cursor over one label's event log — what the runner's subscribers
/// read (`escurel:review`, `escurel:run-control`). [`LabelTail::catch_up`]
/// pages to the END of the label without acting, so a fresh runner never
/// replays a tenant's history (a stale cancel must not fire after a
/// restart; a promotion made while no runner listened is not cascaded — a
/// durable cursor is the follow-up); [`LabelTail::poll`] returns what
/// arrived since. Best-effort: a failed read is retried next tick.
struct LabelTail {
    label: &'static str,
    cursor: Option<String>,
    /// Set once the boot catch-up reached the END of the label. Until it
    /// does, `poll` keeps catching up and acts on nothing — a transient
    /// gateway error at boot must not turn history into fresh requests
    /// (codex second-opinion review of P2, P1).
    caught_up: bool,
}

impl LabelTail {
    fn new(label: &'static str) -> Self {
        Self {
            label,
            cursor: None,
            caught_up: false,
        }
    }

    fn request(&self) -> ListEventsRequest {
        ListEventsRequest {
            label_skill: self.label.to_owned(),
            include_system: true,
            limit: 1000,
            cursor: self.cursor.clone().unwrap_or_default(),
            ..Default::default()
        }
    }

    async fn catch_up(&mut self, client: &Client) {
        loop {
            match client.list_events(self.request()).await {
                Ok(page) => {
                    if let Some(c) = page.resume_cursor {
                        self.cursor = Some(c);
                    }
                    if page.next_cursor.is_none() {
                        self.caught_up = true;
                        return;
                    }
                }
                Err(e) => {
                    tracing::warn!(target: "escurel_runner", label = self.label, error = %e, "tail: catch-up failed; will retry before acting");
                    return;
                }
            }
        }
    }

    async fn poll(&mut self, client: &Client) -> Vec<escurel_types::Event> {
        if !self.caught_up {
            self.catch_up(client).await;
            // Whatever was read while catching up is history, not requests.
            return Vec::new();
        }
        match client.list_events(self.request()).await {
            Ok(page) => {
                if let Some(c) = page.resume_cursor {
                    self.cursor = Some(c);
                }
                page.events
            }
            Err(e) => {
                tracing::warn!(target: "escurel_runner", label = self.label, error = %e, "tail: poll failed; will retry");
                Vec::new()
            }
        }
    }
}

/// The promotion tail (workbench backend P2-1). Every `interval` it reads
/// what arrived under `escurel:review` and, for each `draft-promoted`,
/// cascades from the promoted page under the drafting run's lineage: the
/// ledger names the run by the draft's trigger event, the trigger event
/// itself (re-read by id, through the same lineage trust gate the poller
/// applies) is the parent, and the cascade id is one per draft, so a
/// retried decision, a changeset's paired event or a restart cascades once.
/// Best-effort and non-panicking, like the poller: the gateway stays the
/// record; this only notifies.
async fn promotion_tail_loop(
    gateway_url: String,
    tenant: String,
    tokens: Arc<escurel_runner_core::TokenSource>,
    interval: std::time::Duration,
    ledger: Arc<Ledger>,
    draining: Arc<std::sync::atomic::AtomicBool>,
) {
    use escurel_runner_core::{
        REVIEW_LABEL, cascade_event_id, emit_cascade_with_id, promoted_draft,
    };

    let Some(client) = connect_now(&gateway_url, &tokens).await else {
        tracing::error!(
            target: "escurel_runner",
            "promotion tail could not build a gateway client; promotions will not cascade"
        );
        return;
    };
    let self_subject = tokens.subject();
    let mut tail = LabelTail::new(REVIEW_LABEL);
    tail.catch_up(&client).await;
    tracing::info!(target: "escurel_runner", tenant = %tenant, "promotion tail started");

    let mut ticker = tokio::time::interval(interval);
    loop {
        ticker.tick().await;
        if draining.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        let Some(client) = connect_now(&gateway_url, &tokens).await else {
            continue;
        };
        for event in tail.poll(&client).await {
            let Some(promoted) = promoted_draft(&event) else {
                continue;
            };
            // The run that proposed the draft, by its trigger event.
            let run = match ledger.get_run(&tenant, &promoted.trigger_event_id) {
                Ok(Some(rec)) => rec,
                Ok(None) => {
                    tracing::debug!(
                        target: "escurel_runner",
                        draft_id = %promoted.draft_id,
                        trigger = %promoted.trigger_event_id,
                        "promotion tail: no run of ours behind this draft; not cascading"
                    );
                    continue;
                }
                Err(e) => {
                    tracing::warn!(target: "escurel_runner", error = %e, "promotion tail: ledger lookup failed");
                    continue;
                }
            };
            // The trigger event itself is the cascade's parent — re-read by
            // id so its lineage rides through the same trust gate the
            // poller applies.
            let trigger_event = match client
                .list_events(ListEventsRequest {
                    event_id: Some(promoted.trigger_event_id.clone()),
                    ..Default::default()
                })
                .await
            {
                Ok(page) => page.events.into_iter().next(),
                Err(e) => {
                    tracing::warn!(target: "escurel_runner", error = %e, "promotion tail: trigger read failed");
                    continue;
                }
            };
            let Some(trigger_event) = trigger_event else {
                continue;
            };
            let trigger = match self_subject.as_deref() {
                Some(subj) => Trigger::from_event_gated(&trigger_event, tenant.clone(), subj),
                None => Trigger::from_event(&trigger_event, tenant.clone()),
            };
            let effect = escurel_runner_core::ConfirmedEffect {
                instance_page_id: promoted.target_page_id.clone(),
                version: "promoted".to_owned(),
                held: false,
                result_ref: None,
            };
            match emit_cascade_with_id(
                &client,
                &trigger,
                &run.run_id,
                &effect,
                Some(cascade_event_id(&promoted.draft_id)),
            )
            .await
            {
                Ok(CascadeOutcome::Emitted {
                    event_id,
                    label_skill,
                }) => tracing::info!(
                    target: "escurel_runner",
                    draft_id = %promoted.draft_id,
                    parent_run_id = %run.run_id,
                    cascaded_event_id = %event_id,
                    label_skill = %label_skill,
                    "promotion tail: a promoted draft cascaded under its run's lineage"
                ),
                Ok(CascadeOutcome::NotAllowed { produced_skill }) => tracing::info!(
                    target: "escurel_runner",
                    event_id = %trigger.event_id,
                    produced_skill = %produced_skill,
                    "cascade: the skill's `actions` do not include the produced skill; no follow-on"
                ),
                Ok(CascadeOutcome::DepthCapped { max_depth }) => tracing::info!(
                    target: "escurel_runner",
                    event_id = %trigger.event_id,
                    max_depth,
                    "cascade: the skill's `cascade.max_depth` is reached; no follow-on"
                ),
                Ok(CascadeOutcome::NotCrossSkill) => tracing::debug!(
                    target: "escurel_runner",
                    draft_id = %promoted.draft_id,
                    "promotion tail: promoted page is not a cross-skill change; no follow-on"
                ),
                Err(e) => tracing::warn!(
                    target: "escurel_runner",
                    draft_id = %promoted.draft_id,
                    error = %e,
                    "promotion tail: cascade emit failed (will not retry)"
                ),
            }
        }
    }
}

/// The label a control request is captured under, and the one it is
/// answered under.
const RUN_CONTROL_LABEL: &str = "escurel:run-control";
const RUN_CONTROL_RESULT_LABEL: &str = "escurel:run-control-result";

/// What the runner did with a control request.
struct ControlOutcome {
    outcome: &'static str,
    detail: Option<String>,
    new_run_id: Option<String>,
}

impl ControlOutcome {
    fn done(outcome: &'static str) -> Self {
        Self {
            outcome,
            detail: None,
            new_run_id: None,
        }
    }
    fn refused(detail: impl Into<String>) -> Self {
        Self {
            outcome: "refused",
            detail: Some(detail.into()),
            new_run_id: None,
        }
    }
}

/// Act on one authorised control request (the gateway already decided who
/// may ask what — P2-2; this trusts `provenance.control`, which only the
/// gateway writes).
async fn act_on_control(
    state: &AppState,
    tenant: &str,
    control: &serde_json::Value,
) -> ControlOutcome {
    let text = |k: &str| {
        control[k]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    let Some(action) = text("action") else {
        return ControlOutcome::refused("no action");
    };
    match action.as_str() {
        "cancel" => {
            let Some(run_id) = text("run_id") else {
                return ControlOutcome::refused("cancel names a run_id");
            };
            if state.cancels.cancel(&run_id, text("reason").as_deref()) {
                return ControlOutcome::done("cancelled");
            }
            let detail = match state.ledger.find_run(&run_id) {
                Ok(Some(rec)) => format!("run is {}", rec.status.as_str()),
                Ok(None) => "unknown run".to_owned(),
                Err(e) => format!("ledger: {e}"),
            };
            ControlOutcome {
                outcome: "not_live",
                detail: Some(detail),
                new_run_id: None,
            }
        }
        "retry" => {
            let Some(run_id) = text("run_id") else {
                return ControlOutcome::refused("retry names a run_id");
            };
            match state.ledger.retry_run(&run_id) {
                Ok((run_tenant, event_id)) => {
                    enqueue_requeued(state, &run_tenant, &event_id).await;
                    let new_run_id = state
                        .ledger
                        .get_run(&run_tenant, &event_id)
                        .ok()
                        .flatten()
                        .map(|r| r.run_id);
                    ControlOutcome {
                        outcome: "requeued",
                        detail: None,
                        new_run_id,
                    }
                }
                Err(e) => ControlOutcome::refused(format!("not retriable: {e}")),
            }
        }
        "pause" => {
            state.governor.pause(tenant);
            ControlOutcome::done("paused")
        }
        "resume" => {
            state.governor.resume(tenant);
            ControlOutcome::done("resumed")
        }
        "requeue" => {
            let Some(event_id) = text("event_id") else {
                return ControlOutcome::refused("requeue names an event_id");
            };
            match state.ledger.requeue_dead_letter_by_event(tenant, &event_id) {
                Ok(_) => {
                    enqueue_requeued(state, tenant, &event_id).await;
                    let new_run_id = state
                        .ledger
                        .get_run(tenant, &event_id)
                        .ok()
                        .flatten()
                        .map(|r| r.run_id);
                    ControlOutcome {
                        outcome: "requeued",
                        detail: None,
                        new_run_id,
                    }
                }
                Err(e) => ControlOutcome::refused(format!("not dead-lettered: {e}")),
            }
        }
        other => ControlOutcome::refused(format!("unknown action `{other}`")),
    }
}

/// The run-control subscriber (workbench backend P2-3b). Every `interval`
/// it reads what arrived under `escurel:run-control` since the last poll,
/// acts on each request, and answers it once under
/// `escurel:run-control-result` — a system event in the run's own record
/// (`provenance.runner.run_id`), on the page the request was filed on, with
/// a deterministic id per request so a re-read never answers twice. Like
/// the promotion tail it catches up to the end of the label on boot
/// without acting: a stale cancel must not fire after a restart.
async fn control_tail_loop(
    state: AppState,
    gateway_url: String,
    tenant: String,
    tokens: Arc<escurel_runner_core::TokenSource>,
    interval: std::time::Duration,
) {
    let Some(client) = connect_now(&gateway_url, &tokens).await else {
        tracing::error!(
            target: "escurel_runner",
            "run-control tail could not build a gateway client; controls will not be acted on"
        );
        return;
    };
    let mut tail = LabelTail::new(RUN_CONTROL_LABEL);
    tail.catch_up(&client).await;
    tracing::info!(target: "escurel_runner", tenant = %tenant, "run-control tail started");

    let mut ticker = tokio::time::interval(interval);
    loop {
        ticker.tick().await;
        if state.draining.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        let Some(client) = connect_now(&gateway_url, &tokens).await else {
            continue;
        };
        for event in tail.poll(&client).await {
            let control = &event.provenance["control"];
            if !control.is_object() {
                continue;
            }
            let action = control["action"].as_str().unwrap_or("").to_owned();
            let result = act_on_control(&state, &tenant, control).await;
            tracing::info!(
                target: "escurel_runner",
                request = %event.event_id,
                action = %action,
                outcome = result.outcome,
                detail = ?result.detail,
                "run-control: acted"
            );
            let run_id = result
                .new_run_id
                .clone()
                .or_else(|| control["run_id"].as_str().map(str::to_owned));
            let mut body = json!({
                "action": action,
                "outcome": result.outcome,
                "run_id": control["run_id"],
                "detail": result.detail,
            });
            if let Some(n) = &result.new_run_id {
                body["new_run_id"] = json!(n);
            }
            let mut runner = json!({});
            if let Some(r) = &run_id {
                runner["run_id"] = json!(r);
            }
            if !event.root_event_id.is_empty() {
                runner["root_event_id"] = json!(event.root_event_id);
            }
            let answer = client
                .capture_event(escurel_client::CaptureEventRequest {
                    event_id: format!("run-control-result:{}", event.event_id),
                    source: "escurel-runner".to_owned(),
                    mime: "application/json".to_owned(),
                    label_skill: RUN_CONTROL_RESULT_LABEL.to_owned(),
                    instance_page_id: event.instance_page_id.clone(),
                    title: action.clone(),
                    body: body.to_string(),
                    provenance: json!({
                        "runner": runner,
                        "control": {
                            "request_event_id": event.event_id,
                            "requested_by": control["requested_by"],
                            "action": action,
                        },
                    }),
                    kind: "system".to_owned(),
                    ..Default::default()
                })
                .await;
            if let Err(e) = answer {
                tracing::warn!(
                    target: "escurel_runner",
                    request = %event.event_id,
                    error = %e,
                    "run-control: could not write the result (acted anyway)"
                );
            }
        }
    }
}

/// Who is reporting, for `escurel:runner-status`.
struct StatusIdentity {
    runner_id: String,
    version: String,
    harness: String,
}

/// The runner's health as one JSON body (workbench backend P2-4). Split
/// into the part that means "something changed" (`stable`) and the
/// clock-derived rest, so a change is detected by comparing `stable`.
fn status_snapshot(
    state: &AppState,
    tenant: &str,
    who: &StatusIdentity,
    started: Instant,
) -> (serde_json::Value, serde_json::Value) {
    let count = |s: RunStatus| state.ledger.count_all_by_status(s).unwrap_or(0);
    let mut live: Vec<serde_json::Value> = state
        .cancels
        .live()
        .into_iter()
        .map(|run_id| {
            let rec = state.ledger.find_run(&run_id).ok().flatten();
            json!({
                "run_id": run_id,
                "event_id": rec.as_ref().map(|r| r.event_id.clone()),
                "instance_page_id": rec.as_ref().and_then(|r| r.instance_page_id.clone()),
            })
        })
        .collect();
    live.sort_by(|a, b| a["run_id"].as_str().cmp(&b["run_id"].as_str()));
    let stable = json!({
        "runner_id": who.runner_id,
        "version": who.version,
        "harness": who.harness,
        "tenant": tenant,
        "live_runs": live,
        "paused_tenants": state.governor.paused_tenants(),
        "runs": {
            "pending": count(RunStatus::Pending),
            "processed": count(RunStatus::Processed),
            "failed": count(RunStatus::Failed),
            "dead_letter": count(RunStatus::DeadLetter),
            "cancelled": count(RunStatus::Cancelled),
            "planned": count(RunStatus::Planned),
            "total": state.ledger.count_all_runs().unwrap_or(0),
        },
        "throttled": {
            "runs_per_min": state.governor.throttled(escurel_runner_core::ThrottleReason::RunsPerMin),
            "max_concurrent": state.governor.throttled(escurel_runner_core::ThrottleReason::MaxConcurrent),
            "paused": state.governor.throttled(escurel_runner_core::ThrottleReason::Paused),
        },
        "harness_permits_available": state.governor.harness_permits_available(),
        "draining": state.draining.load(std::sync::atomic::Ordering::Relaxed),
    });
    let last = state
        .last_poll_ms
        .load(std::sync::atomic::Ordering::Relaxed);
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let mut body = stable.clone();
    body["uptime_s"] = json!(started.elapsed().as_secs());
    body["last_poll_age_ms"] = if last == 0 {
        serde_json::Value::Null
    } else {
        json!(now_ms.saturating_sub(last))
    };
    (stable, body)
}

/// The status reporter (workbench backend P2-4). Writes this runner's
/// health as unassigned `escurel:runner-status` system events — `started`
/// once, `changed` as soon as the stable part of the snapshot differs
/// (checked every 250 ms), `heartbeat` every `interval` while nothing does,
/// `stopping` on drain. The gateway keeps a tenant's last 50 rows; the
/// workbench reads the latest with `list_events { label_skill,
/// newest_first: true, limit: 1 }`. Best-effort: a refused write is the
/// next tick's problem.
async fn status_loop(
    state: AppState,
    gateway_url: String,
    tenant: String,
    tokens: Arc<escurel_runner_core::TokenSource>,
    who: StatusIdentity,
    interval: std::time::Duration,
) {
    const CHECK: std::time::Duration = std::time::Duration::from_millis(250);
    let started = Instant::now();
    let mut last_stable: Option<serde_json::Value> = None;
    let mut last_sent = Instant::now();
    let mut ticker = tokio::time::interval(CHECK.min(interval));
    loop {
        ticker.tick().await;
        let draining = state.draining.load(std::sync::atomic::Ordering::Relaxed);
        let (stable, body) = status_snapshot(&state, &tenant, &who, started);
        let title = if draining {
            "stopping"
        } else if last_stable.is_none() {
            "started"
        } else if last_stable.as_ref() != Some(&stable) {
            "changed"
        } else if last_sent.elapsed() >= interval {
            "heartbeat"
        } else {
            continue;
        };
        let Some(client) = connect_now(&gateway_url, &tokens).await else {
            continue;
        };
        let written = client
            .capture_event(escurel_client::CaptureEventRequest {
                source: "escurel-runner".to_owned(),
                mime: "application/json".to_owned(),
                label_skill: "escurel:runner-status".to_owned(),
                title: title.to_owned(),
                body: body.to_string(),
                provenance: json!({ "runner_status": { "runner_id": who.runner_id, "tenant": tenant } }),
                kind: "system".to_owned(),
                ..Default::default()
            })
            .await;
        match written {
            Ok(_) => {
                last_stable = Some(stable);
                last_sent = Instant::now();
            }
            Err(e) => tracing::warn!(
                target: "escurel_runner",
                error = %e,
                title,
                "runner-status: write failed (will retry)"
            ),
        }
        if draining {
            return;
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
    tokens: Arc<escurel_runner_core::TokenSource>,
    interval: std::time::Duration,
    draining: Arc<std::sync::atomic::AtomicBool>,
) {
    // Boot probe; the client is rebuilt each tick so the minted bearer stays
    // live (see `connect_now`).
    if connect_now(&gateway_url, &tokens).await.is_none() {
        tracing::error!(target: "escurel_runner", "lint tick could not build a gateway client; disabled");
        return;
    }
    let secs = interval.as_secs().max(1);
    tracing::info!(target: "escurel_runner", tenant = %tenant, interval_ms = interval.as_millis() as u64, "lint tick started");
    let mut ticker = tokio::time::interval(interval);
    loop {
        ticker.tick().await;
        if draining.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        let Some(client) = connect_now(&gateway_url, &tokens).await else {
            // Already logged; the next tick retries.
            continue;
        };
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
            Ok(_) => {
                tracing::info!(target: "escurel_runner", window, event_id = %event_id, "lint tick: invocation captured")
            }
            Err(e) => {
                tracing::warn!(target: "escurel_runner", error = %e, "lint tick: capture_event failed; will retry next tick")
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use escurel_runner_core::{Lineage, OPERATION_STATUS_LABEL, QuotaLimits};

    /// Build the minimal `gate_and_enqueue` dependency set with limits generous
    /// enough that only the reserved-label guard can reject a trigger. The
    /// [`DispatchConsumer`] is returned so the caller keeps it alive — dropping
    /// it closes the queue channel and every `enqueue` then reports not-sent.
    #[allow(clippy::type_complexity)]
    fn gate_deps() -> (
        Ledger,
        DispatchQueue,
        LoopLimits,
        Governor,
        Metrics,
        InflightSlots,
        DispatchConsumer,
    ) {
        let dir = tempfile::tempdir().expect("tempdir");
        // Leak the tempdir so the sqlite file outlives the test body; the
        // process exits at test end and reclaims it.
        let path = dir.keep().join("ledger.sqlite");
        let ledger = Ledger::open(path).expect("open ledger");
        let (queue, consumer) = DispatchQueue::new(16, 256);
        let limits = LoopLimits {
            max_depth: 16,
            max_runs_per_root: 64,
        };
        let governor = Governor::new(QuotaLimits {
            runs_per_min: 1000,
            max_concurrent: 1000,
            max_harness_procs: 1000,
        });
        let metrics = Metrics::new();
        let inflight: InflightSlots =
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
        (ledger, queue, limits, governor, metrics, inflight, consumer)
    }

    fn trigger_with_label(event_id: &str, label: &str) -> Trigger {
        Trigger {
            manual: None,
            is_system: false,
            tenant: "acme".to_owned(),
            event_id: event_id.to_owned(),
            label_skill: label.to_owned(),
            instance_page_id: None,
            lineage: Lineage::root(event_id.to_owned()),
            workflow: None,
            content_hash: None,
        }
    }

    /// F1: a reserved operation-status event is dropped at the enqueue
    /// chokepoint and creates NO ledger row — so recording a status can never
    /// spawn (and dead-letter) a run. Deterministic: exercises the guard
    /// directly, independent of the poller/webhook timing race that hid the bug.
    #[test]
    fn operation_status_event_creates_no_ledger_row() {
        let (ledger, queue, limits, governor, metrics, inflight, _consumer) = gate_deps();
        let admitted = gate_and_enqueue(
            &ledger,
            &queue,
            &limits,
            &governor,
            &metrics,
            &inflight,
            trigger_with_label("evt-status-1", OPERATION_STATUS_LABEL),
            "test",
        );
        assert!(!admitted, "a status event must not be admitted");
        assert_eq!(
            ledger.count_all_runs().expect("count runs"),
            0,
            "a status event must create no ledger row"
        );
    }

    /// Phase 1 (`/trigger` binding): a single-tenant runner takes its OWN
    /// tenant as authoritative and refuses a body that names a different one, so
    /// a party holding the webhook secret cannot drive another tenant's runs
    /// through this runner. Absent/equal body tenant is accepted; with no
    /// configured tenant the body value passes through (dev/legacy).
    #[test]
    fn trigger_tenant_is_bound_to_the_runner_not_the_body() {
        // Configured runner: own tenant wins; a matching or absent body is fine.
        assert_eq!(
            resolve_trigger_tenant(Some("acme"), Some("acme")).unwrap(),
            "acme"
        );
        assert_eq!(resolve_trigger_tenant(Some("acme"), None).unwrap(), "acme");
        // A body naming a DIFFERENT tenant is rejected (mis-routed / forged).
        assert_eq!(
            resolve_trigger_tenant(Some("acme"), Some("evil")),
            Err("evil".to_owned())
        );
        // Dev/legacy: no configured tenant → body value (or empty) passes.
        assert_eq!(resolve_trigger_tenant(None, Some("acme")).unwrap(), "acme");
        assert_eq!(resolve_trigger_tenant(None, None).unwrap(), "");
    }

    /// Positive control: an ordinary labelled trigger is NOT dropped by the
    /// guard — it creates exactly one ledger row. Proves the guard is scoped to
    /// the reserved label and does not swallow real work.
    #[test]
    fn ordinary_event_creates_one_ledger_row() {
        let (ledger, queue, limits, governor, metrics, inflight, _consumer) = gate_deps();
        let admitted = gate_and_enqueue(
            &ledger,
            &queue,
            &limits,
            &governor,
            &metrics,
            &inflight,
            trigger_with_label("evt-real-1", "research-angle"),
            "test",
        );
        assert!(admitted, "an ordinary event must be admitted");
        assert_eq!(
            ledger.count_all_runs().expect("count runs"),
            1,
            "an ordinary event must create exactly one ledger row"
        );
    }

    /// A `failed` run that is re-delivered must actually re-dispatch — and
    /// must never be left stranded `pending`.
    ///
    /// The ledger deliberately re-claims a `failed` row (reset to `pending`,
    /// fresh run id, `Created`) so a transient failure is re-drivable rather
    /// than wedged for ever (#157). But the in-memory seen-set is only ever
    /// cleared by an operator requeue, never on completion — so the event id
    /// of every run this process has dispatched stays in it. The re-claim
    /// therefore met a seen-set that still held the id, `enqueue` answered
    /// `Duplicate`, and nothing dispatched.
    ///
    /// The row was left `pending` by the re-claim, which is the trap: nothing
    /// can move it (no dispatch), and every later delivery reads `pending` and
    /// returns `InFlight` → dropped. The verdict a human would read is also
    /// gone, because the re-claim cleared it. Observed as
    /// `{"total":1,"terminal":0,"succeeded":0,"failed":0}` sitting unchanged
    /// for four minutes while the runner's own log said `recorded failed`.
    ///
    /// Deterministic: drives the gate directly, so it does not depend on the
    /// poller re-polling inside the window that made this a flake.
    #[test]
    fn a_failed_run_redelivered_dispatches_instead_of_wedging_pending() {
        let (ledger, queue, limits, governor, metrics, inflight, _consumer) = gate_deps();
        let deliver = || {
            gate_and_enqueue(
                &ledger,
                &queue,
                &limits,
                &governor,
                &metrics,
                &inflight,
                trigger_with_label("evt-redrive-1", "research-angle"),
                "test",
            )
        };

        assert!(deliver(), "the first delivery must dispatch");
        // The run finishes with a retriable failure — the state #157 makes
        // re-drivable, and the state this event is in when the poller,
        // finding the event still in the inbox, delivers it again.
        let run = ledger
            .get_run("acme", "evt-redrive-1")
            .expect("get run")
            .expect("a row for the first delivery");
        let run_id = run.run_id.clone();
        ledger
            .mark(&RunId(run.run_id), RunStatus::Failed)
            .expect("mark failed");

        assert!(
            deliver(),
            "a re-delivered failed run must dispatch: the ledger is the \
             idempotency authority and it re-claimed the row, so the seen-set \
             fast-path in front of it must not veto the retry"
        );
        let after = ledger
            .get_run("acme", "evt-redrive-1")
            .expect("get run")
            .expect("the row survives the re-claim");
        assert_eq!(
            after.status,
            RunStatus::Pending,
            "the re-claimed row is in flight again under a fresh run id"
        );
        assert_ne!(
            after.run_id, run_id,
            "the re-claim mints a fresh run id, so the retry is a run of its \
             own rather than an edit of the one that failed"
        );
    }

    /// A trigger the gate admits but cannot queue must be left RETRIABLE.
    ///
    /// The row is already `pending` when `enqueue` is reached, so an outcome
    /// that never reaches the channel and is not reset leaves a run nothing
    /// can move: the dispatch loop never sees it, and every later delivery
    /// reads `pending` and drops it as `InFlight`.
    ///
    /// A regression guard rather than a reproduction: the reachable not-sent
    /// outcomes (`Full`, and a closed channel, which reports as `Full`) were
    /// already reset before the `Duplicate` fix, and with the stale seen-set
    /// entry now dropped ahead of the enqueue, `Duplicate` is no longer
    /// reachable from this arm at all. What this pins is the invariant that
    /// made the wedge possible — a row must never be left `pending` with
    /// nothing queued to move it — so an outcome added to `EnqueueOutcome`
    /// later cannot quietly reintroduce it.
    ///
    /// Dropping the consumer closes the channel, which is the cheapest way to
    /// make every `enqueue` report not-sent.
    #[test]
    fn a_trigger_that_cannot_be_queued_is_left_retriable_not_pending() {
        let (ledger, queue, limits, governor, metrics, inflight, consumer) = gate_deps();
        drop(consumer);

        let admitted = gate_and_enqueue(
            &ledger,
            &queue,
            &limits,
            &governor,
            &metrics,
            &inflight,
            trigger_with_label("evt-unqueueable-1", "research-angle"),
            "test",
        );
        assert!(
            !admitted,
            "a trigger that never reached the channel is not admitted"
        );
        let row = ledger
            .get_run("acme", "evt-unqueueable-1")
            .expect("get run")
            .expect("the gate created a row before enqueueing");
        assert_eq!(
            row.status,
            RunStatus::Failed,
            "an un-queueable trigger must be reset to retriable `failed` so \
             the poller re-drives it; left `pending` it is wedged for ever"
        );
    }
}
