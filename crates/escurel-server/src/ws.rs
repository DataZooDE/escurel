//! WebSocket scaffolding on `/ws`.
//!
//! Wire shape follows `docs/spec/protocol.md §WebSocket framing`
//! verbatim. M3.6 shipped the presence-only path + the auth +
//! quota gates; M4.4 wires the live-CRDT `session` shape — a
//! `hello` frame with a `session` field attaches the socket to
//! an already-open [`SessionManager`] entry and the per-frame
//! handler dispatches `op` / `presence` / `close`.
//!
//! Auth and quota mirror the `POST /mcp` path:
//!
//! - **Auth.** When the gateway is configured with an
//!   [`OidcVerifier`], the upgrade request must carry
//!   `Authorization: Bearer <jwt>`. Missing / invalid is rejected
//!   with HTTP 401 *before* the upgrade — that way browser clients
//!   get a real status code instead of a half-open socket.
//! - **Quota.** Each accepted connection occupies one session-cap
//!   slot on the per-tenant [`QuotaManager`]. The slot is released
//!   on disconnect (a drop guard returned by `try_acquire_session`).
//!   At-cap upgrades are refused with HTTP 429. The attach path
//!   piggybacks on this slot — no extra acquire. Each `op` frame
//!   additionally debits the [`Dimension::Writes`] bucket, mirroring
//!   the HTTP `apply_op` policy in `mcp.rs`.
//!
//! After upgrade, the client sends a `hello` frame. Two shapes:
//!
//! ```jsonc
//! { "type": "hello", "presence_only": true }    // presence + search subs only
//! { "type": "hello", "session": "sess_..." }    // attach to an open CRDT session
//! ```
//!
//! In session mode, an `op` frame is base64-decoded and forwarded
//! to [`SessionManager::apply`]; the reply is an `op_ack` carrying
//! `merged_version` + the post-merge text content. A `close` frame
//! invokes [`SessionManager::close`] and replies with `closed`
//! before tearing down the socket. A WS disconnect *without* an
//! explicit `close` frame leaves the session open — the client
//! can reconnect and re-attach by id (transport disconnect ≠
//! session close).
//!
//! Unknown frame `type`s yield an `error` frame with code
//! `unknown_frame`; the connection stays open so a malformed
//! client can recover without re-handshaking.

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use escurel_auth::{AuthContext, OidcVerifier};
use escurel_crdt::Op;
use escurel_quota::{Dimension, QuotaError, SessionGuard};
use serde_json::{Value, json};

use crate::server::AppState;
use crate::session::SessionError;

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

    // Tenant id is needed for the per-op `Writes` quota debit in
    // session mode. Falls back to the same `"default"` sentinel
    // the HTTP `mcp.rs` path uses when no verifier is wired.
    let tenant_id = auth_ctx
        .as_ref()
        .map(|c| c.tenant_id.clone())
        .unwrap_or_else(|| "default".to_owned());

    ws.on_upgrade(move |socket| handle_socket(socket, state, session_guard, tenant_id))
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
/// on disconnect. The session guard is the *connection's* slot,
/// not the CRDT session's slot — the live `SessionManager` holds
/// its own guard for the lifetime of the live edit (acquired by
/// the HTTP `open_session` tool), so the attach path here debits
/// nothing extra on top of the upgrade.
async fn handle_socket(
    mut socket: WebSocket,
    state: AppState,
    _session_guard: Option<SessionGuard>,
    tenant_id: String,
) {
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
        Hello::Session(session_id) => {
            session_loop(&mut socket, &state, &tenant_id, session_id).await;
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

/// Session-mode per-frame loop. Entered after a `hello` with a
/// `session` field that resolves to an open entry in the
/// [`SessionManager`]. Dispatches `op` / `presence` / `close`
/// and replies per the spec; falls through to `unknown_frame`
/// for anything else (the connection stays open).
///
/// Returns when the client sends a `close` frame, when the
/// stream ends (transport disconnect), or when the registry no
/// longer knows the session id (e.g. another transport closed
/// it concurrently).
async fn session_loop(
    socket: &mut WebSocket,
    state: &AppState,
    tenant_id: &str,
    session_id: String,
) {
    // Reject the attach if the session id is unknown. The
    // registry's `page_id_of` is the cheapest membership probe;
    // any subsequent `apply` / `close` re-checks the same map,
    // so the rare race where the session is closed mid-attach
    // surfaces as an `unknown_session` from the apply path.
    if state.sessions.page_id_of(&session_id).is_none() {
        let _ = send_json(
            socket,
            json!({
                "type": "error",
                "code": "unknown_session",
                "message": format!("session `{session_id}` is not open on this gateway"),
            }),
        )
        .await;
        close(socket).await;
        return;
    }

    loop {
        let frame = match next_json(socket).await {
            Ok(v) => v,
            // Transport disconnect (with or without a close
            // frame from the client) leaves the session open —
            // the spec's session lifetime is decoupled from the
            // WS transport. Only an explicit `close` frame or
            // an HTTP `close_session` tool call closes the
            // session.
            Err(NextStop::ClientClosed | NextStop::StreamEnded) => return,
            Err(NextStop::ProtocolError(msg)) => {
                let _ = send_json(
                    socket,
                    json!({
                        "type": "error",
                        "code": "protocol_error",
                        "message": msg,
                    }),
                )
                .await;
                return;
            }
        };

        let frame_type = frame.get("type").and_then(Value::as_str).unwrap_or("");
        match frame_type {
            "op" => {
                if handle_op(socket, state, tenant_id, &session_id, &frame)
                    .await
                    .is_err()
                {
                    return;
                }
            }
            "presence" => {
                // Single-peer echo for now. Multi-peer broadcast
                // is M4-stretch / v1-deferred ("Live cursors" in
                // the spec); echoing back lets the client
                // confirm round-trip and exercise the loop end
                // to end.
                if send_json(socket, frame).await.is_err() {
                    return;
                }
            }
            "close" => {
                let commit = frame.get("commit").and_then(Value::as_bool).unwrap_or(true);
                match state.sessions.close(&session_id, commit).await {
                    Ok(v) => {
                        let _ = send_json(
                            socket,
                            json!({
                                "type": "closed",
                                "session": session_id,
                                "final_version": v.as_str(),
                            }),
                        )
                        .await;
                    }
                    Err(e) => {
                        let _ = send_json(socket, session_error_frame(&session_id, &e)).await;
                    }
                }
                close(socket).await;
                return;
            }
            other => {
                let _ = send_json(
                    socket,
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

/// Handle one `op` frame in session mode. Debits the per-tenant
/// `Writes` budget, base64-decodes the payload, forwards to
/// [`SessionManager::apply`], and replies with an `op_ack`. On
/// failure (quota, decode, apply) sends a typed `error` frame
/// and returns `Ok(())` so the connection stays open — the
/// client can retry. `Err` signals a transport write failure
/// (the socket is dead).
async fn handle_op(
    socket: &mut WebSocket,
    state: &AppState,
    tenant_id: &str,
    session_id: &str,
    frame: &Value,
) -> Result<(), axum::Error> {
    // Quota first — mirrors the HTTP `apply_op` ordering in
    // `mcp.rs`: refuse before doing any work.
    if let Some(q) = state.quota.as_ref() {
        if let Err(err) = q.try_consume(tenant_id, Dimension::Writes) {
            return send_json(socket, quota_error_frame(session_id, &err)).await;
        }
    }

    let op_b64 = match frame.get("op").and_then(Value::as_str) {
        Some(s) => s,
        None => {
            return send_json(
                socket,
                json!({
                    "type": "error",
                    "code": "invalid_op",
                    "message": "`op` frame missing string `op` field",
                    "session": session_id,
                }),
            )
            .await;
        }
    };
    let op_bytes = match B64.decode(op_b64.as_bytes()) {
        Ok(b) => b,
        Err(e) => {
            return send_json(
                socket,
                json!({
                    "type": "error",
                    "code": "invalid_op",
                    "message": format!("`op` field is not valid base64: {e}"),
                    "session": session_id,
                }),
            )
            .await;
        }
    };

    let merged = match state.sessions.apply(session_id, Op::new(op_bytes)).await {
        Ok(v) => v,
        Err(e) => {
            return send_json(socket, session_error_frame(session_id, &e)).await;
        }
    };

    // Read current content so the client can render without
    // round-tripping. `current_content` returns `None` only when
    // the session id is gone — which would have made the
    // `apply` above fail too — so an empty string is the safe
    // fallback for the edge case where another transport closed
    // the session between the apply and the read.
    let content = state
        .sessions
        .current_content(session_id)
        .await
        .unwrap_or_default();

    send_json(
        socket,
        json!({
            "type": "op_ack",
            "session": session_id,
            "merged_version": merged.as_str(),
            "content": content,
            "conflicts": [],
            "issues": [],
        }),
    )
    .await
}

fn quota_error_frame(session_id: &str, err: &QuotaError) -> Value {
    let QuotaError::Exhausted {
        dimension,
        retry_after_ms,
    } = err;
    let dim = format!("{dimension:?}").to_lowercase();
    json!({
        "type": "error",
        "code": "quota_exhausted",
        "message": format!("quota exhausted on {dim}; retry after {retry_after_ms} ms"),
        "session": session_id,
        "dimension": dim,
        "retry_after_ms": retry_after_ms,
    })
}

fn session_error_frame(session_id: &str, err: &SessionError) -> Value {
    let code = match err {
        SessionError::UnknownSession(_) => "unknown_session",
        SessionError::AlreadyOpen(_) => "session_already_open",
        SessionError::StillReferenced => "session_still_referenced",
        SessionError::LiveDoc(_) => "livedoc_error",
    };
    json!({
        "type": "error",
        "code": code,
        "message": err.to_string(),
        "session": session_id,
    })
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
    /// `{ type: "hello", session: <id> }`. Carries the session id
    /// the M4.4 attach path will look up in the registry.
    Session(String),
    Malformed(String),
}

fn classify_hello(v: &Value) -> Hello {
    if v.get("type").and_then(Value::as_str) != Some("hello") {
        return Hello::Malformed("first frame must be `{type: \"hello\", …}`".to_owned());
    }
    if let Some(true) = v.get("presence_only").and_then(Value::as_bool) {
        return Hello::PresenceOnly;
    }
    if let Some(s) = v.get("session").and_then(Value::as_str) {
        return Hello::Session(s.to_owned());
    }
    Hello::Malformed("hello must set either `presence_only: true` or `session: <id>`".to_owned())
}
