//! `event_assigned` on `/ws`: when an inbox event is assigned to an instance,
//! a subscriber that could see it is told, so an open Inbox drops the row
//! instead of showing it until the next manual refresh. Real gateway, real
//! WebSocket, event ACL enforced.
//!
//! A separate frame, not a second `event` frame: clients dedupe `event` frames
//! by `event_id` (the subscribe/replay overlap), and a consumer that reads
//! every `event` frame as a new capture would put the assigned event back.

use std::time::Duration;

use escurel_test_support::{
    AuthMode, ConfigOverrides, EscurelProcess, EventAclMode, FixtureBuilder, Opts, Role,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

type Sock =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

const TENANT: &str = "stuttgart-ai";
const ALICE: &str = "consultant:alice";
const BOB: &str = "consultant:bob";
const MEMBER_SKILL: &str = "---\nkind: skill\nid: community_member\n\
    description: A member.\nvisibility: owner\nowner_field: credential\n---\n# community_member\n";
const ALICE_MEMBER: &str = "---\nkind: instance\nskill: community_member\nid: alice\n\
    credential: \"consultant:alice\"\n---\n# Alice\n";
const ALICE_PAGE: &str = "markdown/instances/community_member/alice.md";

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            event_acl: Some(EventAclMode::Enforce),
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

async fn call(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    let resp: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": { "name": name, "arguments": args } }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json");
    assert!(resp.get("error").is_none(), "{name}: {resp}");
    resp["result"]["structuredContent"].clone()
}

async fn subscribe(p: &EscurelProcess, bearer: &str) -> Sock {
    let mut req = p.ws_url().into_client_request().unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {bearer}").parse().unwrap());
    let (mut sock, _) = tokio_tungstenite::connect_async(req).await.expect("ws");
    sock.send(Message::Text(
        json!({ "type": "hello", "presence_only": true }).to_string(),
    ))
    .await
    .unwrap();
    sock.send(Message::Text(
        json!({ "type": "event_subscribe", "subscription_id": "s1" }).to_string(),
    ))
    .await
    .unwrap();
    let ack = recv(&mut sock, 3)
        .await
        .expect("an answer to the subscribe");
    assert_eq!(ack["type"], "event_subscribe_ack", "{ack}");
    sock
}

async fn recv(sock: &mut Sock, secs: u64) -> Option<Value> {
    match tokio::time::timeout(Duration::from_secs(secs), sock.next()).await {
        Ok(Some(Ok(Message::Text(t)))) => Some(serde_json::from_str(&t).unwrap()),
        _ => None,
    }
}

/// Every frame until the stream goes quiet for `secs`.
async fn drain(sock: &mut Sock, secs: u64) -> Vec<Value> {
    let mut out = Vec::new();
    while let Some(f) = recv(sock, secs).await {
        out.push(f);
    }
    out
}

#[tokio::test]
async fn assigning_an_event_pushes_event_assigned_to_a_subscriber_who_can_read_it() {
    let p = start().await;
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let bob = p.mint_token_with_sub(TENANT, Role::Agent, BOB);
    let mut alice_sock = subscribe(&p, &alice).await;
    let mut bob_sock = subscribe(&p, &bob).await;

    call(
        &p,
        &alice,
        "capture_event",
        json!({ "event_id": "EVT-A1", "source": "manual", "mime": "text/plain",
                "label_skill": "community_member", "title": "a note", "body": "x" }),
    )
    .await;
    let captured = drain(&mut alice_sock, 2).await;
    assert!(
        captured
            .iter()
            .any(|f| f["type"] == "event" && f["event"]["event_id"] == "EVT-A1"),
        "alice sees her capture: {captured:?}"
    );

    call(
        &p,
        &alice,
        "assign_event",
        json!({ "event_id": "EVT-A1", "instance_page_id": ALICE_PAGE }),
    )
    .await;

    let after = drain(&mut alice_sock, 2).await;
    let assigned: Vec<&Value> = after
        .iter()
        .filter(|f| f["type"] == "event_assigned")
        .collect();
    assert_eq!(assigned.len(), 1, "exactly one event_assigned: {after:?}");
    let f = assigned[0];
    assert_eq!(f["subscription_id"], "s1", "{f}");
    assert_eq!(f["event_id"], "EVT-A1", "{f}");
    assert_eq!(f["instance_page_id"], ALICE_PAGE, "{f}");
    assert_eq!(f["status"], "processed", "{f}");
    assert!(
        !after.iter().any(|f| f["type"] == "event"),
        "an assignment is not a new event: {after:?}"
    );

    // Bob can read neither the inbox event nor alice's owner-only page.
    let bobs = drain(&mut bob_sock, 1).await;
    assert!(
        !bobs.iter().any(|f| f.to_string().contains("EVT-A1")),
        "a stranger learns nothing about the event: {bobs:?}"
    );
}
