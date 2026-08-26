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

// --- ACL ---------------------------------------------------------------------
//
// A live subscription is a READ that repeats itself. `ws_attach_acl` establishes
// the same property for CRDT sessions and states the rule it comes from: a
// socket must not become a side channel around group ACLs. It has to hold here
// for a sharper reason — a session leaks only to someone who attaches and asks,
// while a subscription leaks by being TOLD, on every index mutation, without
// the subscriber doing anything at all.
//
// `search_subscribe` passes `caller.acl()` into the re-run, so this ought to
// hold. Nothing asserted it: both tests above use a single token and a `public`
// skill, so every hit was readable by construction and an ACL that did nothing
// would have passed them.

const DIARY_SKILL: &str = "---\ntype: skill\nid: diary\ndescription: A diary.\n\
    visibility: owner\nowner_field: credential\n---\n# diary\n";
const ALICE_DIARY: &str = "---\ntype: instance\nskill: diary\nid: alice\n\
    credential: \"whatsapp:111\"\n---\n# alice\n\
    The quartz oscillator hums in Alice's private diary.\n";

const ALICE: &str = "whatsapp:111";
const BOB: &str = "whatsapp:222";

async fn start_with_a_private_page() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides::default(),
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("diary", DIARY_SKILL)
                .instance("diary", "alice", ALICE_DIARY)
                .done(),
        ),
    })
    .await
}

/// Subscribe as `token`, and return the socket plus the initial `search_event`.
async fn subscribe(p: &EscurelProcess, token: &str, sub_id: &str) -> (Sock, Value) {
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
            "subscription_id": sub_id,
            "q": "quartz oscillator",
            "k": 10,
        })
        .to_string(),
    ))
    .await
    .expect("subscribe");
    let first = recv_json(&mut sock, 5).await.expect("initial search_event");
    (sock, first)
}

fn mentions_alices_diary(frame: &Value) -> bool {
    frame["hits"].as_array().is_some_and(|hits| {
        hits.iter().any(|h| {
            h["page_id"]
                .as_str()
                .is_some_and(|p| p.contains("diary/alice"))
        })
    })
}

/// A subscription does not hand a subscriber pages they may not read — neither
/// in the initial results nor in what is pushed afterwards.
#[tokio::test]
async fn a_subscription_is_not_a_side_channel_around_the_acl() {
    let p = start_with_a_private_page().await;
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let bob = p.mint_token_with_sub(TENANT, Role::Agent, BOB);

    // THE CONTROL, first and in the same test: Alice's own subscription DOES
    // return her diary. Without it, a search that matched nothing at all — a
    // broken query, an empty index, an FTS lane that never fired — would
    // satisfy every assertion about Bob below while proving nothing about the
    // ACL. This is the assertion that makes the silence meaningful.
    let (_alice_sock, alice_first) = subscribe(&p, &alice, "sub-alice").await;
    assert!(
        mentions_alices_diary(&alice_first),
        "the owner's own subscription must return her page, or the query is \
         what is filtering and not the ACL: {alice_first}"
    );

    // Bob subscribes to the identical query.
    let (mut bob_sock, bob_first) = subscribe(&p, &bob, "sub-bob").await;
    assert_eq!(bob_first["type"], "search_event", "{bob_first}");
    assert!(
        !mentions_alices_diary(&bob_first),
        "Bob's initial results carried a page he may not read: {bob_first}"
    );

    // Now the page is WRITTEN, which moves the index's mutation epoch and is
    // what makes a subscription a subscription. A filter applied once at
    // subscribe time and forgotten on the re-run would pass the assertion above
    // and fail here — and it is the re-run that arrives unbidden.
    let resp: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {alice}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "update_page", "arguments": {
                "page_id": "markdown/instances/diary/alice.md",
                "content": "---\ntype: instance\nskill: diary\nid: alice\n\
                    credential: \"whatsapp:111\"\n---\n# alice\n\
                    The quartz oscillator hums louder in Alice's private diary.\n",
            }},
        }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json");
    assert!(resp.get("error").is_none(), "Alice's write landed: {resp}");

    // Everything Bob is pushed for the next few seconds. The epoch tick is
    // 500ms, so this window covers several re-runs.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline - tokio::time::Instant::now();
        let Ok(Some(frame)) =
            tokio::time::timeout(remaining, async { recv_json(&mut bob_sock, 4).await }).await
        else {
            break;
        };
        assert!(
            !mentions_alices_diary(&frame),
            "a re-run pushed Bob a page he may not read — the subscription is \
             a side channel around the ACL: {frame}"
        );
    }
}

/// A reconnecting subscriber loses nothing that changed while it was away.
///
/// CR-3's third acceptance criterion is that reconnection either does not drop
/// changes or documents the gap so clients can reconcile. `event_subscribe`
/// answers it with `since_event_id` and `replayed`. `search_subscribe` answers
/// it structurally instead: a `search_event` is the CURRENT RESULT SET, not a
/// delta, so the initial frame after reconnecting already contains whatever
/// landed during the silence and there is nothing to replay.
///
/// That is exactly the sort of claim that is obviously true and worth an
/// assertion anyway — it stops holding the moment anyone optimises the frame
/// into "hits that changed since the last push", which would be a reasonable
/// change to make and would silently break every reconnecting client.
#[tokio::test]
async fn a_resubscribe_carries_what_changed_while_the_client_was_gone() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);

    let (mut sock, first) = subscribe(&p, &token, "sub-gone").await;
    assert!(
        !first["hits"]
            .as_array()
            .is_some_and(|h| h.iter().any(|x| x["page_id"]
                .as_str()
                .is_some_and(|p| p.contains("note/gneiss")))),
        "the page must not exist yet, or the test proves nothing: {first}"
    );

    // Away.
    sock.close(None).await.expect("close");
    drop(sock);

    // A matching page lands while nobody is listening.
    let resp: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "update_page", "arguments": {
                "page_id": "markdown/instances/note/gneiss.md",
                "content": "---\ntype: instance\nskill: note\nid: gneiss\n---\n# gneiss\n\
                    A quartz oscillator was fitted while nobody watched.\n",
            }},
        }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json");
    assert!(resp.get("error").is_none(), "the write landed: {resp}");

    // Back. The first frame alone must carry it — no replay frame, no poll.
    let (_sock, again) = subscribe(&p, &token, "sub-back").await;
    assert!(
        again["hits"]
            .as_array()
            .is_some_and(|h| h.iter().any(|x| x["page_id"]
                .as_str()
                .is_some_and(|p| p.contains("note/gneiss")))),
        "a reconnecting subscriber did not get what changed while it was gone, \
         so `search_event` has become a delta and clients must now reconcile: \
         {again}"
    );
}
