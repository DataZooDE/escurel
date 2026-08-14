//! Reject an empty idempotency key (issue #390).
//!
//! `event_id` is the dedup key, so `""` made EVERY id-less capture the
//! same event: first writer wins, every later one is silently
//! discarded with a success receipt naming the stored (empty) id. The
//! store-level invariant is now: an empty or whitespace-only
//! `event_id` is refused on BOTH intake doors (MCP `capture_event` and
//! REST `/ingest*`); an ABSENT key still mints a server ULID.

use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "stuttgart-ai";

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides::default(),
        fixtures: Some(FixtureBuilder::new().tenant(TENANT).done()),
    })
    .await
}

async fn capture(p: &EscurelProcess, token: &str, args: Value) -> Value {
    reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "capture_event", "arguments": args },
        }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json")
}

#[tokio::test]
async fn empty_event_id_is_refused_not_collapsed() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);

    for bad in ["", "   "] {
        let out = capture(
            &p,
            &token,
            json!({ "event_id": bad, "source": "mail", "label_skill": "note", "body": "msg" }),
        )
        .await;
        assert_eq!(
            out["error"]["code"],
            json!(-32602),
            "event_id {bad:?} must be refused, not become THE shared key: {out}"
        );
    }

    // An ABSENT key still mints — two id-less captures are two events.
    for body in ["first message", "second message"] {
        let out = capture(
            &p,
            &token,
            json!({ "source": "mail", "label_skill": "note", "body": body }),
        )
        .await;
        assert!(out.get("error").is_none(), "absent key mints: {out}");
        assert!(
            out["result"]["structuredContent"]["event_id"]
                .as_str()
                .is_some_and(|id| !id.is_empty()),
            "minted id is non-empty: {out}"
        );
    }
    let listed: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "list_inbox", "arguments": {} },
        }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json");
    let events = listed["result"]["structuredContent"]["events"]
        .as_array()
        .unwrap_or_else(|| panic!("{listed}"));
    assert_eq!(events.len(), 2, "both id-less captures survived: {listed}");
}

#[tokio::test]
async fn rest_ingest_refuses_the_empty_key_too() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);
    let resp = reqwest::Client::new()
        .post(format!("{}/ingest/upload", p.base_url()))
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "content_type": "text/plain",
            "bytes_b64": "aGVsbG8=",
            "event_id": "  ",
        }))
        .send()
        .await
        .expect("post");
    assert_eq!(
        resp.status(),
        422,
        "the REST door enforces the same invariant: {}",
        resp.text().await.unwrap_or_default()
    );
}
