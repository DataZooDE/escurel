//! End-to-end tests for the M4.4 WebSocket attach-to-session path.
//!
//! Real axum gateway, real `SessionManager` + `LiveDoc` actor over
//! a real `DuckdbCrdtBackend`, real `OidcVerifier` against the
//! in-process JWKS the support crate stands up, real
//! `tokio-tungstenite` client. Loro ops are produced by a
//! persistent `Client` peer per
//! `docs/notes/discovered/2026-05-25-loro-incremental-updates-need-persistent-client.md`.
//!
//! Each test opens a session over HTTP `POST /mcp` (the M4.2 path)
//! and then attaches via `GET /ws` with `{type: "hello",
//! session: "..."}` — the new code path being tested.
//!
//! The auth + quota gate on the WS upgrade is already enforced by
//! the M3.6 layer; the session-attach loop relies on that and
//! debits only the `Writes` dimension per `op` frame (mirrors the
//! HTTP `apply_op` policy in `mcp.rs`).

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use duckdb::Connection;
use escurel_crdt::{CrdtBackend, DuckdbCrdtBackend};
use escurel_index::Migrator;
use escurel_quota::{QuotaConfig, QuotaManager};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, Opts, Role};
use futures::{SinkExt, StreamExt};
use loro::{ExportMode, LoroDoc};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::client::Request as WsRequest;
use tokio_tungstenite::tungstenite::protocol::Message;

const TENANT: &str = "acme";

// --- Loro client peer (mirrors mcp_session_tools.rs) -----------

struct Client {
    doc: LoroDoc,
    vv: loro::VersionVector,
}

impl Client {
    fn new() -> Self {
        let doc = LoroDoc::new();
        let vv = doc.oplog_vv();
        Self { doc, vv }
    }

    fn insert(&mut self, pos: usize, text: &str) -> Vec<u8> {
        self.doc.get_text("body").insert(pos, text).unwrap();
        self.doc.commit();
        let update = self.doc.export(ExportMode::updates(&self.vv)).unwrap();
        self.vv = self.doc.oplog_vv();
        update
    }

    fn body_len(&self) -> usize {
        self.doc.get_text("body").len_unicode()
    }
}

// --- harness ---------------------------------------------------

struct Harness {
    process: EscurelProcess,
    http: reqwest::Client,
    _db_dir: TempDir,
}

async fn start_authed(quota: Option<Arc<QuotaManager>>) -> Harness {
    let db_dir = TempDir::new().unwrap();
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let shared = Arc::new(Mutex::new(conn));
    let crdt_backend: Arc<dyn CrdtBackend> = Arc::new(DuckdbCrdtBackend::new(Arc::clone(&shared)));

    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: None,
        config_overrides: ConfigOverrides {
            quota,
            crdt_backend: Some(crdt_backend),
            disable_indexer: true,
            ..Default::default()
        },
    })
    .await;
    Harness {
        process,
        http: reqwest::Client::new(),
        _db_dir: db_dir,
    }
}

fn ws_request(url: &str, bearer: &str) -> WsRequest {
    let mut req = url.into_client_request().unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {bearer}").parse().unwrap());
    req
}

async fn recv_json(
    sock: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> Value {
    let msg = tokio::time::timeout(Duration::from_secs(3), sock.next())
        .await
        .expect("recv timed out")
        .expect("stream ended")
        .expect("ws error");
    let txt = match msg {
        Message::Text(t) => t,
        Message::Binary(b) => String::from_utf8(b).unwrap(),
        other => panic!("expected text frame, got {other:?}"),
    };
    serde_json::from_str(&txt).unwrap()
}

async fn send_json(
    sock: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    v: Value,
) {
    sock.send(Message::Text(v.to_string())).await.unwrap();
}

async fn call_ok(h: &Harness, bearer: &str, id: u64, name: &str, args: Value) -> Value {
    let resp = h
        .http
        .post(h.process.mcp_url())
        .bearer_auth(bearer)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": name, "arguments": args },
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert!(body.get("error").is_none(), "{name} error: {body}");
    // tools/call results are MCP-shaped; the payload is under
    // `structuredContent`.
    body["result"]["structuredContent"].clone()
}

async fn call_raw(h: &Harness, bearer: &str, id: u64, name: &str, args: Value) -> Value {
    let resp = h
        .http
        .post(h.process.mcp_url())
        .bearer_auth(bearer)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": name, "arguments": args },
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    resp.json().await.unwrap()
}

async fn open_session(h: &Harness, bearer: &str, page_id: &str) -> String {
    let r = call_ok(h, bearer, 1, "open_session", json!({ "page_id": page_id })).await;
    r["session"].as_str().unwrap().to_owned()
}

// --- tests -----------------------------------------------------

#[tokio::test]
async fn attach_to_session_via_hello_keeps_connection_open() {
    let h = start_authed(None).await;
    let t = h.process.mint_token(TENANT, Role::Agent);
    let session = open_session(&h, &t, "page-attach").await;

    let (mut sock, _) = tokio_tungstenite::connect_async(ws_request(&h.process.ws_url(), &t))
        .await
        .expect("ws connect");
    send_json(&mut sock, json!({ "type": "hello", "session": session })).await;

    // Probe the connection by sending a presence frame; the loop
    // must still be alive (echo back to confirm).
    send_json(
        &mut sock,
        json!({
            "type": "presence",
            "session": session,
            "user": "user-1",
            "anchor": "#intro",
        }),
    )
    .await;
    let echo = recv_json(&mut sock).await;
    assert_eq!(echo["type"], "presence");
    assert_eq!(echo["session"], session);

    sock.close(None).await.ok();
    h.process.shutdown().await;
}

#[tokio::test]
async fn op_via_ws_updates_doc_and_replies_with_op_ack() {
    let h = start_authed(None).await;
    let t = h.process.mint_token(TENANT, Role::Agent);
    let session = open_session(&h, &t, "page-op-ws").await;

    let (mut sock, _) = tokio_tungstenite::connect_async(ws_request(&h.process.ws_url(), &t))
        .await
        .unwrap();
    send_json(&mut sock, json!({ "type": "hello", "session": session })).await;

    let mut client = Client::new();
    let op = client.insert(0, "hello");
    send_json(
        &mut sock,
        json!({
            "type": "op",
            "session": session,
            "op": B64.encode(op),
        }),
    )
    .await;

    let ack = recv_json(&mut sock).await;
    assert_eq!(ack["type"], "op_ack");
    assert_eq!(ack["session"], session);
    assert_eq!(ack["merged_version"], "v1");
    assert_eq!(ack["content"], "hello");
    assert!(ack["conflicts"].as_array().is_some_and(Vec::is_empty));
    assert!(ack["issues"].as_array().is_some_and(Vec::is_empty));

    // A second op extends the doc and bumps the version.
    let op2 = client.insert(client.body_len(), " world");
    send_json(
        &mut sock,
        json!({
            "type": "op",
            "session": session,
            "op": B64.encode(op2),
        }),
    )
    .await;
    let ack2 = recv_json(&mut sock).await;
    assert_eq!(ack2["merged_version"], "v2");
    assert_eq!(ack2["content"], "hello world");

    sock.close(None).await.ok();
    h.process.shutdown().await;
}

#[tokio::test]
async fn unknown_session_id_returns_error_and_closes() {
    let h = start_authed(None).await;
    let t = h.process.mint_token(TENANT, Role::Agent);
    let (mut sock, _) = tokio_tungstenite::connect_async(ws_request(&h.process.ws_url(), &t))
        .await
        .unwrap();

    send_json(
        &mut sock,
        json!({ "type": "hello", "session": "sess_does-not-exist" }),
    )
    .await;

    let err = recv_json(&mut sock).await;
    assert_eq!(err["type"], "error");
    assert_eq!(err["code"], "unknown_session");

    // Server must close after the error.
    let next = tokio::time::timeout(Duration::from_secs(3), sock.next()).await;
    match next {
        Ok(Some(Ok(Message::Close(_)))) | Ok(None) => {}
        Ok(Some(Ok(other))) => panic!("expected close, got {other:?}"),
        Ok(Some(Err(_))) => {}
        Err(_) => panic!("server did not close after unknown_session error"),
    }
    h.process.shutdown().await;
}

#[tokio::test]
async fn close_frame_persists_and_closes_session() {
    let h = start_authed(None).await;
    let t = h.process.mint_token(TENANT, Role::Agent);
    let session = open_session(&h, &t, "page-ws-close").await;

    let (mut sock, _) = tokio_tungstenite::connect_async(ws_request(&h.process.ws_url(), &t))
        .await
        .unwrap();
    send_json(&mut sock, json!({ "type": "hello", "session": session })).await;

    // Apply one op so close has something to snapshot.
    let mut client = Client::new();
    let op = client.insert(0, "persist-me");
    send_json(
        &mut sock,
        json!({
            "type": "op",
            "session": session,
            "op": B64.encode(op),
        }),
    )
    .await;
    let _ack = recv_json(&mut sock).await;

    // Now close via WS with commit=true.
    send_json(
        &mut sock,
        json!({
            "type": "close",
            "session": session,
            "commit": true,
        }),
    )
    .await;
    let closed = recv_json(&mut sock).await;
    assert_eq!(closed["type"], "closed");
    assert_eq!(closed["session"], session);
    assert_eq!(closed["final_version"], "v1");

    // The server should close the socket; tolerate clean close or
    // stream-end.
    let _ = tokio::time::timeout(Duration::from_secs(3), sock.next()).await;

    // After close, the session id is gone from the registry: an
    // HTTP `close_session` on the same id must surface an error.
    let body = call_raw(
        &h,
        &t,
        99,
        "close_session",
        json!({ "session": session, "commit": false }),
    )
    .await;
    assert!(
        body.get("error").is_some(),
        "second close should error; got {body}"
    );

    h.process.shutdown().await;
}

#[tokio::test]
async fn disconnect_without_close_keeps_session_alive() {
    let h = start_authed(None).await;
    let t = h.process.mint_token(TENANT, Role::Agent);
    let session = open_session(&h, &t, "page-no-close").await;

    let (mut sock, _) = tokio_tungstenite::connect_async(ws_request(&h.process.ws_url(), &t))
        .await
        .unwrap();
    send_json(&mut sock, json!({ "type": "hello", "session": session })).await;
    // Round-trip a presence frame so we know the loop is up.
    send_json(
        &mut sock,
        json!({ "type": "presence", "session": session, "user": "u", "anchor": "a" }),
    )
    .await;
    let echo = recv_json(&mut sock).await;
    assert_eq!(echo["type"], "presence");

    // Drop the WS without sending a `close` frame.
    sock.close(None).await.ok();
    drop(sock);
    // Give the server a moment to notice the disconnect.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // The session must still be alive: HTTP `close_session`
    // succeeds.
    let result = call_ok(
        &h,
        &t,
        50,
        "close_session",
        json!({ "session": session, "commit": false }),
    )
    .await;
    assert_eq!(result["ok"], true);

    h.process.shutdown().await;
}

#[tokio::test]
async fn presence_frame_round_trips_in_session_mode() {
    let h = start_authed(None).await;
    let t = h.process.mint_token(TENANT, Role::Agent);
    let session = open_session(&h, &t, "page-presence").await;

    let (mut sock, _) = tokio_tungstenite::connect_async(ws_request(&h.process.ws_url(), &t))
        .await
        .unwrap();
    send_json(&mut sock, json!({ "type": "hello", "session": session })).await;

    send_json(
        &mut sock,
        json!({
            "type": "presence",
            "session": session,
            "user": "alice",
            "anchor": "#section-1",
        }),
    )
    .await;
    let echo = recv_json(&mut sock).await;
    assert_eq!(echo["type"], "presence");
    assert_eq!(echo["session"], session);
    assert_eq!(echo["user"], "alice");
    assert_eq!(echo["anchor"], "#section-1");

    sock.close(None).await.ok();
    h.process.shutdown().await;
}

#[tokio::test]
async fn op_debits_writes_quota() {
    // writes_per_minute=1 → first op succeeds, second op is
    // refused with an `error` frame referencing the writes
    // dimension. The connection stays open so the client can
    // retry after a backoff.
    let q = QuotaConfig {
        queries_per_minute: 600,
        writes_per_minute: 1,
        embeds_per_minute: 300,
        concurrent_sessions: 8,
    };
    let h = start_authed(Some(Arc::new(QuotaManager::new(q)))).await;
    let t = h.process.mint_token(TENANT, Role::Agent);
    let session = open_session(&h, &t, "page-quota-ws").await;

    let (mut sock, _) = tokio_tungstenite::connect_async(ws_request(&h.process.ws_url(), &t))
        .await
        .unwrap();
    send_json(&mut sock, json!({ "type": "hello", "session": session })).await;

    let mut client = Client::new();
    let op = client.insert(0, "x");
    send_json(
        &mut sock,
        json!({
            "type": "op",
            "session": session,
            "op": B64.encode(op),
        }),
    )
    .await;
    let ack = recv_json(&mut sock).await;
    assert_eq!(ack["type"], "op_ack");

    // Second op: bucket dry → expect an error frame.
    let op2 = client.insert(client.body_len(), "y");
    send_json(
        &mut sock,
        json!({
            "type": "op",
            "session": session,
            "op": B64.encode(op2),
        }),
    )
    .await;
    let err = recv_json(&mut sock).await;
    assert_eq!(err["type"], "error");
    assert_eq!(err["code"], "quota_exhausted");

    sock.close(None).await.ok();
    h.process.shutdown().await;
}
