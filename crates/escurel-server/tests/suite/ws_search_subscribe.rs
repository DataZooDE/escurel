//! WS `search_subscribe` — live search push, promoted out of its stub
//! (issue #355, narrowed). The M3 placeholder ignored the advertised
//! `{q, k, filter?}` payload and acked with an empty `search_event`;
//! now the subscription runs the REAL ACL-fused search on subscribe
//! (the initial results ARE the ack) and re-runs it when the index's
//! mutation epoch moves, pushing updated hits.
//!
//! ZeroEmbedder is in play, so ranking rides the FTS lane — queries
//! must use literal body words.

use std::time::Duration;

use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::Message;

const TENANT: &str = "stuttgart-ai";
const NOTE_SKILL: &str = "---\ntype: skill\nid: note\ndescription: A note.\n\
    visibility: public\n---\n# note\n";
const FIRST: &str = "---\ntype: instance\nskill: note\nid: quartz\n---\n# quartz\n\
    The quartz oscillator hums quietly.\n";

type Sock =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides::default(),
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("note", NOTE_SKILL)
                .instance("note", "quartz", FIRST)
                .done(),
        ),
    })
    .await
}

async fn recv_json(sock: &mut Sock, secs: u64) -> Option<Value> {
    match tokio::time::timeout(Duration::from_secs(secs), sock.next()).await {
        Err(_) => None,
        Ok(None) => None,
        Ok(Some(msg)) => match msg.expect("ws error") {
            Message::Text(t) => Some(serde_json::from_str(&t).unwrap()),
            Message::Binary(b) => Some(serde_json::from_slice(&b).unwrap()),
            _ => None,
        },
    }
}

#[tokio::test]
async fn search_subscribe_runs_the_query_and_pushes_on_index_change() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);

    let mut req = p.ws_url().into_client_request().unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    let (mut sock, _) = tokio_tungstenite::connect_async(req).await.expect("ws");
    sock.send(Message::Text(
        json!({ "type": "hello", "presence_only": true }).to_string(),
    ))
    .await
    .expect("hello");

    sock.send(Message::Text(
        json!({
            "type": "search_subscribe",
            "subscription_id": "sub-s",
            "q": "quartz oscillator",
            "k": 5,
        })
        .to_string(),
    ))
    .await
    .expect("subscribe");

    // The initial results are the ack — and they are REAL, not the
    // empty stub: the seeded page matches via FTS.
    let first = recv_json(&mut sock, 5).await.expect("initial search_event");
    assert_eq!(first["type"], "search_event", "{first}");
    assert_eq!(first["subscription_id"], "sub-s", "{first}");
    let hits = first["hits"]
        .as_array()
        .unwrap_or_else(|| panic!("{first}"));
    assert!(
        hits.iter().any(|h| h["page_id"]
            .as_str()
            .is_some_and(|p| p.contains("note/quartz"))),
        "the seeded match is in the initial results: {first}"
    );

    // A NEW matching page lands → the index's mutation epoch moves →
    // updated hits are pushed without any client action.
    let resp: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "update_page", "arguments": {
                "page_id": "markdown/instances/note/feldspar.md",
                "content": "---\ntype: instance\nskill: note\nid: feldspar\n---\n# feldspar\n\
                    Another quartz oscillator appears.\n",
            }},
        }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json");
    assert!(resp.get("error").is_none(), "write landed: {resp}");

    let pushed = recv_json(&mut sock, 10).await.expect("live search_event");
    assert_eq!(pushed["type"], "search_event", "{pushed}");
    let hits = pushed["hits"]
        .as_array()
        .unwrap_or_else(|| panic!("{pushed}"));
    assert!(
        hits.iter().any(|h| h["page_id"]
            .as_str()
            .is_some_and(|p| p.contains("note/feldspar"))),
        "the fresh page is pushed live: {pushed}"
    );
}

/// A subscribe with no query is a typed error frame, not a silent stub.
#[tokio::test]
async fn search_subscribe_without_a_query_is_refused() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);
    let mut req = p.ws_url().into_client_request().unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    let (mut sock, _) = tokio_tungstenite::connect_async(req).await.expect("ws");
    sock.send(Message::Text(
        json!({ "type": "hello", "presence_only": true }).to_string(),
    ))
    .await
    .expect("hello");
    sock.send(Message::Text(
        json!({ "type": "search_subscribe", "subscription_id": "sub-x" }).to_string(),
    ))
    .await
    .expect("subscribe");
    let out = recv_json(&mut sock, 5).await.expect("response");
    assert_eq!(out["type"], "error", "{out}");
    assert_eq!(out["code"], "invalid_subscription", "{out}");
}
