//! axum HTTP gateway + tonic gRPC mirror. Both transports share
//! the same `AppState` (indexer + verifier + quota) so the auth
//! and quota policies enforced on `POST /mcp` are mirrored 1:1 by
//! the gRPC interceptors.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use escurel_admin::TenantStore;
use escurel_auth::OidcVerifier;
use escurel_crdt::CrdtBackend;
use escurel_index::Indexer;
use escurel_proto::v1::escurel_admin_server::EscurelAdminServer;
use escurel_proto::v1::escurel_server::EscurelServer;
use escurel_quota::QuotaManager;
use serde_json::json;
use thiserror::Error;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::grpc::{EscurelAdminGrpc, EscurelGrpc};
use crate::health::{AlwaysReady, ReadinessProbe, ReadinessReport};
use crate::mcp::mcp;
use crate::session::SessionManager;
use crate::ws::ws_upgrade;

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
    shutdown_tx: Option<oneshot::Sender<()>>,
    grpc_shutdown_tx: Option<oneshot::Sender<()>>,
    join: JoinHandle<()>,
    grpc_join: Option<JoinHandle<()>>,
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
        let _ = self.join.await;
        if let Some(j) = self.grpc_join.take() {
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
    /// Always present. Operations no-op (return a JSON-RPC error)
    /// when `crdt_backend` is `None`.
    pub(crate) sessions: Arc<SessionManager>,
}

/// Build the router(s) + bind + spawn the server tasks. Returns
/// once the HTTP (and optional gRPC) listener is bound so tests
/// can read back the local addresses. Both background tasks run
/// until [`ServerHandle::shutdown`] fires or the process exits.
pub async fn serve(config: ServerConfig) -> Result<ServerHandle, ServerError> {
    let state = AppState {
        version: config.version.clone(),
        readiness: Arc::clone(&config.readiness),
        indexer: config.indexer.clone(),
        verifier: config.verifier.clone(),
        quota: config.quota.clone(),
        tenant_store: config.tenant_store.clone(),
        crdt_backend: config.crdt_backend.clone(),
        sessions: Arc::new(SessionManager::new()),
    };

    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/version", get(version))
        .route("/metrics", get(metrics))
        .route("/mcp", post(mcp))
        .route("/ws", get(ws_upgrade))
        .with_state(state.clone());

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

    Ok(ServerHandle {
        local_addr,
        grpc_addr,
        shutdown_tx: Some(tx),
        grpc_shutdown_tx,
        join,
        grpc_join,
    })
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

async fn metrics() -> impl IntoResponse {
    let body = "# HELP escurel_up The gateway is alive.\n\
                # TYPE escurel_up gauge\n\
                escurel_up 1\n";
    (
        StatusCode::OK,
        [("content-type", "text/plain; version=0.0.4")],
        body,
    )
}
