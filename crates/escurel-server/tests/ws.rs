//! End-to-end tests for the WebSocket scaffolding on `/ws`.
//!
//! Real axum gateway, real OidcVerifier against the in-process
//! JWKS the support crate stands up, real signed JWTs, real
//! WebSocket client (`tokio-tungstenite`).

use std::sync::Arc;
use std::time::Duration;

use escurel_quota::{QuotaConfig, QuotaManager};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::StatusCode;
use tokio_tungstenite::tungstenite::protocol::Message;
use tokio_tungstenite::tungstenite::{self, handshake::client::Request as WsRequest};

const TENANT: &str = "acme";

const CUSTOMER_SKILL: &str = "---\ntype: skill\nid: customer\ndescription: x\n---\n# customer\n";

async fn start_authed(quota: Option<Arc<QuotaManager>>) -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("customer", CUSTOMER_SKILL)
                .done(),
        ),
        config_overrides: ConfigOverrides {
            quota,
            disable_grpc: true,
            ..Default::default()
        },
    })
    .await
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
    let p = start_authed(None).await;
    let t = p.mint_token(TENANT, Role::Agent);
    let (mut sock, _resp) = tokio_tungstenite::connect_async(ws_request(&p.ws_url(), Some(&t)))
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
    p.shutdown().await;
}

#[tokio::test]
async fn presence_frame_round_trips() {
    let p = start_authed(None).await;
    let t = p.mint_token(TENANT, Role::Agent);
    let (mut sock, _) = tokio_tungstenite::connect_async(ws_request(&p.ws_url(), Some(&t)))
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
    p.shutdown().await;
}

#[tokio::test]
async fn search_subscribe_acks_with_empty_event_in_m3() {
    let p = start_authed(None).await;
    let t = p.mint_token(TENANT, Role::Agent);
    let (mut sock, _) = tokio_tungstenite::connect_async(ws_request(&p.ws_url(), Some(&t)))
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
    p.shutdown().await;
}

#[tokio::test]
async fn missing_bearer_rejects_upgrade() {
    let p = start_authed(None).await;
    let result = tokio_tungstenite::connect_async(ws_request(&p.ws_url(), None)).await;
    let err = result.expect_err("upgrade without bearer must fail");
    match err {
        tungstenite::Error::Http(resp) => {
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        }
        other => panic!("expected HTTP 401 rejection, got {other:?}"),
    }
    p.shutdown().await;
}

#[tokio::test]
async fn invalid_token_rejects_upgrade() {
    let p = start_authed(None).await;
    let result =
        tokio_tungstenite::connect_async(ws_request(&p.ws_url(), Some("not.a.real.jwt"))).await;
    let err = result.expect_err("upgrade with bad token must fail");
    match err {
        tungstenite::Error::Http(resp) => {
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        }
        other => panic!("expected HTTP 401 rejection, got {other:?}"),
    }
    p.shutdown().await;
}

#[tokio::test]
async fn session_hello_with_unknown_id_returns_unknown_session_error() {
    // M4.4 wires WS attach to an open `SessionManager` entry; a
    // `hello.session = <id>` for an id the registry doesn't know
    // is rejected with `unknown_session` and the socket closes.
    // The end-to-end attach happy path lives in `tests/ws_session.rs`;
    // this M3-era harness has no `crdt_backend` wired and so can
    // only exercise the negative path.
    let p = start_authed(None).await;
    let t = p.mint_token(TENANT, Role::Agent);
    let (mut sock, _) = tokio_tungstenite::connect_async(ws_request(&p.ws_url(), Some(&t)))
        .await
        .unwrap();

    send_json(&mut sock, json!({ "type": "hello", "session": "sess_xyz" })).await;

    let err = recv_json(&mut sock).await;
    assert_eq!(err["type"], "error");
    assert_eq!(err["code"], "unknown_session");

    // Server should close the socket after the error.
    // We tolerate either a Close frame or a clean stream-end.
    let next = tokio::time::timeout(Duration::from_secs(3), sock.next()).await;
    match next {
        Ok(Some(Ok(Message::Close(_)))) | Ok(None) => {}
        Ok(Some(Ok(other))) => panic!("expected close, got {other:?}"),
        Ok(Some(Err(_))) => {}
        Err(_) => panic!("server did not close after unknown_session error"),
    }
    p.shutdown().await;
}

#[tokio::test]
async fn concurrent_session_quota_caps_open_connections() {
    let q = QuotaConfig {
        queries_per_minute: 600,
        writes_per_minute: 120,
        embeds_per_minute: 300,
        concurrent_sessions: 1,
    };
    let p = start_authed(Some(Arc::new(QuotaManager::new(q)))).await;
    let t = p.mint_token(TENANT, Role::Agent);

    // First connection: occupies the only session slot.
    let (mut sock1, _) = tokio_tungstenite::connect_async(ws_request(&p.ws_url(), Some(&t)))
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
    let result = tokio_tungstenite::connect_async(ws_request(&p.ws_url(), Some(&t))).await;
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
    let (mut sock2, _) = tokio_tungstenite::connect_async(ws_request(&p.ws_url(), Some(&t)))
        .await
        .expect("third ws connect must succeed after first drops");
    send_json(
        &mut sock2,
        json!({ "type": "hello", "presence_only": true }),
    )
    .await;
    sock2.close(None).await.ok();

    p.shutdown().await;
}

#[tokio::test]
async fn unknown_frame_returns_error_but_keeps_connection_open() {
    let p = start_authed(None).await;
    let t = p.mint_token(TENANT, Role::Agent);
    let (mut sock, _) = tokio_tungstenite::connect_async(ws_request(&p.ws_url(), Some(&t)))
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
    p.shutdown().await;
}
