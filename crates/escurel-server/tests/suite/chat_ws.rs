//! `event_subscribe` with a `chat_group_id` filter (governed chat agent,
//! BRD FR-G1). Real gateway, real WebSocket, raw JSON-RPC to append.
//!
//! A chat subscription receives `chat_message` frames — not events — for one
//! group, on every successful append. The subscriber must pass
//! `may_access_chat` at subscribe time; a stranger is refused and is pushed
//! nothing. An idempotent retry (same `msg_id`) stores nothing new and so is
//! not pushed again.

use std::time::Duration;

use escurel_server::WriteAclMode;
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

type Sock =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

const TENANT: &str = "stuttgart-ai";
const ALICE: &str = "whatsapp:111";
const BOB: &str = "whatsapp:222";
const NOTE_PAGE: &str = "markdown/instances/note/n-001.md";
const NOTE_SKILL: &str = "---\nkind: skill\nid: note\ndescription: Note.\nvisibility: owner\n\
    owner_field: owner\nchat:\n  page_bound: true\n---\n# Note\n";
const NOTE_RECORD: &str =
    "---\nkind: instance\nskill: note\nid: n-001\nowner: \"whatsapp:111\"\n---\n# Note\n";

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            write_acl: Some(WriteAclMode::Enforce),
            ..Default::default()
        },
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("note", NOTE_SKILL)
                .instance("note", "n-001", NOTE_RECORD)
                .done(),
        ),
    })
    .await
}

fn page_chat(subject: &str, page_id: &str) -> String {
    format!(
        "page-chat:v1:{:x}:{page_id}",
        Sha256::digest(subject.as_bytes())
    )
}

async fn append(p: &EscurelProcess, token: &str, group: &str, msg_id: &str, content: &str) {
    let resp: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "append_message", "arguments": {
                "chat_group_id": group, "role": "user", "content": content,
                "msg_id": msg_id, "embed": false,
            } },
        }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json");
    assert!(resp.get("error").is_none(), "append: {resp}");
}

/// Open a presence-only socket, send `event_subscribe` with `filters`, and
/// return the socket plus the first answer frame.
async fn subscribe(p: &EscurelProcess, bearer: &str, filters: Value) -> (Sock, Value) {
    let mut req = p.ws_url().into_client_request().unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {bearer}").parse().unwrap());
    let (mut sock, _) = tokio_tungstenite::connect_async(req).await.expect("ws");
    sock.send(Message::Text(
        json!({ "type": "hello", "presence_only": true }).to_string(),
    ))
    .await
    .unwrap();
    let frame = json!({ "type": "event_subscribe", "subscription_id": "c1", "filters": filters });
    sock.send(Message::Text(frame.to_string())).await.unwrap();
    let first = recv(&mut sock, 3)
        .await
        .expect("an answer to the subscribe");
    (sock, first)
}

async fn recv(sock: &mut Sock, secs: u64) -> Option<Value> {
    match tokio::time::timeout(Duration::from_secs(secs), sock.next()).await {
        Ok(Some(Ok(Message::Text(t)))) => Some(serde_json::from_str(&t).unwrap()),
        _ => None,
    }
}

/// Collect `chat_message` frames until the stream goes quiet.
async fn chat_frames(sock: &mut Sock) -> Vec<Value> {
    let mut out = Vec::new();
    while let Some(f) = recv(sock, 2).await {
        if f["type"] == "chat_message" {
            out.push(f);
        }
    }
    out
}

#[tokio::test]
async fn a_page_chat_subscriber_receives_appended_messages_and_a_stranger_is_refused() {
    let p = start().await;
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let bob = p.mint_token_with_sub(TENANT, Role::Agent, BOB);
    let group = page_chat(ALICE, NOTE_PAGE);

    let (mut owner, ack) = subscribe(&p, &alice, json!({ "chat_group_id": group })).await;
    assert_eq!(ack["type"], "event_subscribe_ack", "{ack}");

    // A stranger is refused at subscribe time and is pushed nothing.
    let (mut stranger, refused) = subscribe(&p, &bob, json!({ "chat_group_id": group })).await;
    assert_eq!(refused["type"], "error", "{refused}");
    assert_eq!(refused["code"], "forbidden", "{refused}");
    assert_eq!(refused["subscription_id"], "c1");

    append(&p, &alice, &group, "u-1", "Which customers renew?").await;
    // The retry stores nothing new, so it is not pushed a second time.
    append(&p, &alice, &group, "u-1", "Which customers renew?").await;
    append(&p, &alice, &group, "u-2", "yes").await;

    let frames = chat_frames(&mut owner).await;
    let ids: Vec<&str> = frames
        .iter()
        .map(|f| f["msg"]["msg_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["u-1", "u-2"], "{frames:?}");
    let first = &frames[0];
    assert_eq!(first["subscription_id"], "c1");
    assert_eq!(first["chat_group_id"], group.as_str());
    assert_eq!(first["msg"]["role"], "user");
    assert_eq!(first["msg"]["author"], ALICE, "server-stamped author");
    assert_eq!(first["msg"]["content"], "Which customers renew?");
    assert!(first["msg"]["ts"].is_string());

    assert!(chat_frames(&mut stranger).await.is_empty());
}

#[tokio::test]
async fn a_chat_subscription_receives_no_events_and_only_its_own_group() {
    let p = start().await;
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let admin = p.mint_token(TENANT, Role::Admin);
    let group = page_chat(ALICE, NOTE_PAGE);
    let (mut sock, ack) = subscribe(&p, &alice, json!({ "chat_group_id": group })).await;
    assert_eq!(ack["type"], "event_subscribe_ack", "{ack}");

    // Another group's message and a captured event: neither reaches it.
    append(&p, &admin, "some-other-room", "o-1", "elsewhere").await;
    reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {alice}"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "capture_event", "arguments": {
                "label_skill": "note", "source": "test", "title": "t", "body": "b" } } }))
        .send()
        .await
        .expect("capture");
    append(&p, &alice, &group, "u-1", "mine").await;

    let mut seen = Vec::new();
    while let Some(f) = recv(&mut sock, 2).await {
        seen.push(f);
    }
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(seen[0]["type"], "chat_message");
    assert_eq!(seen[0]["msg"]["msg_id"], "u-1");
}

#[tokio::test]
async fn a_chat_filter_mixed_with_event_filters_is_refused() {
    let p = start().await;
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let group = page_chat(ALICE, NOTE_PAGE);
    let (_sock, refused) = subscribe(
        &p,
        &alice,
        json!({ "chat_group_id": group, "label_skill": "note" }),
    )
    .await;
    assert_eq!(refused["type"], "error", "{refused}");
    assert_eq!(refused["code"], "invalid_subscription", "{refused}");
}
