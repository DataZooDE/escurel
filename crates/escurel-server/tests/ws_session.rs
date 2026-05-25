//! End-to-end tests for the M4.4 WebSocket attach-to-session path.
//!
//! Real axum gateway, real `SessionManager` + `LiveDoc` actor over
//! a real `DuckdbCrdtBackend`, real `OidcVerifier` against a
//! wiremock JWKS, real `tokio-tungstenite` client. Loro ops are
//! produced by a persistent `Client` peer per
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
use escurel_auth::{OidcConfig, OidcVerifier};
use escurel_crdt::{CrdtBackend, DuckdbCrdtBackend};
use escurel_index::Migrator;
use escurel_quota::{QuotaConfig, QuotaManager};
use escurel_server::{AlwaysReady, ServerConfig, serve};
use futures::{SinkExt, StreamExt};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use loro::{ExportMode, LoroDoc};
use rsa::pkcs1::EncodeRsaPrivateKey;
use rsa::traits::PublicKeyParts;
use rsa::{RsaPrivateKey, RsaPublicKey};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::client::Request as WsRequest;
use tokio_tungstenite::tungstenite::protocol::Message;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TENANT: &str = "acme";
const AUDIENCE: &str = "escurel";
const KID: &str = "test-kid";
const ISSUER_PATH: &str = "/realms/test";

// --- crypto fixtures (mirrors tests/ws.rs) ---------------------

struct Keys {
    private_pem: Vec<u8>,
    n_b64: String,
    e_b64: String,
}

fn keys() -> Keys {
    let mut rng = rand::thread_rng();
    let private = RsaPrivateKey::new(&mut rng, 2048).unwrap();
    let public = RsaPublicKey::from(&private);
    let private_pem = private
        .to_pkcs1_pem(rsa::pkcs1::LineEnding::LF)
        .unwrap()
        .as_bytes()
        .to_vec();
    Keys {
        private_pem,
        n_b64: b64url(&public.n().to_bytes_be()),
        e_b64: b64url(&public.e().to_bytes_be()),
    }
}

fn b64url(b: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

fn now() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

async fn jwks_mock(server: &MockServer, k: &Keys) {
    let jwks = json!({
        "keys": [{
            "kid": KID, "kty": "RSA", "alg": "RS256", "use": "sig",
            "n": k.n_b64, "e": k.e_b64,
        }]
    });
    Mock::given(method("GET"))
        .and(path(format!("{ISSUER_PATH}/protocol/openid-connect/certs")))
        .respond_with(ResponseTemplate::new(200).set_body_json(jwks))
        .mount(server)
        .await;
}

fn token(keys: &Keys, issuer: &str, tenant: &str) -> String {
    let now = now();
    let claims = json!({
        "iss": issuer,
        "aud": AUDIENCE,
        "sub": "user-1",
        "tenant": tenant,
        "iat": now,
        "exp": now + 600,
    });
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(KID.to_owned());
    let key = EncodingKey::from_rsa_pem(&keys.private_pem).unwrap();
    encode(&header, &claims, &key).unwrap()
}

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
    handle: escurel_server::ServerHandle,
    http: reqwest::Client,
    base_http_url: String,
    base_ws_url: String,
    issuer: String,
    keys: Keys,
    _db_dir: TempDir,
    _wm: MockServer,
}

async fn start_authed(quota: Option<Arc<QuotaManager>>) -> Harness {
    let wm = MockServer::start().await;
    let keys = keys();
    jwks_mock(&wm, &keys).await;
    let issuer = format!("{}{ISSUER_PATH}", wm.uri());
    let cfg = OidcConfig::new(issuer.clone(), AUDIENCE.to_owned())
        .with_jwks_uri(format!("{issuer}/protocol/openid-connect/certs"));
    let verifier = Arc::new(OidcVerifier::new(cfg));

    let db_dir = TempDir::new().unwrap();
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let shared = Arc::new(Mutex::new(conn));
    let crdt_backend: Arc<dyn CrdtBackend> = Arc::new(DuckdbCrdtBackend::new(Arc::clone(&shared)));

    let handle = serve(ServerConfig {
        listen: "127.0.0.1:0".to_owned(),
        grpc_listen: None,
        version: "1.0.0-test".to_owned(),
        readiness: Arc::new(AlwaysReady),
        indexer: None,
        verifier: Some(verifier),
        quota,
        tenant_store: None,
        crdt_backend: Some(crdt_backend),
    })
    .await
    .unwrap();
    let base_http_url = format!("http://{}", handle.local_addr);
    let base_ws_url = format!("ws://{}/ws", handle.local_addr);
    Harness {
        handle,
        http: reqwest::Client::new(),
        base_http_url,
        base_ws_url,
        issuer,
        keys,
        _db_dir: db_dir,
        _wm: wm,
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
        .post(format!("{}/mcp", h.base_http_url))
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
    body["result"].clone()
}

async fn call_raw(h: &Harness, bearer: &str, id: u64, name: &str, args: Value) -> Value {
    let resp = h
        .http
        .post(format!("{}/mcp", h.base_http_url))
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
    let t = token(&h.keys, &h.issuer, TENANT);
    let session = open_session(&h, &t, "page-attach").await;

    let (mut sock, _) = tokio_tungstenite::connect_async(ws_request(&h.base_ws_url, &t))
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
    h.handle.shutdown().await;
}

#[tokio::test]
async fn op_via_ws_updates_doc_and_replies_with_op_ack() {
    let h = start_authed(None).await;
    let t = token(&h.keys, &h.issuer, TENANT);
    let session = open_session(&h, &t, "page-op-ws").await;

    let (mut sock, _) = tokio_tungstenite::connect_async(ws_request(&h.base_ws_url, &t))
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
    h.handle.shutdown().await;
}

#[tokio::test]
async fn unknown_session_id_returns_error_and_closes() {
    let h = start_authed(None).await;
    let t = token(&h.keys, &h.issuer, TENANT);
    let (mut sock, _) = tokio_tungstenite::connect_async(ws_request(&h.base_ws_url, &t))
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
    h.handle.shutdown().await;
}

#[tokio::test]
async fn close_frame_persists_and_closes_session() {
    let h = start_authed(None).await;
    let t = token(&h.keys, &h.issuer, TENANT);
    let session = open_session(&h, &t, "page-ws-close").await;

    let (mut sock, _) = tokio_tungstenite::connect_async(ws_request(&h.base_ws_url, &t))
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

    h.handle.shutdown().await;
}

#[tokio::test]
async fn disconnect_without_close_keeps_session_alive() {
    let h = start_authed(None).await;
    let t = token(&h.keys, &h.issuer, TENANT);
    let session = open_session(&h, &t, "page-no-close").await;

    let (mut sock, _) = tokio_tungstenite::connect_async(ws_request(&h.base_ws_url, &t))
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

    h.handle.shutdown().await;
}

#[tokio::test]
async fn presence_frame_round_trips_in_session_mode() {
    let h = start_authed(None).await;
    let t = token(&h.keys, &h.issuer, TENANT);
    let session = open_session(&h, &t, "page-presence").await;

    let (mut sock, _) = tokio_tungstenite::connect_async(ws_request(&h.base_ws_url, &t))
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
    h.handle.shutdown().await;
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
    let t = token(&h.keys, &h.issuer, TENANT);
    let session = open_session(&h, &t, "page-quota-ws").await;

    let (mut sock, _) = tokio_tungstenite::connect_async(ws_request(&h.base_ws_url, &t))
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
    h.handle.shutdown().await;
}
