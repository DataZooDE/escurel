//! Machine-readable JSON-RPC error `data` over real HTTP.
//!
//! The 2026-08-14 API review (R3): the error taxonomy forced clients to
//! string-match English messages — `-32000` was overloaded three ways
//! distinguished only by a message prefix, `unknown_session` was
//! indistinguishable from a genuine server fault, and `assign_event`'s
//! "already assigned" vs "not found" required parsing prose. The spec
//! (`protocol.md` §Errors) promised a `retryable` flag the wire never
//! carried. These tests pin the contract: refusals carry
//! `error.data: { code, retryable }`, additively — `code`/`message`
//! are unchanged, old clients ignore `data`.

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

async fn call_err(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    let body: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": args },
        }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("decode");
    assert!(
        body.get("error").is_some(),
        "{name} was expected to refuse: {body}"
    );
    body["error"].clone()
}

fn assert_data(err: &Value, code: &str, retryable: bool) {
    assert_eq!(
        err["data"]["code"],
        json!(code),
        "data.code must be `{code}`: {err}"
    );
    assert_eq!(
        err["data"]["retryable"],
        json!(retryable),
        "data.retryable must be {retryable}: {err}"
    );
}

/// `-32001` admin-required refusals name themselves.
#[tokio::test]
async fn admin_required_carries_data_code() {
    let p = start().await;
    let agent = p.mint_token(TENANT, Role::Agent);
    let err = call_err(&p, &agent, "tenant_list", json!({})).await;
    assert_eq!(err["code"], json!(-32001), "{err}");
    assert_data(&err, "admin_required", false);
}

/// A dead session is the CALLER's state problem (reopen), not a server
/// fault — it must be distinguishable from `-32603 internal` without
/// parsing English.
#[tokio::test]
async fn unknown_session_carries_data_code() {
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            live_crdt: true,
            ..Default::default()
        },
        fixtures: Some(FixtureBuilder::new().tenant(TENANT).done()),
    })
    .await;
    let agent = p.mint_token(TENANT, Role::Agent);
    let err = call_err(
        &p,
        &agent,
        "apply_op",
        json!({ "session": "sess-never-opened", "op": "AAAA" }),
    )
    .await;
    assert_data(&err, "unknown_session", false);
}

/// `assign_event`'s two caller errors are different actions for the
/// caller — "not found" (give up / re-capture) vs "already assigned"
/// (another worker won the claim) — and both used to require parsing
/// the message. An ACL-hidden event deliberately reports
/// `event_not_found` too (no existence oracle).
#[tokio::test]
async fn assign_event_data_codes_distinguish_the_outcomes() {
    let p = start().await;
    let agent = p.mint_token(TENANT, Role::Agent);

    let not_found = call_err(
        &p,
        &agent,
        "assign_event",
        json!({ "event_id": "evt-missing", "instance_page_id": "markdown/instances/x/y.md" }),
    )
    .await;
    assert_eq!(not_found["code"], json!(-32602), "{not_found}");
    assert_data(&not_found, "event_not_found", false);

    // Capture, claim, then a SECOND claim for a different target.
    let ok = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {agent}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "capture_event",
                "arguments": { "event_id": "evt-1", "source": "t", "label_skill": "note" } },
        }))
        .send()
        .await
        .expect("post");
    assert_eq!(ok.status(), 200);
    for (target, expect_err) in [
        ("markdown/instances/note/a.md", false),
        ("markdown/instances/note/b.md", true),
    ] {
        let body: Value = reqwest::Client::new()
            .post(p.mcp_url())
            .header("authorization", format!("Bearer {agent}"))
            .json(&json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": { "name": "assign_event",
                    "arguments": { "event_id": "evt-1", "instance_page_id": target } },
            }))
            .send()
            .await
            .expect("post")
            .json()
            .await
            .expect("decode");
        if expect_err {
            assert_data(&body["error"], "already_assigned", false);
        } else {
            assert!(body.get("error").is_none(), "first claim wins: {body}");
        }
    }
}

/// A reader replica's "retry against the writer" is the one refusal
/// that IS retryable — elsewhere.
#[tokio::test]
async fn read_only_replica_is_marked_retryable() {
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            reader_mode: true,
            ..Default::default()
        },
        fixtures: Some(FixtureBuilder::new().tenant(TENANT).done()),
    })
    .await;
    let agent = p.mint_token(TENANT, Role::Agent);
    let err = call_err(
        &p,
        &agent,
        "update_page",
        json!({ "page_id": "markdown/instances/x/y.md", "content": "x" }),
    )
    .await;
    assert_eq!(err["code"], json!(-32004), "{err}");
    assert_data(&err, "read_only_replica", true);
}
