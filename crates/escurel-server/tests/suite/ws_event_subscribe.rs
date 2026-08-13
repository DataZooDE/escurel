//! WS `event_subscribe` (#333): a locally-running agent that cannot
//! host an HTTP endpoint consumes the event bus by SUBSCRIBING over the
//! WebSocket it already has, instead of polling `list_inbox`.
//!
//! Real running gateway (TestIssuer auth), real Indexer, two real
//! `tokio-tungstenite` clients. The push respects the event ACL
//! (#362's rule): under `enforce`, an un-triaged capture is pushed only
//! to the subject that captured it — a subscription must not become a
//! second, ungated read path onto the bus.

use std::time::Duration;

use escurel_test_support::{
    AuthMode, ConfigOverrides, EscurelProcess, EventAclMode, FixtureBuilder, Opts, Role,
};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::Message;

const TENANT: &str = "stuttgart-ai";
const ALICE: &str = "consultant:alice";
const BOB: &str = "consultant:bob";

const MEMBER_SKILL: &str = "---\ntype: skill\nid: community_member\n\
    description: A member.\nvisibility: owner\nowner_field: credential\n---\n# community_member\n";
const ALICE_MEMBER: &str = "---\ntype: instance\nskill: community_member\nid: alice\n\
    credential: \"consultant:alice\"\n---\n# Alice\n";

async fn start_with(mode: EventAclMode) -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            event_acl: Some(mode),
            ..Default::default()
        },
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("community_member", MEMBER_SKILL)
                .instance("community_member", "alice", ALICE_MEMBER)
                .done(),
        ),
    })
    .await
}

type Sock =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Connect, complete the presence-only hello, and subscribe to the bus.
async fn subscribed_socket(p: &EscurelProcess, bearer: &str, sub_id: &str) -> Sock {
    let mut req = p.ws_url().into_client_request().unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {bearer}").parse().unwrap());
    let (mut sock, _) = tokio_tungstenite::connect_async(req).await.expect("ws");
    sock.send(Message::Text(
        json!({ "type": "hello", "presence_only": true }).to_string(),
    ))
    .await
    .expect("hello");
    sock.send(Message::Text(
        json!({ "type": "event_subscribe", "subscription_id": sub_id }).to_string(),
    ))
    .await
    .expect("subscribe");
    // The ack must arrive before any event can, so a consumer knows the
    // subscription is live rather than silently ignored.
    let ack = recv_json(&mut sock).await.expect("subscribe ack");
    assert_eq!(ack["type"], "event_subscribe_ack", "ack frame: {ack}");
    assert_eq!(ack["subscription_id"], sub_id, "ack echoes the id: {ack}");
    sock
}

async fn recv_json(sock: &mut Sock) -> Option<Value> {
    match tokio::time::timeout(Duration::from_secs(3), sock.next()).await {
        Err(_) => None,
        Ok(None) => None,
        Ok(Some(msg)) => match msg.expect("ws error") {
            Message::Text(t) => Some(serde_json::from_str(&t).unwrap()),
            Message::Binary(b) => Some(serde_json::from_slice(&b).unwrap()),
            _ => None,
        },
    }
}

async fn capture(p: &EscurelProcess, bearer: &str, event_id: &str) {
    let resp = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {bearer}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "capture_event", "arguments": {
                "event_id": event_id,
                "source": "heron-share",
                "mime": "text/plain",
                "label_skill": "community_member",
                "title": "unreviewed",
                "body": "customer text",
            }},
        }))
        .send()
        .await
        .expect("capture post");
    let body: Value = resp.json().await.expect("capture json");
    assert!(body.get("error").is_none(), "capture ok: {body}");
}

/// The capturing subject's own subscription receives the push; another
/// non-admin subject's does NOT (enforce mode) — the subscription
/// follows exactly the `may_read_event` rule the polling surfaces use.
#[tokio::test]
async fn event_push_reaches_the_capturer_and_not_a_stranger() {
    let p = start_with(EventAclMode::Enforce).await;
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let bob = p.mint_token_with_sub(TENANT, Role::Agent, BOB);

    let mut alice_sock = subscribed_socket(&p, &alice, "sub-a").await;
    let mut bob_sock = subscribed_socket(&p, &bob, "sub-b").await;

    capture(&p, &alice, "EVT-WS-1").await;

    let pushed = recv_json(&mut alice_sock)
        .await
        .expect("alice receives her capture");
    assert_eq!(pushed["type"], "event", "push frame: {pushed}");
    assert_eq!(pushed["subscription_id"], "sub-a", "tagged: {pushed}");
    assert_eq!(pushed["event"]["event_id"], "EVT-WS-1", "payload: {pushed}");
    assert_eq!(
        pushed["event"]["source"], "heron-share",
        "payload: {pushed}"
    );

    // Bob gets NOTHING: an un-triaged inbox item is visible only to its
    // capturer, and the push path must enforce the same rule as list_inbox.
    assert!(
        recv_json(&mut bob_sock).await.is_none(),
        "a stranger's subscription must not receive alice's capture"
    );

    p.shutdown().await;
}

/// `off` keeps the legacy open bus: every subscriber sees every event,
/// matching the polling surfaces in the same mode.
#[tokio::test]
async fn off_mode_pushes_to_every_subscriber() {
    let p = start_with(EventAclMode::Off).await;
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let bob = p.mint_token_with_sub(TENANT, Role::Agent, BOB);

    let mut bob_sock = subscribed_socket(&p, &bob, "sub-b").await;

    capture(&p, &alice, "EVT-WS-2").await;

    let pushed = recv_json(&mut bob_sock)
        .await
        .expect("open bus pushes to every subscriber");
    assert_eq!(pushed["event"]["event_id"], "EVT-WS-2", "payload: {pushed}");

    p.shutdown().await;
}
