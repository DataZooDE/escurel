//! WebSocket scaffolding on `/ws`.
//!
//! Wire shape follows `docs/spec/protocol.md §WebSocket framing`
//! verbatim. The full live-CRDT loop lands in M4; M3 ships the
//! scaffolding so clients can already establish presence-only
//! sessions and exercise the auth + quota gates the production
//! path will use.
//!
//! Auth and quota mirror the `POST /mcp` path:
//!
//! - **Auth.** When the gateway is configured with an
//!   [`OidcVerifier`], the upgrade request must carry
//!   `Authorization: Bearer <jwt>`. Missing / invalid is rejected
//!   with HTTP 401 *before* the upgrade — that way browser clients
//!   get a real status code instead of a half-open socket.
//! - **Quota.** Each accepted connection occupies one
//!   [`Dimension::Sessions`]-equivalent slot on the per-tenant
//!   [`QuotaManager`]. The slot is released on disconnect (a drop
//!   guard returned by `try_acquire_session`). At-cap upgrades
//!   are refused with HTTP 429.
//!
//! After upgrade, the client sends a `hello` frame. M3 supports
//! `{ "type": "hello", "presence_only": true }` end-to-end and
//! returns a typed error for the live-CRDT shape — the per-frame
//! handler then services `presence`, `search_subscribe`, and
//! `close` frames. `search_subscribe` ACKs synchronously with an
//! empty `search_event`; the live push of new hits as new pages
//! are indexed is a v1-deferred feature per the spec.
//!
//! Unknown frame `type`s yield an `error` frame with code
//! `unknown_frame`; the connection stays open so a malformed
//! client can recover without re-handshaking.

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use escurel_auth::{AuthContext, OidcVerifier};
use escurel_quota::SessionGuard;
use serde_json::{Value, json};

use crate::server::AppState;

/// `GET /ws` entry point. Authenticates the upgrade request,
/// acquires a per-tenant session slot from the [`QuotaManager`],
/// then upgrades and dispatches frames per the spec.
pub async fn ws_upgrade(
    State(state): State<AppState>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> axum::response::Response {
    // Auth gate — only enforced when a verifier is configured.
    // Unconfigured (dev) gateways skip auth and quota entirely;
    // production deployments always wire both.
    let auth_ctx = match state.verifier.as_ref() {
        Some(verifier) => match enforce_auth(verifier, &headers).await {
            Ok(ctx) => Some(ctx),
            Err(resp) => return resp,
        },
        None => None,
    };

    // Quota gate — debit a session slot. The guard is moved into
    // the upgraded socket task and released on drop.
    let session_guard = match (state.quota.as_ref(), auth_ctx.as_ref()) {
        (Some(q), Some(ctx)) => match q.try_acquire_session(&ctx.tenant_id) {
            Some(g) => Some(g),
            None => return session_cap_response(),
        },
        _ => None,
    };

    ws.on_upgrade(move |socket| handle_socket(socket, session_guard))
}

async fn enforce_auth(
    verifier: &OidcVerifier,
    headers: &HeaderMap,
) -> Result<AuthContext, axum::response::Response> {
    let Some(token) = bearer_token(headers) else {
        return Err(auth_failure("missing Authorization: Bearer header"));
    };
    verifier
        .verify(&token)
        .await
        .map_err(|e| auth_failure(format!("token rejected: {e}")))
}

fn bearer_token(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get("authorization")?.to_str().ok()?;
    if let Some(stripped) = raw.strip_prefix("Bearer ") {
        return Some(stripped.trim().to_owned());
    }
    if let Some(stripped) = raw.strip_prefix("bearer ") {
        return Some(stripped.trim().to_owned());
    }
    None
}

fn auth_failure(message: impl Into<String>) -> axum::response::Response {
    (
        StatusCode::UNAUTHORIZED,
        axum::Json(json!({
            "error": "unauthorized",
            "message": message.into(),
        })),
    )
        .into_response()
}

fn session_cap_response() -> axum::response::Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        axum::Json(json!({
            "error": "session_cap_reached",
            "message": "tenant concurrent-session cap exhausted; try again after another session closes",
        })),
    )
        .into_response()
}

/// Per-connection state machine. Owns the session guard for the
/// lifetime of the socket; drops it (releasing the quota slot)
/// on disconnect.
async fn handle_socket(mut socket: WebSocket, _session_guard: Option<SessionGuard>) {
    // Wait for the client's hello. Spec: the very first frame
    // after upgrade is `{ "type": "hello", ... }`. We tolerate
    // pings and arbitrary control frames before it (the underlying
    // axum WS handler answers pings transparently).
    let hello = match next_json(&mut socket).await {
        Ok(v) => v,
        Err(stop) => {
            close_with(&mut socket, stop).await;
            return;
        }
    };

    match classify_hello(&hello) {
        Hello::PresenceOnly => {}
        Hello::Session => {
            // M3 doesn't yet attach to a live CRDT session. Send
            // the typed error and close. M4 will route to a
            // LiveSessionDispatcher here.
            let _ = send_json(
                &mut socket,
                json!({
                    "type": "error",
                    "code": "live_session_unavailable",
                    "message": "live CRDT mode lands in M4",
                }),
            )
            .await;
            close(&mut socket).await;
            return;
        }
        Hello::Malformed(reason) => {
            let _ = send_json(
                &mut socket,
                json!({
                    "type": "error",
                    "code": "invalid_hello",
                    "message": reason,
                }),
            )
            .await;
            close(&mut socket).await;
            return;
        }
    }

    // presence-only main loop
    loop {
        let frame = match next_json(&mut socket).await {
            Ok(v) => v,
            Err(NextStop::ClientClosed | NextStop::StreamEnded) => break,
            Err(NextStop::ProtocolError(msg)) => {
                let _ = send_json(
                    &mut socket,
                    json!({
                        "type": "error",
                        "code": "protocol_error",
                        "message": msg,
                    }),
                )
                .await;
                break;
            }
        };

        let frame_type = frame.get("type").and_then(Value::as_str).unwrap_or("");
        match frame_type {
            "presence" => {
                // Placeholder: echo back the presence frame as
                // confirmation. M4 broadcasts to other connected
                // peers via the LiveSessionDispatcher.
                if send_json(&mut socket, frame).await.is_err() {
                    break;
                }
            }
            "search_subscribe" => {
                let sub_id = frame.get("subscription_id").cloned().unwrap_or(Value::Null);
                // M3 ACKs with an empty event. The live push of
                // new hits as new pages are indexed is a
                // v1-deferred feature per the spec.
                let event = json!({
                    "type": "search_event",
                    "subscription_id": sub_id,
                    "hits": [],
                });
                if send_json(&mut socket, event).await.is_err() {
                    break;
                }
            }
            "close" => {
                close(&mut socket).await;
                break;
            }
            other => {
                let _ = send_json(
                    &mut socket,
                    json!({
                        "type": "error",
                        "code": "unknown_frame",
                        "message": format!("unsupported frame `{other}`"),
                    }),
                )
                .await;
                // Keep the connection open — a malformed client
                // can recover without re-handshaking.
            }
        }
    }
}

#[derive(Debug)]
enum NextStop {
    ClientClosed,
    StreamEnded,
    ProtocolError(String),
}

async fn next_json(socket: &mut WebSocket) -> Result<Value, NextStop> {
    loop {
        let msg = match socket.recv().await {
            Some(Ok(m)) => m,
            Some(Err(e)) => return Err(NextStop::ProtocolError(format!("ws read failed: {e}"))),
            None => return Err(NextStop::StreamEnded),
        };
        match msg {
            Message::Text(t) => {
                return serde_json::from_str(&t)
                    .map_err(|e| NextStop::ProtocolError(format!("frame is not valid JSON: {e}")));
            }
            Message::Binary(b) => {
                return serde_json::from_slice(&b).map_err(|e| {
                    NextStop::ProtocolError(format!("binary frame is not valid UTF-8 JSON: {e}"))
                });
            }
            Message::Close(_) => return Err(NextStop::ClientClosed),
            // Ping/Pong are handled transparently by the axum WS
            // extractor at the transport layer, but the message
            // is still surfaced here. Ignore and keep reading.
            Message::Ping(_) | Message::Pong(_) => continue,
        }
    }
}

async fn send_json(socket: &mut WebSocket, value: Value) -> Result<(), axum::Error> {
    socket.send(Message::Text(value.to_string().into())).await
}

async fn close(socket: &mut WebSocket) {
    let _ = socket.send(Message::Close(None)).await;
}

async fn close_with(socket: &mut WebSocket, _stop: NextStop) {
    close(socket).await;
}

#[derive(Debug)]
enum Hello {
    PresenceOnly,
    /// `{ type: "hello", session: <id> }`. M3 only routes the
    /// shape; the session id is unused until M4 wires the live
    /// CRDT dispatcher.
    Session,
    Malformed(String),
}

fn classify_hello(v: &Value) -> Hello {
    if v.get("type").and_then(Value::as_str) != Some("hello") {
        return Hello::Malformed("first frame must be `{type: \"hello\", …}`".to_owned());
    }
    if let Some(true) = v.get("presence_only").and_then(Value::as_bool) {
        return Hello::PresenceOnly;
    }
    if v.get("session").and_then(Value::as_str).is_some() {
        return Hello::Session;
    }
    Hello::Malformed("hello must set either `presence_only: true` or `session: <id>`".to_owned())
}
