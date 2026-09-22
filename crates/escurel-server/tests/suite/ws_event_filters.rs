//! `event_subscribe` with `filters` (knowledge-workbench backend, P1 PR10 —
//! BRD FR-W-1/3). Real gateway, real WebSocket, raw JSON-RPC to capture.
//!
//! A workbench watching one thread subscribes to that thread — the fan-out
//! predicate runs server-side, before the per-event ACL — and resumes it
//! gap-free: with a `root_event_id` (or `run_id`) filter, `since_event_id`
//! replays from the lineage's event log, not the inbox, so a system event
//! that was stored `processed` while the socket was down is replayed too.

use std::time::Duration;

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

type Sock =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

const TENANT: &str = "carl";
const PAGE: &str = "markdown/instances/order/4500123.md";
const ROOT: &str = "01HROOT";
const RUN: &str = "01HRUN";

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(FixtureBuilder::new().tenant(TENANT).done()),
        ..Default::default()
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

/// Open a presence-only socket and send an `event_subscribe` with `extra`
/// merged into the frame; returns the socket and the first answer frame.
async fn subscribe(p: &EscurelProcess, bearer: &str, extra: Value) -> (Sock, Value) {
    let mut req = p.ws_url().into_client_request().unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {bearer}").parse().unwrap());
    let (mut sock, _) = tokio_tungstenite::connect_async(req).await.expect("ws");
    sock.send(Message::Text(
        json!({ "type": "hello", "presence_only": true }).to_string(),
    ))
    .await
    .unwrap();
    let mut frame = json!({ "type": "event_subscribe", "subscription_id": "s1" });
    for (k, v) in extra.as_object().unwrap() {
        frame[k] = v.clone();
    }
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

/// Collect `event` frames until `n` arrived or the stream went quiet.
async fn events(sock: &mut Sock, n: usize) -> Vec<Value> {
    let mut out = Vec::new();
    while out.len() < n {
        match recv(sock, 2).await {
            Some(f) if f["type"] == "event" => out.push(f),
            Some(_) => continue,
            None => break,
        }
    }
    out
}

fn run_event(title: &str, target: Option<&str>) -> Value {
    let mut v = json!({
        "kind": "system", "label_skill": "escurel:run", "title": title, "source": "escurel-runner",
        "provenance": { "runner": { "run_id": RUN, "root_event_id": ROOT, "event_id": ROOT } },
    });
    if let Some(t) = target {
        v["instance_page_id"] = json!(t);
    }
    v
}

#[tokio::test]
async fn a_root_event_id_filter_delivers_only_that_lineage_including_system_events() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);
    let (mut sock, ack) =
        subscribe(&p, &admin, json!({ "filters": { "root_event_id": ROOT } })).await;
    assert_eq!(ack["type"], "event_subscribe_ack", "{ack}");

    call(
        &p,
        &admin,
        "capture_event",
        json!({ "event_id": "01HOTHER", "label_skill": "email", "title": "unrelated" }),
    )
    .await;
    call(
        &p,
        &admin,
        "capture_event",
        json!({ "event_id": ROOT, "label_skill": "meeting", "title": "root" }),
    )
    .await;
    call(
        &p,
        &admin,
        "capture_event",
        run_event("run-started", Some(PAGE)),
    )
    .await;

    let got = events(&mut sock, 2).await;
    let ids: Vec<&str> = got
        .iter()
        .map(|f| f["event"]["event_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.len(), 2, "{got:?}");
    assert_eq!(ids[0], ROOT);
    assert_eq!(got[1]["event"]["title"], "run-started");
    assert_eq!(
        got[1]["event"]["kind"], "system",
        "system events flow to a lineage subscriber"
    );
    assert!(
        recv(&mut sock, 1).await.is_none(),
        "the unrelated event never arrives"
    );
}

#[tokio::test]
async fn a_kind_filter_hides_user_events_from_a_system_only_subscriber() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);
    let (mut sock, ack) = subscribe(&p, &admin, json!({ "filters": { "kind": "system" } })).await;
    assert_eq!(ack["type"], "event_subscribe_ack", "{ack}");
    call(
        &p,
        &admin,
        "capture_event",
        json!({ "event_id": ROOT, "label_skill": "meeting", "title": "root" }),
    )
    .await;
    call(
        &p,
        &admin,
        "capture_event",
        run_event("runner-status", None),
    )
    .await;
    let got = events(&mut sock, 1).await;
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0]["event"]["title"], "runner-status");
    assert!(recv(&mut sock, 1).await.is_none());
}

#[tokio::test]
async fn since_event_id_with_a_root_filter_replays_processed_system_events_the_inbox_replay_would_miss()
 {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);
    // Captured BEFORE the subscriber connects: the root (still inbox) and a
    // run event stored `processed` on its page — which the inbox replay
    // could never return.
    call(
        &p,
        &admin,
        "capture_event",
        json!({ "event_id": ROOT, "label_skill": "meeting", "title": "root" }),
    )
    .await;
    call(
        &p,
        &admin,
        "capture_event",
        run_event("run-started", Some(PAGE)),
    )
    .await;
    call(
        &p,
        &admin,
        "capture_event",
        json!({ "event_id": "01HOTHER", "label_skill": "email", "title": "unrelated" }),
    )
    .await;

    let (mut sock, ack) = subscribe(
        &p,
        &admin,
        json!({ "filters": { "root_event_id": ROOT }, "since_event_id": "0" }),
    )
    .await;
    assert_eq!(ack["type"], "event_subscribe_ack", "{ack}");
    let got = events(&mut sock, 2).await;
    assert_eq!(got.len(), 2, "{got:?}");
    for f in &got {
        assert_eq!(f["replayed"], true, "{f}");
        assert_eq!(f["event"]["root_event_id"], ROOT);
    }
    assert!(
        got.iter()
            .any(|f| f["event"]["title"] == "run-started" && f["event"]["status"] == "processed"),
        "{got:?}"
    );
    assert!(
        recv(&mut sock, 1).await.is_none(),
        "the unrelated inbox event is not replayed"
    );
}

/// Codex triage (P1): run lifecycle ids are deterministic strings, not
/// ULIDs — `run:X:attempt:1` and `run:X:finished` sort BELOW
/// `run:X:started`. A lineage resume that compared ids would skip the
/// terminal for ever. It resumes by log POSITION: everything after the
/// row named by `since_event_id`, and everything when that row is unknown.
#[tokio::test]
async fn since_event_id_resumes_a_lineage_by_log_position_not_id_order() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);
    call(&p, &admin, "capture_event", json!({ "event_id": ROOT, "label_skill": "meeting", "title": "root", "at": "2026-09-22T09:00:00Z" })).await;
    for (title, at) in [
        ("run-started", "2026-09-22T09:01:00Z"),
        ("run-attempt", "2026-09-22T09:02:00Z"),
        ("run-finished", "2026-09-22T09:03:00Z"),
    ] {
        let mut e = run_event(title, Some(PAGE));
        e["event_id"] = json!(format!("run:{RUN}:{}", title.trim_start_matches("run-")));
        e["at"] = json!(at);
        call(&p, &admin, "capture_event", e).await;
    }
    let (mut sock, ack) = subscribe(
        &p,
        &admin,
        json!({ "filters": { "root_event_id": ROOT }, "since_event_id": format!("run:{RUN}:started") }),
    )
    .await;
    assert_eq!(ack["type"], "event_subscribe_ack", "{ack}");
    let got = events(&mut sock, 2).await;
    let titles: Vec<&str> = got
        .iter()
        .map(|f| f["event"]["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, ["run-attempt", "run-finished"], "{got:?}");
    assert!(
        recv(&mut sock, 1).await.is_none(),
        "nothing before the position is replayed"
    );
}

/// Codex triage (P2): the protocol says a second `event_subscribe` replaces
/// the first and a malformed one subscribes nothing — so a malformed
/// REPLACEMENT must leave no subscription behind, not the broader old one.
#[tokio::test]
async fn an_invalid_resubscribe_clears_the_previous_subscription() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);
    let (mut sock, ack) = subscribe(&p, &admin, json!({})).await;
    assert_eq!(ack["type"], "event_subscribe_ack", "{ack}");
    call(
        &p,
        &admin,
        "capture_event",
        json!({ "label_skill": "meeting", "title": "one" }),
    )
    .await;
    assert_eq!(
        events(&mut sock, 1).await.len(),
        1,
        "the first subscription is live"
    );

    sock.send(Message::Text(
        json!({ "type": "event_subscribe", "subscription_id": "s2", "filters": { "kind": "robot" } }).to_string(),
    ))
    .await
    .unwrap();
    let err = recv(&mut sock, 3).await.expect("an error frame");
    assert_eq!(err["code"], "invalid_subscription", "{err}");
    call(
        &p,
        &admin,
        "capture_event",
        json!({ "label_skill": "meeting", "title": "two" }),
    )
    .await;
    assert!(
        recv(&mut sock, 1).await.is_none(),
        "the old subscription must be gone"
    );
}

#[tokio::test]
async fn an_invalid_filter_is_refused_with_invalid_subscription() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);
    let (mut sock, first) = subscribe(&p, &admin, json!({ "filters": { "kind": "robot" } })).await;
    assert_eq!(first["type"], "error", "{first}");
    assert_eq!(first["code"], "invalid_subscription");
    assert_eq!(first["subscription_id"], "s1");
    // Nothing is subscribed: a capture produces no frame.
    call(
        &p,
        &admin,
        "capture_event",
        json!({ "label_skill": "meeting", "title": "root" }),
    )
    .await;
    assert!(recv(&mut sock, 1).await.is_none());
}

#[tokio::test]
async fn report_progress_reaches_a_run_filtered_subscriber_within_a_second() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);
    let agent = p.mint_token_for_run(TENANT, Role::Agent, "agent:note", RUN, ROOT);
    let (mut sock, ack) = subscribe(&p, &admin, json!({ "filters": { "run_id": RUN } })).await;
    assert_eq!(ack["type"], "event_subscribe_ack", "{ack}");

    let started = std::time::Instant::now();
    call(
        &p,
        &agent,
        "report_progress",
        json!({ "plan": [{ "step": "read", "status": "completed" }, { "step": "write", "status": "in_progress" }], "current": "write" }),
    )
    .await;
    let frame = recv(&mut sock, 3).await.expect("a frame");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(frame["type"], "event", "{frame}");
    assert_eq!(frame["event"]["title"], "run-progress");
    assert_eq!(frame["event"]["run_id"], RUN);
    let body: Value = serde_json::from_str(frame["event"]["body"].as_str().unwrap()).unwrap();
    assert_eq!(body["current"], "write");
}
