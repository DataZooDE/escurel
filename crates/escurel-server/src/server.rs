//! axum HTTP gateway + tonic gRPC mirror. Both transports share
//! the same `AppState` (indexer + verifier + quota) so the auth
//! and quota policies enforced on `POST /mcp` are mirrored 1:1 by
//! the gRPC interceptors.

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use escurel_admin::TenantStore;
use escurel_auth::OidcVerifier;
use escurel_crdt::CrdtBackend;
use escurel_embed::{Embedder, ReloadableEmbedder};
use escurel_index::Indexer;
use escurel_obs::{Metrics, TelemetryConfig, init_telemetry};
use escurel_proto::v1::escurel_admin_server::EscurelAdminServer;
use escurel_proto::v1::escurel_server::EscurelServer;
use escurel_quota::QuotaManager;
use serde_json::json;
use thiserror::Error;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tower_http::cors::CorsLayer;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;

use crate::grpc::{EscurelAdminGrpc, EscurelGrpc};
use crate::health::{AlwaysReady, ReadinessProbe, ReadinessReport};
use crate::mcp::mcp;
use crate::session::SessionManager;
use crate::ws::ws_upgrade;

/// Async factory that rebuilds the real embedder from the same
/// config the binary booted with. The `embedding_reload` admin RPC
/// calls it on demand: on `Ok((embedder, revision))` the freshly-
/// built embedder is swapped into the live [`ReloadableEmbedder`]
/// seam and `revision` is returned to the caller; on `Err` the RPC
/// returns `Status::internal` and the server stays degraded.
///
/// The factory owns (captures) the embedding config — the gRPC layer
/// never sees the original [`EscurelConfig`](crate::EscurelConfig),
/// keeping the handler thin and the config out of `AppState`. The
/// `revision` is the model id / path (or any short label the binary
/// chooses) so the admin response names *which* model is now live
/// without the `Embedder` trait having to carry a revision method.
pub type EmbedderFactory = Arc<
    dyn Fn() -> Pin<Box<dyn Future<Output = Result<(Arc<dyn Embedder>, String), String>> + Send>>
        + Send
        + Sync,
>;

/// Gateway configuration. Built by the operator (or the test
/// harness) and consumed by [`serve`].
#[derive(Clone)]
pub struct ServerConfig {
    /// HTTP listener — `0.0.0.0:8080` in production; tests pass
    /// `127.0.0.1:0` to let the OS pick a free port.
    pub listen: String,
    /// gRPC listener — `0.0.0.0:8081` in production; `None`
    /// disables the gRPC mirror entirely (useful for HTTP-only or
    /// health-only deployments). Tests pass `Some("127.0.0.1:0")`.
    pub grpc_listen: Option<String>,
    /// Returned as the body of `GET /version`. Comes from `VERSION`
    /// env var in production; tests usually pass a literal.
    pub version: String,
    /// Probe behind `/readyz`. Defaults to [`AlwaysReady`].
    pub readiness: Arc<dyn ReadinessProbe>,
    /// Per-tenant indexer. None disables the `/mcp` endpoint
    /// (useful for health-only deployments). `tools/call` returns
    /// a JSON-RPC `method not found` when absent.
    pub indexer: Option<Arc<Indexer>>,
    /// OIDC verifier. When `Some`, `/mcp` requires a valid
    /// `Authorization: Bearer <jwt>` header; `None` runs the
    /// gateway unauthenticated (dev / on-host use).
    pub verifier: Option<Arc<OidcVerifier>>,
    /// Per-tenant rate-limit + concurrency cap. When `Some`,
    /// `/mcp` debits the relevant quota dimension before
    /// dispatch. Required `verifier` to be set too (the tenant
    /// id comes from the verified token).
    pub quota: Option<Arc<QuotaManager>>,
    /// Backing store for the admin tenant-CRUD RPCs. `None`
    /// means every tenant CRUD RPC returns
    /// `Status::failed_precondition` — useful for health-only
    /// deployments and for the M3 grpc_admin stubs test (which
    /// keeps proving the role-gate without exercising the new
    /// implementation surface).
    pub tenant_store: Option<Arc<dyn TenantStore>>,
    /// Live-CRDT backend powering the `open_session` / `apply_op`
    /// / `close_session` MCP tools (and, later, the WS / gRPC bidi
    /// live channels). `None` disables live mode — the session
    /// tools return a JSON-RPC `-32603 internal` error with
    /// `"live CRDT mode not enabled on this server"`.
    pub crdt_backend: Option<Arc<dyn CrdtBackend>>,
    /// The hot-swappable embedder seam. `Some` when the binary
    /// booted the embedder behind a [`ReloadableEmbedder`] (always,
    /// in production); the `embedding_reload` admin RPC swaps a
    /// freshly-built model in here. `None` → the RPC returns
    /// `Status::failed_precondition`.
    pub embedder_reload: Option<Arc<ReloadableEmbedder>>,
    /// Rebuilds the real embedder on demand for `embedding_reload`.
    /// Paired with `embedder_reload`: when both are `Some`, the RPC
    /// calls the factory and (on success) reloads the seam. `None`
    /// → the RPC returns `Status::failed_precondition`.
    pub embedder_factory: Option<EmbedderFactory>,
    /// Directory of a built static demo bundle (Flutter web
    /// `build/web`) to serve at `/`. `Some` → the router mounts a
    /// `ServeDir` fallback so the gateway and the demo run as one
    /// process; explicit API routes (`/mcp`, `/ws`, `/healthz`, …)
    /// keep precedence, and unknown paths fall back to `index.html`
    /// for SPA client-side routing. `None` (default) → no static
    /// serving; unknown paths are 404. Set from
    /// `ESCUREL_SERVE_DEMO_DIR` by the binary.
    pub demo_dir: Option<std::path::PathBuf>,
    /// Opt-in outbound capture webhook URL (`ESCUREL_WEBHOOK_URL`).
    /// `Some` → `capture_event` fires a fire-and-forget POST of the
    /// new event to this URL; `None` (default) disables it.
    pub webhook_url: Option<String>,
    /// Dedicated Prometheus `/metrics` listener
    /// (`ESCUREL_OBSERVABILITY_METRICS_LISTEN`, default
    /// `0.0.0.0:9090`). Served on its own port — tailnet-only in the
    /// substrate — and NOT mounted on the main HTTP app. `None`
    /// disables metrics scraping entirely. Tests pass
    /// `Some("127.0.0.1:0")`.
    pub metrics_listen: Option<String>,
}

impl std::fmt::Debug for ServerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerConfig")
            .field("listen", &self.listen)
            .field("grpc_listen", &self.grpc_listen)
            .field("version", &self.version)
            .finish_non_exhaustive()
    }
}

impl ServerConfig {
    /// Minimal config for a local dev / test run: HTTP on a random
    /// port, gRPC disabled, `version = "0.0.0-dev"`, `AlwaysReady`.
    #[must_use]
    pub fn test_defaults() -> Self {
        Self {
            listen: "127.0.0.1:0".to_owned(),
            grpc_listen: None,
            version: "0.0.0-dev".to_owned(),
            readiness: Arc::new(AlwaysReady),
            indexer: None,
            verifier: None,
            quota: None,
            tenant_store: None,
            crdt_backend: None,
            embedder_reload: None,
            embedder_factory: None,
            demo_dir: None,
            webhook_url: None,
            metrics_listen: Some("127.0.0.1:0".to_owned()),
        }
    }
}

#[derive(Debug, Error)]
pub enum ServerError {
    #[error("bind {addr} failed: {source}")]
    Bind {
        addr: String,
        #[source]
        source: std::io::Error,
    },
    #[error("axum server failed: {0}")]
    Serve(#[from] std::io::Error),
}

/// Handle to a running [`serve`]. Drops cleanly: callers signal
/// shutdown via [`ServerHandle::shutdown`].
pub struct ServerHandle {
    pub local_addr: SocketAddr,
    /// Address of the gRPC listener, when configured.
    pub grpc_addr: Option<SocketAddr>,
    /// Address of the dedicated Prometheus `/metrics` listener, when
    /// configured.
    pub metrics_addr: Option<SocketAddr>,
    shutdown_tx: Option<oneshot::Sender<()>>,
    grpc_shutdown_tx: Option<oneshot::Sender<()>>,
    metrics_shutdown_tx: Option<oneshot::Sender<()>>,
    join: JoinHandle<()>,
    grpc_join: Option<JoinHandle<()>>,
    metrics_join: Option<JoinHandle<()>>,
    /// Telemetry guard. `Some` when this `serve()` call was the
    /// one that installed the global subscriber; `None` when a
    /// pre-existing subscriber (e.g. a test's) was already
    /// installed. Held for the lifetime of the server so the OTLP
    /// exporter (when configured) flushes on shutdown.
    _telemetry: Option<escurel_obs::TelemetryGuard>,
}

impl std::fmt::Debug for ServerHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerHandle")
            .field("local_addr", &self.local_addr)
            .field("grpc_addr", &self.grpc_addr)
            .finish_non_exhaustive()
    }
}

impl ServerHandle {
    /// Signal graceful shutdown on both listeners and await the
    /// server tasks. `Ok(())` on clean stop; cancelled join is
    /// silenced (tests often abort).
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(tx) = self.grpc_shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(tx) = self.metrics_shutdown_tx.take() {
            let _ = tx.send(());
        }
        let _ = self.join.await;
        if let Some(j) = self.grpc_join.take() {
            let _ = j.await;
        }
        if let Some(j) = self.metrics_join.take() {
            let _ = j.await;
        }
    }
}

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) version: String,
    pub(crate) readiness: Arc<dyn ReadinessProbe>,
    pub(crate) indexer: Option<Arc<Indexer>>,
    pub(crate) verifier: Option<Arc<OidcVerifier>>,
    pub(crate) quota: Option<Arc<QuotaManager>>,
    pub(crate) tenant_store: Option<Arc<dyn TenantStore>>,
    pub(crate) crdt_backend: Option<Arc<dyn CrdtBackend>>,
    /// Live embedder seam swapped by `embedding_reload`. `None`
    /// when no reloadable embedder is wired.
    pub(crate) embedder_reload: Option<Arc<ReloadableEmbedder>>,
    /// On-demand rebuild closure for `embedding_reload`. `None`
    /// when no factory is wired.
    pub(crate) embedder_factory: Option<EmbedderFactory>,
    /// Always present. Operations no-op (return a JSON-RPC error)
    /// when `crdt_backend` is `None`.
    pub(crate) sessions: Arc<SessionManager>,
    /// Per-process metrics registry. Handlers debit it on the
    /// `(route, status)` axis; `/metrics` renders it as the
    /// Prometheus text exposition body.
    pub(crate) metrics: Arc<Metrics>,
    /// Opt-in outbound capture webhook (`ESCUREL_WEBHOOK_URL`). When
    /// `Some`, `capture_event` fires a fire-and-forget POST of the new
    /// event; `None` (default) is a no-op.
    pub(crate) webhook: Option<crate::webhook::Webhook>,
}

/// Build the router(s) + bind + spawn the server tasks. Returns
/// once the HTTP (and optional gRPC) listener is bound so tests
/// can read back the local addresses. Both background tasks run
/// until [`ServerHandle::shutdown`] fires or the process exits.
pub async fn serve(config: ServerConfig) -> Result<ServerHandle, ServerError> {
    // Install the global JSON tracing subscriber once per process.
    // Tests install their own subscriber up front (a scoped one
    // pointing at a buffer) and the `AlreadyInstalled` error is
    // ignored — `init_telemetry` is idempotent from the caller's
    // perspective. The returned `TelemetryGuard` is parked on the
    // server handle so the OTLP exporter (when configured) flushes
    // on shutdown.
    let telemetry_guard = install_telemetry(&config);
    let metrics_registry = Arc::new(Metrics::new());
    // The gateway is "up" the moment we build state — flip the
    // liveness gauge before binding the listener so a /metrics
    // scrape that races the first request still sees `escurel_up
    // 1` as soon as the route is wired.
    metrics_registry.set_up(true);
    let state = AppState {
        version: config.version.clone(),
        readiness: Arc::clone(&config.readiness),
        indexer: config.indexer.clone(),
        verifier: config.verifier.clone(),
        quota: config.quota.clone(),
        tenant_store: config.tenant_store.clone(),
        crdt_backend: config.crdt_backend.clone(),
        embedder_reload: config.embedder_reload.clone(),
        embedder_factory: config.embedder_factory.clone(),
        sessions: Arc::new(SessionManager::new()),
        metrics: metrics_registry,
        webhook: config.webhook_url.clone().map(crate::webhook::Webhook::new),
    };

    let mut app = Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/version", get(version))
        .route("/mcp", post(mcp))
        .route("/ws", get(ws_upgrade))
        .with_state(state.clone());

    // Optional static demo bundle at `/`. Mounted as a fallback so
    // the explicit API routes above always win; `ServeDir` with a
    // `ServeFile` not-found handler gives SPA routing — an unknown
    // path (a Flutter client-side route) serves `index.html` so the
    // in-app router takes over. `serve.dart` etc. resolve as assets.
    if let Some(dir) = config.demo_dir.as_ref() {
        let index = dir.join("index.html");
        let serve_dir = ServeDir::new(dir).fallback(ServeFile::new(index));
        app = app.fallback_service(serve_dir);
        // Demo mode also relaxes CORS so a cross-origin demo client
        // can call `/mcp` — the production bundle served at `/` is
        // same-origin and doesn't need this, but the `flutter drive`
        // integration-test harness serves the app from its own
        // web-server origin. Scoped to demo mode + a tailnet-only
        // gateway, allow-any is acceptable.
        app = app.layer(CorsLayer::very_permissive());
    }

    // tower-http's TraceLayer opens one span per request
    // (`http.request`) and emits a record at completion with
    // status + latency. Layered last so it sees every route +
    // the static fallback uniformly.
    let app = app.layer(TraceLayer::new_for_http());

    let listener = TcpListener::bind(&config.listen)
        .await
        .map_err(|e| ServerError::Bind {
            addr: config.listen.clone(),
            source: e,
        })?;
    let local_addr = listener.local_addr().map_err(ServerError::Serve)?;

    let (tx, rx) = oneshot::channel();
    let join = tokio::spawn(async move {
        let serve = axum::serve(listener, app).with_graceful_shutdown(async move {
            let _ = rx.await;
        });
        let _ = serve.await;
    });

    // gRPC mirror (optional). Same AppState — same auth/quota.
    let (grpc_addr, grpc_shutdown_tx, grpc_join) = match config.grpc_listen.as_ref() {
        Some(addr) => {
            let (a, tx, j) = spawn_grpc(addr, state.clone()).await?;
            (Some(a), Some(tx), Some(j))
        }
        None => (None, None, None),
    };

    // Dedicated Prometheus `/metrics` listener (optional). Served on
    // its own port — substrate scrapes it over the tailnet — so the
    // public HTTP app never exposes `/metrics`. Same AppState → same
    // registry the request handlers debit.
    let (metrics_addr, metrics_shutdown_tx, metrics_join) = match config.metrics_listen.as_ref() {
        Some(addr) => {
            let (a, tx, j) = spawn_metrics(addr, state.clone()).await?;
            (Some(a), Some(tx), Some(j))
        }
        None => (None, None, None),
    };

    Ok(ServerHandle {
        local_addr,
        grpc_addr,
        metrics_addr,
        shutdown_tx: Some(tx),
        grpc_shutdown_tx,
        metrics_shutdown_tx,
        join,
        grpc_join,
        metrics_join,
        _telemetry: telemetry_guard,
    })
}

/// Bind a minimal axum app exposing only `GET /metrics` on `addr` and
/// spawn it with graceful shutdown. Returns the bound address, a
/// shutdown sender, and the join handle.
async fn spawn_metrics(
    addr: &str,
    state: AppState,
) -> Result<(SocketAddr, oneshot::Sender<()>, JoinHandle<()>), ServerError> {
    let app = Router::new()
        .route("/metrics", get(metrics))
        .with_state(state);
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|e| ServerError::Bind {
            addr: addr.to_owned(),
            source: e,
        })?;
    let bound = listener.local_addr().map_err(ServerError::Serve)?;
    let (tx, rx) = oneshot::channel();
    let join = tokio::spawn(async move {
        let serve = axum::serve(listener, app).with_graceful_shutdown(async move {
            let _ = rx.await;
        });
        let _ = serve.await;
    });
    Ok((bound, tx, join))
}

/// Install the process-global JSON tracing subscriber. Errors
/// returned by `init_telemetry` (notably `AlreadyInstalled`) are
/// swallowed: tests pre-install their own subscriber, and a
/// second `serve()` call in the same process must not panic. The
/// production path's first call gets the real installer; every
/// later call (or a test's `serve()` after the test installed its
/// own subscriber) silently keeps the existing global.
fn install_telemetry(config: &ServerConfig) -> Option<escurel_obs::TelemetryGuard> {
    let env = std::env::var("ESCUREL_ENV").unwrap_or_else(|_| "dev".to_owned());
    let cfg = TelemetryConfig {
        app: "escurel".to_owned(),
        env,
        version: config.version.clone(),
        otlp_endpoint: std::env::var("ESCUREL_OTLP_ENDPOINT").ok(),
        json_logs: true,
    };
    init_telemetry(cfg).ok()
}

async fn spawn_grpc(
    addr: &str,
    state: AppState,
) -> Result<(SocketAddr, oneshot::Sender<()>, JoinHandle<()>), ServerError> {
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|e| ServerError::Bind {
            addr: addr.to_owned(),
            source: e,
        })?;
    let local_addr = listener.local_addr().map_err(ServerError::Serve)?;
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);

    let agent_svc = EscurelServer::new(EscurelGrpc::new(state.clone()));
    let admin_svc = EscurelAdminServer::new(EscurelAdminGrpc::new(state));

    let (tx, rx) = oneshot::channel();
    let join = tokio::spawn(async move {
        let _ = tonic::transport::Server::builder()
            // Open a span per RPC. tonic 0.12 honours the Tower
            // `trace` layer through `Server::layer`. Records emit
            // a structured `http.request` span with method + uri
            // and a completion event at the end.
            .layer(TraceLayer::new_for_grpc())
            .add_service(agent_svc)
            .add_service(admin_svc)
            .serve_with_incoming_shutdown(incoming, async move {
                let _ = rx.await;
            })
            .await;
    });
    Ok((local_addr, tx, join))
}

// --- handlers ---------------------------------------------------

async fn healthz() -> impl IntoResponse {
    (StatusCode::OK, "OK")
}

async fn readyz(State(state): State<AppState>) -> impl IntoResponse {
    let report: ReadinessReport = state.readiness.probe().await;
    if report.all_up() {
        (StatusCode::OK, "OK").into_response()
    } else {
        let body = json!({
            "ready": false,
            "components": report,
        });
        (StatusCode::SERVICE_UNAVAILABLE, axum::Json(body)).into_response()
    }
}

async fn version(State(state): State<AppState>) -> impl IntoResponse {
    (StatusCode::OK, state.version.clone())
}

async fn metrics(State(state): State<AppState>) -> impl IntoResponse {
    // Sample gauge-style metrics at scrape time.
    state
        .metrics
        .set_live_sessions(state.sessions.open_count() as i64);
    let body = state.metrics.render_prometheus();
    (
        StatusCode::OK,
        [("content-type", "text/plain; version=0.0.4")],
        body,
    )
}
