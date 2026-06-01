//! `echo-app` — minimal demonstration backend.
//!
//! This crate exists to prove the contract from
//! [`docs/spec/dx.md`](../../../docs/spec/dx.md) §"Chaining recipe"
//! holds end-to-end. It is *not* a reusable framework: it is the
//! smallest realistic shape of an application backend that uses
//! [`escurel_client::Client`] to fetch a markdown page by
//! `[[wikilink]]` and serves the rendered body over HTTP.
//!
//! # Surface
//!
//! ```text
//! GET /pages/{slug}        →  text/markdown body of customer::{slug}
//! GET /healthz             →  200 "OK"
//! ```
//!
//! The router only knows about one skill (`customer`) on purpose.
//! Real applications would template the skill name from request
//! shape; this one hard-codes it so the demo stays compact.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use escurel_client::{Client, ExpandRequest, ResolveRequest, SecretString};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

/// Construction options for [`spawn`].
///
/// `escurel_endpoint` is the HTTP MCP URL of the escurel gateway
/// (e.g. `http://127.0.0.1:8080`); `escurel_token` is the bearer
/// the backend uses on every upstream call. In production both
/// come from env vars (see [`crate::env_opts`]); in tests they
/// come from the `escurel-test-support` façade.
#[derive(Debug, Clone)]
pub struct Opts {
    pub escurel_endpoint: String,
    pub escurel_token: String,
}

/// Handle to a running echo-app backend.
///
/// Holds the bound `base_url` and the join handle for the axum
/// task; [`Self::shutdown`] signals graceful shutdown and awaits
/// the task. Dropping the handle without calling `shutdown`
/// signals the same oneshot but does not block on the join — the
/// runtime carries the task to completion on its own schedule.
pub struct Backend {
    base_url: String,
    shutdown_tx: Option<oneshot::Sender<()>>,
    join: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Backend")
            .field("base_url", &self.base_url)
            .finish_non_exhaustive()
    }
}

impl Backend {
    /// `http://127.0.0.1:<port>` — the address an HTTP client uses
    /// to drive the backend.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Signal shutdown and await the axum task. Idempotent.
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(join) = self.join.take() {
            let _ = join.await;
        }
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        // Best-effort: signal the shutdown channel if the caller
        // didn't `shutdown().await` explicitly. We don't block on
        // the join — Drop running inside an async context would
        // risk a runtime-within-runtime panic.
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

#[derive(Clone)]
struct AppState {
    escurel: Arc<Client>,
}

/// Bind the backend on `127.0.0.1:0`, connect to escurel, and
/// spawn the axum server task. Returns once the listener is bound
/// so callers can read `base_url()` and dial back without a race.
///
/// Errors:
/// - failed bind (port exhaustion, permissions).
/// - failed escurel client connect (HTTP handshake, invalid
///   endpoint, invalid token).
pub async fn spawn(opts: Opts) -> anyhow::Result<Backend> {
    let client = Client::connect(
        &opts.escurel_endpoint,
        SecretString::from(opts.escurel_token),
    )
    .await
    .map_err(|e| anyhow::anyhow!("connect to escurel at {}: {e:?}", opts.escurel_endpoint))?;

    let state = AppState {
        escurel: Arc::new(client),
    };
    let app = router(state);

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let local: SocketAddr = listener.local_addr()?;
    let base_url = format!("http://{local}");

    let (tx, rx) = oneshot::channel::<()>();
    let join = tokio::spawn(async move {
        let serve = axum::serve(listener, app).with_graceful_shutdown(async move {
            let _ = rx.await;
        });
        let _ = serve.await;
    });

    Ok(Backend {
        base_url,
        shutdown_tx: Some(tx),
        join: Some(join),
    })
}

fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/pages/{slug}", get(get_page))
        .with_state(state)
}

async fn healthz() -> impl IntoResponse {
    (StatusCode::OK, "OK")
}

/// `GET /pages/{slug}` — the chaining recipe in one handler.
///
/// 1. Build a `[[customer::{slug}]]` wikilink.
/// 2. `Client::resolve` to obtain the page id.
/// 3. `Client::expand` to fetch the markdown body.
/// 4. Return the body as `text/markdown; charset=utf-8`.
async fn get_page(State(state): State<AppState>, Path(slug): Path<String>) -> Response {
    let wikilink = format!("[[customer::{slug}]]");

    let resolved = match state
        .escurel
        .resolve(ResolveRequest {
            wikilink,
            ..Default::default()
        })
        .await
    {
        Ok(r) => r,
        Err(e) => return upstream_error("resolve", &e.to_string()),
    };
    if !resolved.exists {
        return (StatusCode::NOT_FOUND, format!("customer::{slug} not found")).into_response();
    }
    let Some(page) = resolved.page else {
        // resolve says exists=true but no PageRef — treat as
        // upstream contract violation rather than a silent 500.
        return upstream_error("resolve", "exists=true but no PageRef returned");
    };

    let expanded = match state
        .escurel
        .expand(ExpandRequest {
            page_id: page.page_id.clone(),
            anchor: String::new(),
            version: String::new(),
            ..Default::default()
        })
        .await
    {
        Ok(e) => e,
        Err(e) => return upstream_error("expand", &e.to_string()),
    };

    let mut response = expanded.body.into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/markdown; charset=utf-8"),
    );
    response
}

fn upstream_error(op: &str, detail: &str) -> Response {
    (
        StatusCode::BAD_GATEWAY,
        format!("escurel {op} failed: {detail}"),
    )
        .into_response()
}

/// Read [`Opts`] from the `ESCUREL_ENDPOINT` / `ESCUREL_TOKEN`
/// environment variables. The main binary uses this; tests build
/// [`Opts`] directly so they don't poison the process env.
pub fn env_opts() -> anyhow::Result<Opts> {
    let escurel_endpoint = std::env::var("ESCUREL_ENDPOINT")
        .map_err(|_| anyhow::anyhow!("ESCUREL_ENDPOINT not set"))?;
    let escurel_token =
        std::env::var("ESCUREL_TOKEN").map_err(|_| anyhow::anyhow!("ESCUREL_TOKEN not set"))?;
    Ok(Opts {
        escurel_endpoint,
        escurel_token,
    })
}
