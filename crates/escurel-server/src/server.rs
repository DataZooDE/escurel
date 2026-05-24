//! axum-based HTTP gateway. Composes the health surface for now;
//! the MCP-over-HTTP dispatcher + WebSocket land in later PRs.

use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use escurel_auth::OidcVerifier;
use escurel_index::Indexer;
use escurel_quota::QuotaManager;
use serde_json::json;
use thiserror::Error;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::health::{AlwaysReady, ReadinessProbe, ReadinessReport};
use crate::mcp::mcp;

/// Gateway configuration. Built by the operator (or the test
/// harness) and consumed by [`serve`].
#[derive(Clone)]
pub struct ServerConfig {
    /// `0.0.0.0:8080` in production; tests pass `127.0.0.1:0` to
    /// let the OS pick a free port and read it back from the
    /// `ServerHandle`.
    pub listen: String,
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
}

impl std::fmt::Debug for ServerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerConfig")
            .field("listen", &self.listen)
            .field("version", &self.version)
            .finish_non_exhaustive()
    }
}

impl ServerConfig {
    /// Minimal config for a local dev / test run on a random port,
    /// `version = "0.0.0-dev"`, and the `AlwaysReady` probe.
    #[must_use]
    pub fn test_defaults() -> Self {
        Self {
            listen: "127.0.0.1:0".to_owned(),
            version: "0.0.0-dev".to_owned(),
            readiness: Arc::new(AlwaysReady),
            indexer: None,
            verifier: None,
            quota: None,
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
    pub local_addr: std::net::SocketAddr,
    shutdown_tx: Option<oneshot::Sender<()>>,
    join: JoinHandle<()>,
}

impl std::fmt::Debug for ServerHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerHandle")
            .field("local_addr", &self.local_addr)
            .finish_non_exhaustive()
    }
}

impl ServerHandle {
    /// Signal graceful shutdown and await the server task.
    /// `Ok(())` on clean stop; cancelled join is silenced (tests
    /// often abort).
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        let _ = self.join.await;
    }
}

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) version: String,
    pub(crate) readiness: Arc<dyn ReadinessProbe>,
    pub(crate) indexer: Option<Arc<Indexer>>,
    pub(crate) verifier: Option<Arc<OidcVerifier>>,
    pub(crate) quota: Option<Arc<QuotaManager>>,
}

/// Build the router + bind + spawn the axum server. Returns once
/// the listener is bound (and exposes its `local_addr` so tests can
/// dial back); the background task runs until [`ServerHandle::shutdown`]
/// fires or the process exits.
pub async fn serve(config: ServerConfig) -> Result<ServerHandle, ServerError> {
    let state = AppState {
        version: config.version.clone(),
        readiness: Arc::clone(&config.readiness),
        indexer: config.indexer.clone(),
        verifier: config.verifier.clone(),
        quota: config.quota.clone(),
    };

    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/version", get(version))
        .route("/metrics", get(metrics))
        .route("/mcp", post(mcp))
        .with_state(state);

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
        // We swallow the inner serve error in the spawned task —
        // shutdown is cooperative, and a clean shutdown returns Ok.
        let _ = serve.await;
    });

    Ok(ServerHandle {
        local_addr,
        shutdown_tx: Some(tx),
        join,
    })
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

async fn metrics() -> impl IntoResponse {
    // Placeholder Prometheus text body. The OTel exporter (M5)
    // replaces this with real numbers; for now the substrate
    // collector just sees a well-formed empty registry.
    let body = "# HELP escurel_up The gateway is alive.\n\
                # TYPE escurel_up gauge\n\
                escurel_up 1\n";
    (
        StatusCode::OK,
        [("content-type", "text/plain; version=0.0.4")],
        body,
    )
}
