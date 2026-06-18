//! Admin-managed DuckDB group membership over the MCP boundary (group
//! ACL v1). A running gateway (TestIssuer auth) + real Indexer + real
//! DuckDB. Proves: an admin grants `team-acme` membership via
//! `add_group_member`, after which a teammate (agent) gains the group
//! read the skill header declares; a non-admin is refused membership
//! mutation with JSON-RPC -32001; `list_group_members` reflects the
//! seeded rows. No mocks, no LLM in the decision.

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "stuttgart-ai";
const ALICE: &str = "whatsapp:111";
const BOB: &str = "whatsapp:222";

const DEAL_NOTE_SKILL: &str = "---\ntype: skill\nid: deal_note\n\
    description: A shared deal note.\nowner_field: author\n\
    acl:\n  read: [owner, team-acme]\n  create: [owner]\n  update: [owner]\n  delete: [owner]\n\
    ---\n# deal_note\n";
const ALICE_NOTE: &str = "---\ntype: instance\nskill: deal_note\nid: alice-q3\n\
    author: \"whatsapp:111\"\n---\n# Alice Q3\nPipeline.\n";
const ALICE_NOTE_PAGE: &str = "markdown/instances/deal_note/alice-q3.md";

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("deal_note", DEAL_NOTE_SKILL)
                .instance("deal_note", "alice-q3", ALICE_NOTE)
                .done(),
        ),
        ..Default::default()
    })
    .await
}

/// Raw `tools/call` — returns the whole JSON-RPC body so a test can
/// assert on either `result` or `error`.
async fn call_raw(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
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

async fn call_ok(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    let body = call_raw(p, token, name, args).await;
    assert!(body.get("error").is_none(), "{name} error: {body}");
    body["result"]["structuredContent"].clone()
}

#[tokio::test]
async fn admin_adds_member_then_agent_gains_group_read() {
    let p = start().await;
    let admin = p.mint_token_with_groups(TENANT, "operator", &[], true);
    let bob = p.mint_token_with_sub(TENANT, Role::Agent, BOB);

    // Before membership: Bob cannot read Alice's owner+team-acme note.
    let before = call_ok(&p, &bob, "expand", json!({ "page_id": ALICE_NOTE_PAGE })).await;
    assert!(
        before["page"].is_null(),
        "bob is not yet in team-acme, note is hidden: {before}"
    );

    // Admin grants membership.
    call_ok(
        &p,
        &admin,
        "add_group_member",
        json!({ "group_id": "team-acme", "subject": BOB }),
    )
    .await;

    // After membership: Bob reads Alice's note via team-acme.
    let after = call_ok(&p, &bob, "expand", json!({ "page_id": ALICE_NOTE_PAGE })).await;
    assert!(
        after["page"].is_object(),
        "bob now reads alice's note via team-acme: {after}"
    );
}

#[tokio::test]
async fn non_admin_cannot_mutate_membership() {
    let p = start().await;
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let body = call_raw(
        &p,
        &alice,
        "add_group_member",
        json!({ "group_id": "team-acme", "subject": BOB }),
    )
    .await;
    assert_eq!(
        body["error"]["code"].as_i64(),
        Some(-32001),
        "a non-admin must be refused membership mutation: {body}"
    );
}

#[tokio::test]
async fn list_group_members_returns_seeded_rows() {
    let p = start().await;
    let admin = p.mint_token_with_groups(TENANT, "operator", &[], true);
    call_ok(
        &p,
        &admin,
        "add_group_member",
        json!({ "group_id": "team-acme", "subject": ALICE }),
    )
    .await;
    call_ok(
        &p,
        &admin,
        "add_group_member",
        json!({ "group_id": "team-acme", "subject": BOB }),
    )
    .await;

    let listed = call_ok(
        &p,
        &admin,
        "list_group_members",
        json!({ "group_id": "team-acme" }),
    )
    .await;
    let subjects: Vec<&str> = listed["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["subject"].as_str().unwrap())
        .collect();
    assert!(
        subjects.contains(&ALICE) && subjects.contains(&BOB),
        "got {subjects:?}"
    );
}
