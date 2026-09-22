//! Draft and changeset transitions are published as `escurel:review` system
//! events (knowledge-workbench backend, P1 PR8 — BRD FR-W-2). Real gateway,
//! real DuckDB, raw JSON-RPC and a real WebSocket subscriber.
//!
//! A held write changing state is a fact the bus must carry: the review
//! queue and the lineage tree update live from it. Each transition is one
//! `kind: system` event attached to the draft's target page, carrying the
//! draft's run lineage (from the draft ROW, never the caller) and who
//! decided, under `provenance.review`. Best-effort — a transition never
//! fails because the bus could not be told.

use std::time::Duration;

use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

const TENANT: &str = "stuttgart-ai";
const NOTE_SKILL: &str = "---\ntype: skill\nid: note\ndescription: A note.\n\
    visibility: public\n---\n# note\n";
const BASE: &str = "---\ntype: instance\nskill: note\nid: plan\n---\n# Plan\nv1 body.\n";
const PAGE: &str = "markdown/instances/note/plan.md";
const RUN: &str = "01HRUNXXXXXXXXXXXXXXXXXXXX";
const ROOT: &str = "01HROOTXXXXXXXXXXXXXXXXXXX";

fn sha(s: &str) -> String {
    format!("{:x}", Sha256::digest(s.as_bytes()))
}

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides::default(),
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("note", NOTE_SKILL)
                .instance("note", "plan", BASE)
                .done(),
        ),
    })
    .await
}

async fn call(p: &EscurelProcess, token: &str, tool: &str, args: Value) -> Value {
    let body: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": tool, "arguments": args },
        }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json");
    assert!(body.get("error").is_none(), "{tool} error: {body}");
    body["result"]["structuredContent"].clone()
}

fn draft_args(text: &str) -> Value {
    json!({
        "target_page_id": PAGE,
        "content": format!("---\ntype: instance\nskill: note\nid: plan\n---\n# Plan\n{text}\n"),
        "base_sha256": sha(BASE),
    })
}

/// The `escurel:review` events on the page, oldest first, as the admin.
async fn review_events(p: &EscurelProcess) -> Vec<Value> {
    let admin = p.mint_token(TENANT, Role::Admin);
    let r = call(
        p,
        &admin,
        "list_events",
        json!({ "instance_page_id": PAGE, "include_system": true, "kind": "system" }),
    )
    .await;
    r["events"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|e| e["label_skill"] == "escurel:review")
        .collect()
}

fn titles(events: &[Value]) -> Vec<&str> {
    events
        .iter()
        .map(|e| e["title"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn creating_promoting_and_discarding_a_draft_publish_review_system_events_on_the_target_page()
{
    let p = start().await;
    let agent = p.mint_token_for_run(TENANT, Role::Agent, "agent:note", RUN, ROOT);
    let human = p.mint_token_with_sub(TENANT, Role::Agent, "alice");

    let r = call(&p, &agent, "create_draft", draft_args("v2 from a run.")).await;
    assert_eq!(r["ok"], true, "{r}");
    let d1 = r["draft"]["draft_id"].as_str().unwrap().to_owned();
    let r = call(&p, &human, "promote_draft", json!({ "draft_id": d1 })).await;
    assert_eq!(r["ok"], true, "{r}");

    let events = review_events(&p).await;
    assert_eq!(
        titles(&events),
        ["draft-created", "draft-promoted"],
        "{events:?}"
    );
    for e in &events {
        assert_eq!(e["kind"], "system");
        assert_eq!(e["status"], "processed", "attached, never inbox work: {e}");
        assert_eq!(e["instance_page_id"], PAGE);
        assert_eq!(e["run_id"], RUN, "lineage from the draft row: {e}");
        assert_eq!(e["root_event_id"], ROOT);
        assert_eq!(e["provenance"]["review"]["draft_id"], d1, "{e}");
        assert_eq!(e["provenance"]["review"]["run_id"], RUN);
        assert_eq!(e["provenance"]["review"]["root_event_id"], ROOT);
    }
    assert_eq!(
        events[0]["provenance"]["review"]["decided_by"], "agent:note",
        "{}",
        events[0]
    );
    assert_eq!(
        events[1]["provenance"]["review"]["decided_by"], "alice",
        "{}",
        events[1]
    );
    assert_eq!(events[1]["provenance"]["review"]["already_decided"], false);

    // A second draft against the now-moved page, discarded with a reason.
    let head = call(&p, &human, "expand", json!({ "page_id": PAGE })).await;
    let mut args = draft_args("v3, refused.");
    args["base_sha256"] = head["content_sha256"].clone();
    let r = call(&p, &agent, "create_draft", args).await;
    assert_eq!(r["ok"], true, "{r}");
    let d2 = r["draft"]["draft_id"].as_str().unwrap().to_owned();
    let r = call(
        &p,
        &human,
        "discard_draft",
        json!({ "draft_id": d2, "reason": "not this quarter" }),
    )
    .await;
    assert_eq!(r["ok"], true, "{r}");
    let events = review_events(&p).await;
    assert_eq!(
        titles(&events),
        [
            "draft-created",
            "draft-promoted",
            "draft-created",
            "draft-discarded"
        ],
        "{events:?}"
    );
    let discarded = &events[3];
    assert_eq!(discarded["provenance"]["review"]["draft_id"], d2);
    assert_eq!(discarded["provenance"]["review"]["decided_by"], "alice");
    let body: Value = serde_json::from_str(discarded["body"].as_str().unwrap()).unwrap();
    assert_eq!(body["reason"], "not this quarter", "{body}");
    // And the lineage read sees them all under the root.
    let admin = p.mint_token(TENANT, Role::Admin);
    let tree = call(
        &p,
        &admin,
        "list_events",
        json!({ "root_event_id": ROOT, "include_system": true }),
    )
    .await;
    assert_eq!(tree["events"].as_array().unwrap().len(), 4, "{tree}");
}

/// P2-0: the runner's promotion subscriber needs the draft's TRIGGER event
/// to find the run in its ledger, so every review event names it.
#[tokio::test]
async fn review_events_carry_the_drafts_trigger_event() {
    let p = start().await;
    let agent = p.mint_token_for_run(TENANT, Role::Agent, "agent:note", RUN, ROOT);
    let human = p.mint_token_with_sub(TENANT, Role::Agent, "alice");
    let mut args = draft_args("v2 answering an event.");
    args["event_id"] = json!("01HTRIGGER");
    args["new_changeset"] = json!(true);
    let r = call(&p, &agent, "create_draft", args).await;
    assert_eq!(r["ok"], true, "{r}");
    let cs = r["draft"]["changeset_id"].as_str().unwrap().to_owned();
    call(
        &p,
        &human,
        "promote_changeset",
        json!({ "changeset_id": cs }),
    )
    .await;
    let events = review_events(&p).await;
    assert_eq!(
        titles(&events),
        ["draft-created", "draft-promoted", "changeset-promoted"],
        "{events:?}"
    );
    for e in &events {
        assert_eq!(e["provenance"]["review"]["event_id"], "01HTRIGGER", "{e}");
        let body: Value = serde_json::from_str(e["body"].as_str().unwrap()).unwrap();
        assert_eq!(body["event_id"], "01HTRIGGER");
    }
}

#[tokio::test]
async fn promoting_an_already_decided_changeset_publishes_already_decided() {
    let p = start().await;
    let agent = p.mint_token_for_run(TENANT, Role::Agent, "agent:note", RUN, ROOT);
    let human = p.mint_token_with_sub(TENANT, Role::Agent, "alice");
    let mut args = draft_args("v2 grouped.");
    args["new_changeset"] = json!(true);
    let r = call(&p, &agent, "create_draft", args).await;
    let cs = r["draft"]["changeset_id"].as_str().unwrap().to_owned();

    let r = call(
        &p,
        &human,
        "promote_changeset",
        json!({ "changeset_id": cs }),
    )
    .await;
    assert_eq!(r["ok"], true, "{r}");
    let r = call(
        &p,
        &human,
        "promote_changeset",
        json!({ "changeset_id": cs }),
    )
    .await;
    assert_eq!(r["already_decided"], true, "{r}");

    let events = review_events(&p).await;
    assert_eq!(
        titles(&events),
        [
            "draft-created",
            "draft-promoted",
            "changeset-promoted",
            "changeset-already_decided"
        ],
        "{events:?}"
    );
    for e in &events[2..] {
        assert_eq!(e["provenance"]["review"]["changeset_id"], cs, "{e}");
        assert_eq!(e["run_id"], RUN);
        assert_eq!(e["root_event_id"], ROOT);
    }
    assert_eq!(events[2]["provenance"]["review"]["already_decided"], false);
    assert_eq!(events[3]["provenance"]["review"]["already_decided"], true);
    assert_eq!(events[3]["provenance"]["review"]["decided_by"], "alice");
}

#[tokio::test]
async fn review_events_are_hidden_from_list_inbox_and_visible_with_include_system() {
    let p = start().await;
    let agent = p.mint_token_for_run(TENANT, Role::Agent, "agent:note", RUN, ROOT);
    let admin = p.mint_token(TENANT, Role::Admin);
    let r = call(&p, &agent, "create_draft", draft_args("v2.")).await;
    assert_eq!(r["ok"], true, "{r}");
    let inbox = call(&p, &admin, "list_inbox", json!({})).await;
    assert!(inbox["events"].as_array().unwrap().is_empty(), "{inbox}");
    let hist = call(
        &p,
        &admin,
        "list_events",
        json!({ "instance_page_id": PAGE }),
    )
    .await;
    assert!(
        hist["events"].as_array().unwrap().is_empty(),
        "hidden by default: {hist}"
    );
    let shown = call(
        &p,
        &admin,
        "list_events",
        json!({ "instance_page_id": PAGE, "include_system": true }),
    )
    .await;
    assert_eq!(shown["events"][0]["title"], "draft-created", "{shown}");
}

#[tokio::test]
async fn a_subscriber_sees_the_review_event_when_a_draft_is_promoted() {
    let p = start().await;
    let agent = p.mint_token_for_run(TENANT, Role::Agent, "agent:note", RUN, ROOT);
    let human = p.mint_token_with_sub(TENANT, Role::Agent, "alice");
    let admin = p.mint_token(TENANT, Role::Admin);

    let mut req = p.ws_url().into_client_request().unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {admin}").parse().unwrap());
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
    let ack: Value = match sock.next().await.unwrap().unwrap() {
        Message::Text(t) => serde_json::from_str(&t).unwrap(),
        other => panic!("{other:?}"),
    };
    assert_eq!(ack["type"], "event_subscribe_ack");

    let r = call(&p, &agent, "create_draft", draft_args("v2 live.")).await;
    let d = r["draft"]["draft_id"].as_str().unwrap().to_owned();
    call(&p, &human, "promote_draft", json!({ "draft_id": d })).await;

    let mut seen = Vec::new();
    while seen.len() < 2 {
        match tokio::time::timeout(Duration::from_secs(3), sock.next()).await {
            Ok(Some(Ok(Message::Text(t)))) => {
                let f: Value = serde_json::from_str(&t).unwrap();
                if f["type"] == "event" && f["event"]["label_skill"] == "escurel:review" {
                    seen.push(f["event"]["title"].as_str().unwrap().to_owned());
                }
            }
            other => panic!("no review frame within 3s; got {other:?}; seen {seen:?}"),
        }
    }
    assert_eq!(seen, ["draft-created", "draft-promoted"]);
}
