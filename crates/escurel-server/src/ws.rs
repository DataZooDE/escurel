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
use escurel_auth::Role;
use escurel_crdt::Op;
use escurel_index::{AclCaller, IndexerHandle};
use escurel_md::PageType;
use escurel_quota::{Dimension, QuotaError, SessionGuard};
use serde_json::{Value, json};

use crate::live_dispatch::PeerRecv;
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
    let auth_ctx = match crate::auth_gate::authenticate(&state, &headers).await {
        Ok(ctx) => ctx,
        Err(resp) => return resp,
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

    // The verified principal, carried into the socket task so the session
    // attach can be ACL-gated (#352). Previously only `tenant_id` survived
    // the upgrade, which is why `ws.rs` had no way to make an ACL decision
    // even in principle.
    let caller = WsCaller {
        subject: auth_ctx
            .as_ref()
            .map(|c| c.subject.clone())
            .unwrap_or_default(),
        groups: crate::auth_gate::rbac_groups(&state, auth_ctx.as_ref()),
        // No verifier (dev / on-host mode) → admin bypass, matching the
        // HTTP and ingest gates exactly.
        is_admin: auth_ctx
            .as_ref()
            .is_none_or(|c| matches!(c.role, Role::Admin)),
        tenant_id,
    };

    ws.on_upgrade(move |socket| handle_socket(socket, state, session_guard, caller))
}

/// The verified principal behind a WebSocket connection.
pub(crate) struct WsCaller {
    pub tenant_id: String,
    subject: String,
    groups: Vec<String>,
    is_admin: bool,
}

impl WsCaller {
    fn acl(&self) -> AclCaller<'_> {
        AclCaller {
            subject: &self.subject,
            is_admin: self.is_admin,
            token_groups: &self.groups,
        }
    }
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
    caller: WsCaller,
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
            session_loop(socket, &state, &caller, session_id).await;
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

/// Whether `caller` may attach to a session on `page_id`.
///
/// Mirrors the HTTP read path: only `type: instance` pages carry an instance
/// ACL, and `may_read_instance` is the same predicate `expand` applies to the
/// same bytes.
///
/// Fails OPEN in exactly two cases, both of which mean "there is nothing to
/// enforce against", not "enforcement is optional":
///
/// * **No indexer.** A session-only gateway (`indexer = None`) has no page
///   corpus, so no ACL exists. `tool_open_session`'s layer guard reasons the
///   same way about the same deployment.
/// * **The page is not in the corpus**, or is not an instance. There is no
///   ACL attached to it, and a session on an unknown page holds no stored
///   content to disclose.
///
/// Any error reading the page fails CLOSED — an ACL decision that cannot be
/// made is not a decision to allow.
async fn may_attach(state: &AppState, caller: &WsCaller, page_id: &str) -> bool {
    let Some(indexer) = state.indexer.as_ref().map(IndexerHandle::current) else {
        return true;
    };
    match indexer.expand(page_id, None, None).await {
        Ok(Some(e)) if e.page.page_type == PageType::Instance => indexer
            .may_read_instance(&caller.acl(), &e.page.skill, &e.frontmatter)
            .await
            .unwrap_or(false),
        Ok(_) => true,
        Err(_) => false,
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
    mut socket: WebSocket,
    state: &AppState,
    caller: &WsCaller,
    session_id: String,
) {
    let socket = &mut socket;
    let tenant_id = caller.tenant_id.as_str();
    // Reject the attach if the session id is unknown. The
    // registry's `page_id_of` is the cheapest membership probe;
    // any subsequent `apply` / `close` re-checks the same map,
    // so the rare race where the session is closed mid-attach
    // surfaces as an `unknown_session` from the apply path.
    let Some(page_id) = state.sessions.page_id_of(&session_id) else {
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
    };

    // ACL gate (#352). Attaching is a READ: `op_ack` carries the session's
    // current content, so a principal who may not read the page must not
    // join a session on it. Without this the session was a side channel
    // around the instance ACL — verified by
    // `tests/ws_attach_acl.rs`, where a non-owner received the owner's live
    // draft text verbatim from a page HTTP `expand` denies them.
    //
    // Enforced at attach rather than per frame. Per-frame evaluation would
    // put a DuckDB round trip in front of every keystroke; the cost of
    // attach-time enforcement is that an ACL revoked mid-session takes
    // effect when the peer next attaches, which is documented in
    // `docs/spec/protocol.md`.
    if !may_attach(state, caller, &page_id).await {
        let _ = send_json(
            socket,
            json!({
                "type": "error",
                "code": "forbidden",
                "message": format!(
                    "not permitted to attach to a session on page `{page_id}`"
                ),
                "session": session_id,
            }),
        )
        .await;
        close(socket).await;
        return;
    }

    // Split so incoming frames and peer broadcasts can be awaited together;
    // a single `&mut WebSocket` cannot be borrowed by both arms of a
    // `select!`. The sink half is what every reply and every delivered peer
    // frame is written to.
    let (mut sink, mut stream) = {
        use futures_util::StreamExt as _;
        socket.split()
    };
    let me = state.dispatcher.next_peer_id();
    let mut peers = state.dispatcher.subscribe(&session_id);
    let socket = &mut sink;

    loop {
        let frame = tokio::select! {
            incoming = next_json(&mut stream) => incoming,
            peer = peers.recv(me) => {
                match peer {
                    PeerRecv::Frame(f) => {
                        if send_json(socket, (*f).clone()).await.is_err() {
                            break;
                        }
                        continue;
                    }
                    // This peer fell behind the channel. Tell it rather than
                    // silently dropping frames: a client showing stale
                    // content confidently is worse than one told to re-read.
                    PeerRecv::Lagged { skipped } => {
                        let notice = json!({
                            "type": "resync_required",
                            "session": session_id,
                            "skipped": skipped,
                            "message": "fell behind the session broadcast; \
                                        re-read the page and re-attach",
                        });
                        if send_json(socket, notice).await.is_err() {
                            break;
                        }
                        continue;
                    }
                    // No senders remain; nothing further can arrive on this
                    // channel, but the client's own frames still work.
                    PeerRecv::Closed => {
                        std::future::pending::<()>().await;
                        continue;
                    }
                }
            }
        };
        let frame = match frame {
            Ok(v) => v,
            // Transport disconnect (with or without a close
            // frame from the client) leaves the session open —
            // the spec's session lifetime is decoupled from the
            // WS transport. Only an explicit `close` frame or
            // an HTTP `close_session` tool call closes the
            // session.
            Err(NextStop::ClientClosed | NextStop::StreamEnded) => break,
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
                break;
            }
        };

        let frame_type = frame.get("type").and_then(Value::as_str).unwrap_or("");
        match frame_type {
            "op" => {
                if handle_op(socket, state, tenant_id, &session_id, &frame, me)
                    .await
                    .is_err()
                {
                    break;
                }
            }
            "presence" => {
                // Broadcast to the other peers — this is what makes live
                // cursors possible (#352) — and still echo to the sender,
                // which clients rely on to confirm round-trip.
                state.dispatcher.publish(&session_id, me, frame.clone());
                if send_json(socket, frame).await.is_err() {
                    break;
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
                break;
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

    // Release the broadcast channel once this was the last peer watching.
    drop(peers);
    state.dispatcher.release(&session_id);
}

/// Handle one `op` frame in session mode. Debits the per-tenant
/// `Writes` budget, base64-decodes the payload, forwards to
/// [`SessionManager::apply`], and replies with an `op_ack`. On
/// failure (quota, decode, apply) sends a typed `error` frame
/// and returns `Ok(())` so the connection stays open — the
/// client can retry. `Err` signals a transport write failure
/// (the socket is dead).
async fn handle_op<S>(
    socket: &mut S,
    state: &AppState,
    tenant_id: &str,
    session_id: &str,
    frame: &Value,
    me: crate::live_dispatch::PeerId,
) -> Result<(), axum::Error>
where
    S: futures_util::Sink<Message, Error = axum::Error> + Unpin,
{
    // Quota first — mirrors the HTTP `apply_op` ordering in
    // `mcp.rs`: refuse before doing any work.
    if let Some(q) = state.quota.as_ref()
        && let Err(err) = q.try_consume(tenant_id, Dimension::Writes)
    {
        return send_json(socket, quota_error_frame(session_id, &err)).await;
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

    // Fan out to the other peers on this session (#352). They receive
    // `peer_op` rather than `op_ack`: an ack answers "your write landed",
    // which is only true for the originator. The merged content rides along
    // so a peer can render without a round trip — the same courtesy the
    // sender's `op_ack` already extends.
    //
    // Published before the sender's ack so a peer is never behind the
    // originator's own view of the document.
    state.dispatcher.publish(
        session_id,
        me,
        json!({
            "type": "peer_op",
            "session": session_id,
            "merged_version": merged.as_str(),
            "content": content,
            "op": op_b64,
        }),
    );

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

/// Read the next JSON frame.
///
/// Generic over the stream so the presence-only loop (which owns the whole
/// `WebSocket`) and the session loop (which owns a `SplitStream` half, so it
/// can await broadcasts concurrently) share one implementation instead of
/// two that must agree about framing.
async fn next_json<S>(socket: &mut S) -> Result<Value, NextStop>
where
    S: futures_util::Stream<Item = Result<Message, axum::Error>> + Unpin,
{
    use futures_util::StreamExt as _;
    loop {
        let msg = match socket.next().await {
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

async fn send_json<S>(socket: &mut S, value: Value) -> Result<(), axum::Error>
where
    S: futures_util::Sink<Message, Error = axum::Error> + Unpin,
{
    use futures_util::SinkExt as _;
    socket.send(Message::Text(value.to_string().into())).await
}

async fn close<S>(socket: &mut S)
where
    S: futures_util::Sink<Message, Error = axum::Error> + Unpin,
{
    use futures_util::SinkExt as _;
    let _ = socket.send(Message::Close(None)).await;
}

async fn close_with<S>(socket: &mut S, _stop: NextStop)
where
    S: futures_util::Sink<Message, Error = axum::Error> + Unpin,
{
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
