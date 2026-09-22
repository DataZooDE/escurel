//! `list_events { newest_first }` and runner-status retention (knowledge-
//! workbench backend P2-4).
//!
//! Every `list_events` selector pages oldest first — the tail shape the
//! runner's subscribers read. The workbench asks the opposite question of
//! `escurel:runner-status` ("what is the runner's CURRENT state?"), so a
//! listing may be turned around with `newest_first: true` and `limit: 1`
//! is then the latest row. And because a heartbeat every N seconds is
//! unbounded, the gateway keeps only the last 50 `escurel:runner-status`
//! rows per tenant, pruned at capture like `run-progress` per run.
//!
//! Real gateway, real DuckDB, raw JSON-RPC.

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "carl";

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(FixtureBuilder::new().tenant(TENANT).done()),
        ..Default::default()
    })
    .await
}

async fn call(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    let body: Value = reqwest::Client::new()
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
    assert!(body.get("error").is_none(), "{name}: {body}");
    body["result"]["structuredContent"].clone()
}

fn titles(page: &Value) -> Vec<String> {
    page["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["title"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn newest_first_turns_a_listing_around_and_limit_one_is_the_latest() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);
    for (i, t) in ["first", "second", "third"].iter().enumerate() {
        call(
            &p,
            &admin,
            "capture_event",
            json!({ "at": format!("2026-09-22T10:00:0{i}Z"), "source": "t", "mime": "text/plain",
                    "label_skill": "note", "title": t, "body": "" }),
        )
        .await;
    }
    let asc = call(&p, &admin, "list_events", json!({ "label_skill": "note" })).await;
    assert_eq!(titles(&asc), ["first", "second", "third"], "{asc}");
    let desc = call(
        &p,
        &admin,
        "list_events",
        json!({ "label_skill": "note", "newest_first": true }),
    )
    .await;
    assert_eq!(titles(&desc), ["third", "second", "first"], "{desc}");
    let latest = call(
        &p,
        &admin,
        "list_events",
        json!({ "label_skill": "note", "newest_first": true, "limit": 1 }),
    )
    .await;
    assert_eq!(titles(&latest), ["third"], "{latest}");
    assert!(
        latest["next_cursor"].is_string(),
        "more lie past the page: {latest}"
    );
    // The cursor keeps walking in the same direction.
    let next = call(
        &p,
        &admin,
        "list_events",
        json!({ "label_skill": "note", "newest_first": true, "limit": 1,
                "cursor": latest["next_cursor"] }),
    )
    .await;
    assert_eq!(titles(&next), ["second"], "{next}");
}

#[tokio::test]
async fn runner_status_keeps_only_the_last_fifty_rows() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);
    for i in 0..55u32 {
        call(
            &p,
            &admin,
            "capture_event",
            json!({ "kind": "system", "source": "escurel-runner", "mime": "application/json",
                    "label_skill": "escurel:runner-status", "title": "heartbeat",
                    "at": format!("2026-09-22T11:{:02}:{:02}Z", i / 60, i % 60),
                    "body": json!({ "seq": i }).to_string() }),
        )
        .await;
    }
    let all = call(
        &p,
        &admin,
        "list_events",
        json!({ "label_skill": "escurel:runner-status", "newest_first": true, "limit": 1000 }),
    )
    .await;
    let rows = all["events"].as_array().unwrap();
    assert_eq!(rows.len(), 50, "pruned at capture: {}", rows.len());
    let newest: Value = serde_json::from_str(rows[0]["body"].as_str().unwrap()).unwrap();
    assert_eq!(newest["seq"], 54, "the latest survives: {newest}");
    let oldest: Value = serde_json::from_str(rows[49]["body"].as_str().unwrap()).unwrap();
    assert_eq!(oldest["seq"], 5, "the first five were pruned: {oldest}");
}

/// The tail contract (hardening H3): a listing by label, root or run is in
/// INGESTION order and resumes by it, so an event captured later with an
/// earlier `at` (a backdated import, a caller's clock) still arrives after
/// the cursor a subscriber holds. Before H3 the cursor was `(at, event_id)`
/// and such an event sorted before it, never to be seen.
#[tokio::test]
async fn a_label_tail_never_loses_a_backdated_event() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);
    let cap = |title: &str, at: &str| {
        let admin = admin.clone();
        let title = title.to_owned();
        let at = at.to_owned();
        let p = &p;
        async move {
            call(
                p,
                &admin,
                "capture_event",
                json!({ "at": at, "source": "t", "mime": "text/plain",
                    "label_skill": "note", "title": title, "body": "" }),
            )
            .await
        }
    };
    cap("first", "2026-09-22T10:00:05Z").await;
    let page = call(&p, &admin, "list_events", json!({ "label_skill": "note" })).await;
    assert_eq!(titles(&page), ["first"]);
    let cursor = page["resume_cursor"]
        .as_str()
        .expect("resume cursor")
        .to_owned();
    // Captured AFTER the subscriber's poll, dated BEFORE the first event.
    cap("backdated", "2026-09-22T10:00:01Z").await;
    let next = call(
        &p,
        &admin,
        "list_events",
        json!({ "label_skill": "note", "cursor": cursor }),
    )
    .await;
    assert_eq!(titles(&next), ["backdated"], "the tail must see it: {next}");
    // And a full listing is ingestion order, not `at` order.
    let all = call(&p, &admin, "list_events", json!({ "label_skill": "note" })).await;
    assert_eq!(titles(&all), ["first", "backdated"], "{all}");
    // A page's own history stays chronological by `at`.
    let hist = call(
        &p,
        &admin,
        "list_events",
        json!({ "root_event_id": all["events"][0]["event_id"] }),
    )
    .await;
    assert!(!hist["events"].as_array().unwrap().is_empty(), "{hist}");
}
