//! Cursor pagination on the event growth surfaces, over real HTTP.
//!
//! The 2026-08-14 API review (R1): `list_events` is oldest-first with a
//! bounded `limit` and had **no cursor at all** — an instance whose
//! history passed the limit had a permanently unreachable tail, and
//! because the ACL filter runs after the limit, a short page never
//! meant "done". These tests pin the `list_messages` idiom on
//! `list_inbox` + `list_events`: an opaque `next_cursor` whose ABSENCE
//! (and nothing else) means the listing is complete.

use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};
use std::collections::BTreeSet;

const TENANT: &str = "stuttgart-ai";
const NOTE_SKILL: &str = "---\ntype: skill\nid: note\ndescription: A note.\n\
    visibility: public\n---\n# note\n";
const TARGET: &str = "---\ntype: instance\nskill: note\nid: log\n---\n# Log\n";
const TARGET_PAGE: &str = "markdown/instances/note/log.md";

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides::default(),
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("note", NOTE_SKILL)
                .instance("note", "log", TARGET)
                .done(),
        ),
    })
    .await
}

async fn mcp(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    let resp = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": args },
        }))
        .send()
        .await
        .expect("post");
    assert_eq!(resp.status(), 200, "http status");
    resp.json().await.unwrap()
}

fn structured(v: &Value) -> &Value {
    &v["result"]["structuredContent"]
}

/// Capture `n` events; half carry an explicit `at`, half are untimed
/// (`at_ts IS NULL`) — the NULLS LAST branch must paginate too.
async fn capture_n(p: &EscurelProcess, token: &str, n: usize) {
    for i in 0..n {
        let mut args = json!({
            "event_id": format!("evt-{i:03}"),
            "source": "test",
            "label_skill": "note",
            "title": format!("event {i}"),
        });
        if i % 2 == 0 {
            args["at"] = json!(format!("2026-08-01T10:{:02}:00Z", i % 60));
        }
        let r = mcp(p, token, "capture_event", args).await;
        assert!(r.get("error").is_none(), "capture {i}: {r}");
    }
}

/// Walk a paginated list tool until `next_cursor` disappears; return
/// the distinct event ids seen and assert per-page invariants.
async fn drain(p: &EscurelProcess, token: &str, tool: &str, mut base: Value) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut cursor: Option<String> = None;
    for page_no in 0..20 {
        let mut args = base.clone();
        if let Some(c) = &cursor {
            args["cursor"] = json!(c);
        }
        let out = mcp(p, token, tool, args).await;
        let s = structured(&out);
        let events = s["events"]
            .as_array()
            .unwrap_or_else(|| panic!("{tool} page {page_no} shape: {out}"));
        for e in events {
            let id = e["event_id"].as_str().expect("event_id").to_owned();
            assert!(
                seen.insert(id.clone()),
                "{tool} page {page_no} repeated `{id}` — cursor must not replay"
            );
        }
        match s.get("next_cursor").and_then(Value::as_str) {
            Some(c) => cursor = Some(c.to_owned()),
            None => return seen,
        }
    }
    base.take();
    panic!("{tool} never terminated — next_cursor kept coming");
}

/// The inbox tail past `limit` must be reachable via the cursor.
#[tokio::test]
async fn list_inbox_pages_past_the_limit() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);
    capture_n(&p, &token, 25).await;

    // A single limited call sees 10 — the old behaviour kept the other
    // 15 permanently unreachable.
    let first = mcp(&p, &token, "list_inbox", json!({ "limit": 10 })).await;
    let events = structured(&first)["events"].as_array().unwrap().clone();
    assert_eq!(events.len(), 10, "page size respected: {first}");
    assert!(
        structured(&first)["next_cursor"].is_string(),
        "a full page with a tail must carry next_cursor: {first}"
    );

    let seen = drain(&p, &token, "list_inbox", json!({ "limit": 10 })).await;
    assert_eq!(seen.len(), 25, "every inbox event reachable: {seen:?}");
}

/// An instance's processed history past `limit` must be reachable too —
/// this was the "unreachable tail forever" case.
#[tokio::test]
async fn list_events_pages_past_the_limit() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);
    capture_n(&p, &token, 25).await;
    for i in 0..25 {
        let r = mcp(
            &p,
            &token,
            "assign_event",
            json!({ "event_id": format!("evt-{i:03}"), "instance_page_id": TARGET_PAGE }),
        )
        .await;
        assert!(r.get("error").is_none(), "assign {i}: {r}");
    }

    let seen = drain(
        &p,
        &token,
        "list_events",
        json!({ "instance_page_id": TARGET_PAGE, "limit": 10 }),
    )
    .await;
    assert_eq!(seen.len(), 25, "every processed event reachable: {seen:?}");
}

/// Garbage cursors are a typed refusal, not a silent full restart.
#[tokio::test]
async fn invalid_cursor_is_invalid_params() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);
    let out = mcp(
        &p,
        &token,
        "list_inbox",
        json!({ "cursor": "not!base64!at!all" }),
    )
    .await;
    assert_eq!(
        out["error"]["code"],
        json!(-32602),
        "an undecodable cursor must be invalid_params: {out}"
    );
}
