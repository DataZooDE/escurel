//! End-to-end tests for the WebSocket scaffolding on `/ws`.
//!
//! Real axum gateway, real OidcVerifier against a wiremock JWKS
//! endpoint with a freshly-generated 2048-bit RSA pair, real signed
//! JWTs, real WebSocket client (`tokio-tungstenite`). The only
//! "fake" is the wiremock JWKS server that feeds the verifier —
//! identical to the pattern in `auth_quota.rs`.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use duckdb::Connection;
use escurel_auth::{OidcConfig, OidcVerifier};
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_quota::{QuotaConfig, QuotaManager};
use escurel_server::{AlwaysReady, ServerConfig, serve};
use escurel_storage::{FsStore, Key, LaneStore};
use futures::{SinkExt, StreamExt};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use rsa::pkcs1::EncodeRsaPrivateKey;
use rsa::traits::PublicKeyParts;
use rsa::{RsaPrivateKey, RsaPublicKey};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::StatusCode;
use tokio_tungstenite::tungstenite::protocol::Message;
use tokio_tungstenite::tungstenite::{self, handshake::client::Request as WsRequest};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TENANT: &str = "acme";
const AUDIENCE: &str = "escurel";
const KID: &str = "test-kid";
const ISSUER_PATH: &str = "/realms/test";

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

async fn make_indexer() -> (Arc<Indexer>, TempDir, TempDir) {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Arc::new(Indexer::new(Arc::clone(&store), embedder, conn, TENANT).unwrap());

    // Minimal seed so list_skills returns something.
    let body = "---\ntype: skill\nid: customer\ndescription: x\n---\n# customer\n";
    let key = Key::new(TENANT, "markdown/skills/customer.md".to_owned()).unwrap();
    store
        .write(&key, Bytes::from_static(body.as_bytes()))
        .await
        .unwrap();
    indexer
        .update_page("markdown/skills/customer.md", body)
        .await
        .unwrap();

    (indexer, store_dir, db_dir)
}

struct Harness {
    handle: escurel_server::ServerHandle,
    base_ws_url: String,
    issuer: String,
    keys: Keys,
    _store_dir: TempDir,
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

    let (indexer, store_dir, db_dir) = make_indexer().await;

    let handle = serve(ServerConfig {
        listen: "127.0.0.1:0".to_owned(),
        grpc_listen: None,
        version: "1.0.0-test".to_owned(),
        readiness: Arc::new(AlwaysReady),
        indexer: Some(indexer),
        verifier: Some(verifier),
        quota,
        tenant_store: None,
    })
    .await
    .unwrap();
    let base_ws_url = format!("ws://{}/ws", handle.local_addr);
    Harness {
        handle,
        base_ws_url,
        issuer,
        keys,
        _store_dir: store_dir,
        _db_dir: db_dir,
        _wm: wm,
    }
}

fn ws_request(url: &str, bearer: Option<&str>) -> WsRequest {
    let mut req = url.into_client_request().unwrap();
    if let Some(t) = bearer {
        req.headers_mut()
            .insert("authorization", format!("Bearer {t}").parse().unwrap());
    }
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

#[tokio::test]
async fn presence_only_hello_keeps_connection_open() {
    let h = start_authed(None).await;
    let t = token(&h.keys, &h.issuer, TENANT);
    let (mut sock, _resp) = tokio_tungstenite::connect_async(ws_request(&h.base_ws_url, Some(&t)))
        .await
        .expect("ws connect");

    send_json(&mut sock, json!({ "type": "hello", "presence_only": true })).await;

    // Server must NOT close after a valid presence_only hello.
    // We probe by sending a presence frame and expecting the echo.
    send_json(
        &mut sock,
        json!({
            "type": "presence",
            "session": "s1",
            "user": "user-1",
            "anchor": "#intro",
        }),
    )
    .await;
    let echo = recv_json(&mut sock).await;
    assert_eq!(echo["type"], "presence");
    assert_eq!(echo["session"], "s1");

    sock.close(None).await.ok();
    h.handle.shutdown().await;
}

#[tokio::test]
async fn presence_frame_round_trips() {
    let h = start_authed(None).await;
    let t = token(&h.keys, &h.issuer, TENANT);
    let (mut sock, _) = tokio_tungstenite::connect_async(ws_request(&h.base_ws_url, Some(&t)))
        .await
        .unwrap();

    send_json(&mut sock, json!({ "type": "hello", "presence_only": true })).await;
    send_json(
        &mut sock,
        json!({
            "type": "presence",
            "session": "sess-42",
            "user": "alice",
            "anchor": "#section-1",
        }),
    )
    .await;

    let echo = recv_json(&mut sock).await;
    assert_eq!(echo["type"], "presence");
    assert_eq!(echo["session"], "sess-42");
    assert_eq!(echo["user"], "alice");
    assert_eq!(echo["anchor"], "#section-1");

    sock.close(None).await.ok();
    h.handle.shutdown().await;
}

#[tokio::test]
async fn search_subscribe_acks_with_empty_event_in_m3() {
    let h = start_authed(None).await;
    let t = token(&h.keys, &h.issuer, TENANT);
    let (mut sock, _) = tokio_tungstenite::connect_async(ws_request(&h.base_ws_url, Some(&t)))
        .await
        .unwrap();

    send_json(&mut sock, json!({ "type": "hello", "presence_only": true })).await;
    send_json(
        &mut sock,
        json!({
            "type": "search_subscribe",
            "subscription_id": "sub-1",
            "q": "anything",
            "k": 10,
        }),
    )
    .await;

    let ack = recv_json(&mut sock).await;
    assert_eq!(ack["type"], "search_event");
    assert_eq!(ack["subscription_id"], "sub-1");
    assert!(
        ack["hits"].as_array().is_some_and(Vec::is_empty),
        "M3 placeholder must ack with empty hits array; got {ack}"
    );

    sock.close(None).await.ok();
    h.handle.shutdown().await;
}

#[tokio::test]
async fn missing_bearer_rejects_upgrade() {
    let h = start_authed(None).await;
    let result = tokio_tungstenite::connect_async(ws_request(&h.base_ws_url, None)).await;
    let err = result.expect_err("upgrade without bearer must fail");
    match err {
        tungstenite::Error::Http(resp) => {
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        }
        other => panic!("expected HTTP 401 rejection, got {other:?}"),
    }
    h.handle.shutdown().await;
}

#[tokio::test]
async fn invalid_token_rejects_upgrade() {
    let h = start_authed(None).await;
    let result =
        tokio_tungstenite::connect_async(ws_request(&h.base_ws_url, Some("not.a.real.jwt"))).await;
    let err = result.expect_err("upgrade with bad token must fail");
    match err {
        tungstenite::Error::Http(resp) => {
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        }
        other => panic!("expected HTTP 401 rejection, got {other:?}"),
    }
    h.handle.shutdown().await;
}

#[tokio::test]
async fn session_hello_returns_live_session_unavailable_error_in_m3() {
    let h = start_authed(None).await;
    let t = token(&h.keys, &h.issuer, TENANT);
    let (mut sock, _) = tokio_tungstenite::connect_async(ws_request(&h.base_ws_url, Some(&t)))
        .await
        .unwrap();

    send_json(&mut sock, json!({ "type": "hello", "session": "sess_xyz" })).await;

    let err = recv_json(&mut sock).await;
    assert_eq!(err["type"], "error");
    assert_eq!(err["code"], "live_session_unavailable");

    // Server should close the socket after the error.
    // We tolerate either a Close frame or a clean stream-end.
    let next = tokio::time::timeout(Duration::from_secs(3), sock.next()).await;
    match next {
        Ok(Some(Ok(Message::Close(_)))) | Ok(None) => {}
        Ok(Some(Ok(other))) => panic!("expected close, got {other:?}"),
        Ok(Some(Err(_))) => {}
        Err(_) => panic!("server did not close after session hello in M3"),
    }
    h.handle.shutdown().await;
}

#[tokio::test]
async fn concurrent_session_quota_caps_open_connections() {
    let q = QuotaConfig {
        queries_per_minute: 600,
        writes_per_minute: 120,
        embeds_per_minute: 300,
        concurrent_sessions: 1,
    };
    let h = start_authed(Some(Arc::new(QuotaManager::new(q)))).await;
    let t = token(&h.keys, &h.issuer, TENANT);

    // First connection: occupies the only session slot.
    let (mut sock1, _) = tokio_tungstenite::connect_async(ws_request(&h.base_ws_url, Some(&t)))
        .await
        .expect("first ws connect must succeed");
    send_json(
        &mut sock1,
        json!({ "type": "hello", "presence_only": true }),
    )
    .await;
    // Round-trip a presence frame so we're sure the connection is
    // fully accepted (the slot is debited) before we open the second.
    send_json(
        &mut sock1,
        json!({
            "type": "presence", "session": "s", "user": "u", "anchor": "a",
        }),
    )
    .await;
    let _ = recv_json(&mut sock1).await;

    // Second connection: must be refused with HTTP 429.
    let result = tokio_tungstenite::connect_async(ws_request(&h.base_ws_url, Some(&t))).await;
    let err = result.expect_err("second upgrade must be rejected on session-cap");
    match err {
        tungstenite::Error::Http(resp) => {
            assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        }
        other => panic!("expected HTTP 429 rejection, got {other:?}"),
    }

    // After we drop the first connection, the slot becomes
    // available again.
    sock1.close(None).await.ok();
    drop(sock1);
    // Give the server a moment to release the permit.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let (mut sock2, _) = tokio_tungstenite::connect_async(ws_request(&h.base_ws_url, Some(&t)))
        .await
        .expect("third ws connect must succeed after first drops");
    send_json(
        &mut sock2,
        json!({ "type": "hello", "presence_only": true }),
    )
    .await;
    sock2.close(None).await.ok();

    h.handle.shutdown().await;
}

#[tokio::test]
async fn unknown_frame_returns_error_but_keeps_connection_open() {
    let h = start_authed(None).await;
    let t = token(&h.keys, &h.issuer, TENANT);
    let (mut sock, _) = tokio_tungstenite::connect_async(ws_request(&h.base_ws_url, Some(&t)))
        .await
        .unwrap();

    send_json(&mut sock, json!({ "type": "hello", "presence_only": true })).await;
    send_json(
        &mut sock,
        json!({ "type": "definitely-not-a-real-frame", "x": 1 }),
    )
    .await;

    let err = recv_json(&mut sock).await;
    assert_eq!(err["type"], "error");
    assert_eq!(err["code"], "unknown_frame");

    // Connection still alive: a follow-up presence frame round-trips.
    send_json(
        &mut sock,
        json!({
            "type": "presence", "session": "s", "user": "u", "anchor": "a",
        }),
    )
    .await;
    let echo = recv_json(&mut sock).await;
    assert_eq!(echo["type"], "presence");

    sock.close(None).await.ok();
    h.handle.shutdown().await;
}
