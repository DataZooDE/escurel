//! Cursor pagination on `list_instances` — the last list surface whose
//! `next_cursor` was a hardcoded `null` (2026-08-14 API review R1;
//! `protocol.md` said "reserved; always null today").
//!
//! Contract matches the event surfaces: opaque `cursor` in,
//! `next_cursor` out while rows remain. For byte-compat with clients
//! written against the always-null era, `next_cursor` stays PRESENT
//! as `null` on the final page (unlike the event listings, which omit
//! it) — the field's meaning is unchanged, it just finally loads.

use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};
use std::collections::BTreeSet;

const TENANT: &str = "stuttgart-ai";
const NOTE_SKILL: &str = "---\ntype: skill\nid: note\ndescription: A note.\n\
    visibility: public\n---\n# note\n";

async fn start() -> EscurelProcess {
    let mut fx = FixtureBuilder::new()
        .tenant(TENANT)
        .skill("note", NOTE_SKILL);
    for i in 0..25 {
        // Half timed (an `at:` timeline), half untimed — the NULLS LAST
        // block must paginate under `order_by` too.
        let body = if i % 2 == 0 {
            format!(
                "---\ntype: instance\nskill: note\nid: n{i:02}\nat: \"2026-08-01T10:{:02}:00Z\"\n---\n# n{i:02}\n",
                i % 60
            )
        } else {
            format!("---\ntype: instance\nskill: note\nid: n{i:02}\n---\n# n{i:02}\n")
        };
        fx = fx.instance("note", &format!("n{i:02}"), body);
    }
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides::default(),
        fixtures: Some(fx.done()),
    })
    .await
}

async fn call(p: &EscurelProcess, token: &str, args: Value) -> Value {
    reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "list_instances", "arguments": args },
        }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("decode")
}

/// Drain all pages under the given base args; assert no replay.
async fn drain(p: &EscurelProcess, token: &str, base: Value) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut cursor: Option<String> = None;
    for page_no in 0..20 {
        let mut args = base.clone();
        if let Some(c) = &cursor {
            args["cursor"] = json!(c);
        }
        let out = call(p, token, args).await;
        let s = &out["result"]["structuredContent"];
        let instances = s["instances"]
            .as_array()
            .unwrap_or_else(|| panic!("page {page_no} shape: {out}"));
        for i in instances {
            let id = i["page_id"].as_str().expect("page_id").to_owned();
            assert!(seen.insert(id.clone()), "page {page_no} replayed `{id}`");
        }
        match s["next_cursor"].as_str() {
            Some(c) => cursor = Some(c.to_owned()),
            None => return seen,
        }
    }
    panic!("pagination never terminated");
}

#[tokio::test]
async fn default_ordering_pages_past_the_limit() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);

    let first = call(&p, &token, json!({ "skill_id": "note", "limit": 10 })).await;
    let s = &first["result"]["structuredContent"];
    assert_eq!(s["instances"].as_array().unwrap().len(), 10, "{first}");
    assert!(
        s["next_cursor"].is_string(),
        "a full page with a tail must carry a real next_cursor — the \
         always-null era is over: {first}"
    );

    let seen = drain(&p, &token, json!({ "skill_id": "note", "limit": 10 })).await;
    assert_eq!(seen.len(), 25, "every instance reachable: {seen:?}");
}

#[tokio::test]
async fn at_desc_ordering_pages_past_the_limit() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);
    let seen = drain(
        &p,
        &token,
        json!({ "skill_id": "note", "order_by": "at desc", "limit": 7 }),
    )
    .await;
    assert_eq!(
        seen.len(),
        25,
        "timed AND untimed instances reachable under order_by: {seen:?}"
    );
}

#[tokio::test]
async fn invalid_cursor_is_invalid_params() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);
    let out = call(
        &p,
        &token,
        json!({ "skill_id": "note", "cursor": "!!definitely-not-base64!!" }),
    )
    .await;
    assert_eq!(out["error"]["code"], json!(-32602), "{out}");
}
