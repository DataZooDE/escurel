//! Chat-surface ACL over real HTTP: a chat group (ADR-13: `chat_group_id`
//! := `community_member_id`) is private to its owning member. Only the owner
//! (or admin) may append to or read a chat's history; a non-owner read
//! returns an EMPTY page (non-leaking) and a non-owner append is forbidden.
//! Gated by `ESCUREL_WRITE_ACL` (off = legacy open chat). No LLM in the path.

use escurel_server::WriteAclMode;
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "stuttgart-ai";
const ALICE: &str = "whatsapp:111";
const BOB: &str = "whatsapp:222";

const MEMBER_SKILL: &str = "---\ntype: skill\nid: community_member\n\
    description: A member.\nvisibility: owner\nowner_field: credential\n---\n# community_member\n";
const ALICE_MEMBER: &str = "---\ntype: instance\nskill: community_member\nid: alice\n\
    credential: \"whatsapp:111\"\n---\n# Alice\n";
const BOB_MEMBER: &str = "---\ntype: instance\nskill: community_member\nid: bob\n\
    credential: \"whatsapp:222\"\n---\n# Bob\n";

// Chat group id == community_member id (ADR-13).
const ALICE_CHAT: &str = "alice";

async fn start(mode: WriteAclMode) -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            write_acl: Some(mode),
            ..Default::default()
        },
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("community_member", MEMBER_SKILL)
                .instance("community_member", "alice", ALICE_MEMBER)
                .instance("community_member", "bob", BOB_MEMBER)
                .done(),
        ),
    })
    .await
}

/// Raw JSON-RPC call → the whole response body (so callers can inspect
/// `result` vs `error`).
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

fn append_args(chat: &str, content: &str) -> Value {
    json!({ "chat_group_id": chat, "role": "user", "content": content, "embed": false })
}

#[tokio::test]
async fn owner_appends_and_reads_non_owner_blocked() {
    let p = start(WriteAclMode::Enforce).await;
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let bob = p.mint_token_with_sub(TENANT, Role::Agent, BOB);
    let admin = p.mint_token(TENANT, Role::Admin);

    // Owner appends to her own chat → ok.
    let a = call(
        &p,
        &alice,
        "append_message",
        append_args(ALICE_CHAT, "geheim"),
    )
    .await;
    assert!(
        a.get("error").is_none(),
        "alice may append to her own chat: {a}"
    );

    // Non-owner append → forbidden error.
    let b = call(&p, &bob, "append_message", append_args(ALICE_CHAT, "spy")).await;
    assert!(
        b["error"]["code"] == json!(-32003),
        "bob's append to alice's chat must be forbidden: {b}"
    );

    // Non-owner read → empty page (non-leaking), never alice's message.
    let bread = call(
        &p,
        &bob,
        "list_messages",
        json!({ "chat_group_id": ALICE_CHAT }),
    )
    .await;
    let msgs = &bread["result"]["structuredContent"]["messages"];
    assert_eq!(msgs, &json!([]), "bob must see an empty page, got: {bread}");

    // Owner reads her own → sees the message.
    let aread = call(
        &p,
        &alice,
        "list_messages",
        json!({ "chat_group_id": ALICE_CHAT }),
    )
    .await;
    let amsgs = aread["result"]["structuredContent"]["messages"]
        .as_array()
        .unwrap();
    assert_eq!(amsgs.len(), 1, "alice reads her own chat: {aread}");

    // Admin reads any chat.
    let adread = call(
        &p,
        &admin,
        "list_messages",
        json!({ "chat_group_id": ALICE_CHAT }),
    )
    .await;
    let admsgs = adread["result"]["structuredContent"]["messages"]
        .as_array()
        .unwrap();
    assert_eq!(admsgs.len(), 1, "admin reads any chat: {adread}");
}

#[tokio::test]
async fn off_mode_leaves_chat_open() {
    let p = start(WriteAclMode::Off).await;
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let bob = p.mint_token_with_sub(TENANT, Role::Agent, BOB);

    let a = call(
        &p,
        &alice,
        "append_message",
        append_args(ALICE_CHAT, "geheim"),
    )
    .await;
    assert!(a.get("error").is_none(), "{a}");
    // With the flag off, bob can still read alice's chat (legacy behaviour).
    let bread = call(
        &p,
        &bob,
        "list_messages",
        json!({ "chat_group_id": ALICE_CHAT }),
    )
    .await;
    let msgs = bread["result"]["structuredContent"]["messages"]
        .as_array()
        .unwrap();
    assert_eq!(msgs.len(), 1, "off mode keeps chat open: {bread}");
}

#[tokio::test]
async fn unknown_chat_group_is_ungated() {
    // A chat_group_id with no owning community_member is not private — any
    // authenticated caller may use it (compat for non-member chat groups).
    let p = start(WriteAclMode::Enforce).await;
    let bob = p.mint_token_with_sub(TENANT, Role::Agent, BOB);
    let a = call(
        &p,
        &bob,
        "append_message",
        append_args("ops-broadcast", "hi"),
    )
    .await;
    assert!(
        a.get("error").is_none(),
        "unknown chat group is ungated: {a}"
    );
}
