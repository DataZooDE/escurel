//! `list_messages` cursor errors are the CALLER's fault, over real HTTP.
//!
//! The bug this pins: `tool_list_messages` mapped every
//! `list_chat_messages` error to `internal` (-32603) — including
//! `IndexerError::InvalidCursor` from a garbage `cursor` argument. The
//! event surfaces already route the same failure through
//! `cursor_aware_error` and answer `invalid_params` (-32602); chat must
//! agree.

use escurel_server::WriteAclMode;
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "stuttgart-ai";

const NOTE_SKILL: &str = "---\ntype: skill\nid: note\ndescription: A note.\n\
    visibility: public\n---\n# note\n";

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            write_acl: Some(WriteAclMode::Off),
            ..Default::default()
        },
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("note", NOTE_SKILL)
                .done(),
        ),
    })
    .await
}

async fn call(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
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

/// A garbage cursor is a typed refusal (`invalid_params`), never an
/// internal server error — parity with `list_inbox`/`list_events`.
#[tokio::test]
async fn list_messages_garbage_cursor_is_invalid_params() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);
    let out = call(
        &p,
        &token,
        "list_messages",
        json!({ "chat_group_id": "alice", "cursor": "not!base64!at!all" }),
    )
    .await;
    assert_eq!(
        out["error"]["code"],
        json!(-32602),
        "an undecodable list_messages cursor must be invalid_params, \
         not internal: {out}"
    );
}
