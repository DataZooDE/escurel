//! The bridge's HTTP surface: the platform's messaging endpoint plus the
//! substrate probes.

use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};

use crate::bridge::Bridge;
use crate::channel::ChatChannel;

struct AppState<C: ChatChannel> {
    bridge: Arc<Bridge<C>>,
    version: String,
}

/// `POST /api/messages`, `GET /healthz` (dependency-free), `GET /version`.
pub fn router<C: ChatChannel>(bridge: Arc<Bridge<C>>, version: String) -> Router {
    Router::new()
        .route("/api/messages", post(messages::<C>))
        .route("/healthz", get(|| async { "ok" }))
        .route("/version", get(version_of::<C>))
        .with_state(Arc::new(AppState { bridge, version }))
}

async fn version_of<C: ChatChannel>(State(s): State<Arc<AppState<C>>>) -> String {
    s.version.clone()
}

/// Verify, admit, answer at once; the turn is worked in the background. A
/// store failure is a `500` so the platform redelivers — safe, because the
/// message id is the dedup key.
async fn messages<C: ChatChannel>(
    State(s): State<Arc<AppState<C>>>,
    headers: HeaderMap,
    body: Bytes,
) -> StatusCode {
    let payload = match s.bridge.channel().verify_inbound(&headers, &body).await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "chat bridge: inbound request refused");
            return StatusCode::UNAUTHORIZED;
        }
    };
    match s.bridge.accept(&payload) {
        Ok(()) => StatusCode::OK,
        Err(e) => {
            tracing::error!(error = %e, "chat bridge: could not admit the message");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}
