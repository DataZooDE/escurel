//! `append_message` retry-safety over real HTTP: a caller-supplied
//! `msg_id` must be a real idempotency key, the way `capture_event`'s
//! `event_id` already is.
//!
//! The failure this pins (2026-08-14 API review, R4): when `ts` is
//! omitted the server stamps `CURRENT_TIMESTAMP`, and the PK is
//! `(chat_group_id, ts, msg_id)` — so an offline client retrying the
//! same `msg_id` used to land a SECOND row at a different `ts`, and
//! dedup-on-read was impossible because both rows were "valid".

use escurel_server::WriteAclMode;
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "stuttgart-ai";
const CHAT: &str = "alice";

const MEMBER_SKILL: &str = "---\ntype: skill\nid: community_member\n\
    description: A member.\nvisibility: owner\nowner_field: credential\n---\n# community_member\n";
const ALICE_MEMBER: &str = "---\ntype: instance\nskill: community_member\nid: alice\n\
    credential: \"whatsapp:111\"\n---\n# Alice\n";

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
                .skill("community_member", MEMBER_SKILL)
                .instance("community_member", "alice", ALICE_MEMBER)
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
    let body: Value = resp.json().await.unwrap();
    assert!(body.get("error").is_none(), "{name} error: {body}");
    body["result"]["structuredContent"].clone()
}

#[tokio::test]
async fn append_message_retry_with_same_msg_id_is_one_row() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);

    let args = json!({
        "chat_group_id": CHAT,
        "msg_id": "client-ulid-1",
        "role": "user",
        "content": "Hallo — bitte einmal, nicht zweimal.",
        "embed": false,
    });

    // First delivery.
    let first = call(&p, &token, "append_message", args.clone()).await;
    assert_eq!(first["msg_id"], json!("client-ulid-1"), "{first}");
    let first_ts = first["ts"].as_str().expect("ts").to_owned();

    // The redelivery: same msg_id, ts still omitted (the retrying client
    // doesn't know what the server stamped). Must echo the stored row.
    let second = call(&p, &token, "append_message", args).await;
    assert_eq!(second["msg_id"], json!("client-ulid-1"), "{second}");
    assert_eq!(
        second["ts"].as_str(),
        Some(first_ts.as_str()),
        "retry must echo the originally stamped ts, not mint a new row: {second}"
    );

    // And the transcript holds exactly one copy.
    let page = call(
        &p,
        &token,
        "list_messages",
        json!({ "chat_group_id": CHAT }),
    )
    .await;
    let msgs = page["messages"].as_array().expect("messages: {page}");
    assert_eq!(
        msgs.len(),
        1,
        "one delivery + one retry must be ONE row: {page}"
    );
}
