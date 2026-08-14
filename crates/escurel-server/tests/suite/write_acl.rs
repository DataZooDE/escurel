//! Deterministic per-instance WRITE ACL, enforced at the MCP boundary
//! over real HTTP — symmetric to `instance_acl.rs` (reads). Only the
//! resolved owner (or admin) may mutate an owner-private instance;
//! public / no-`owner_field` instances are admin-write-only. The
//! `ESCUREL_WRITE_ACL` mode gates it: `Off` = legacy (no check),
//! `Enforce` = reject. No LLM, no agent in the decision.

use escurel_server::WriteAclMode;
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "stuttgart-ai";
const ALICE: &str = "whatsapp:111";
const BOB: &str = "whatsapp:222";

const MEMBER_SKILL: &str = "---\ntype: skill\nid: community_member\n\
    description: A member.\nvisibility: owner\nowner_field: credential\n---\n# community_member\n";
const TALK_SKILL: &str = "---\ntype: skill\nid: talk\ndescription: A program item.\n\
    visibility: public\n---\n# talk\n";

const ALICE_MEMBER: &str = "---\ntype: instance\nskill: community_member\nid: alice\n\
    credential: \"whatsapp:111\"\n---\n# Alice\n";
const BOB_MEMBER: &str = "---\ntype: instance\nskill: community_member\nid: bob\n\
    credential: \"whatsapp:222\"\n---\n# Bob\n";
const KEYNOTE: &str =
    "---\ntype: instance\nskill: talk\nid: keynote\n---\n# Keynote\nDie Eröffnung.\n";

const ALICE_PAGE: &str = "markdown/instances/community_member/alice.md";
const KEYNOTE_PAGE: &str = "markdown/instances/talk/keynote.md";

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
                .skill("talk", TALK_SKILL)
                .instance("community_member", "alice", ALICE_MEMBER)
                .instance("community_member", "bob", BOB_MEMBER)
                .instance("talk", "keynote", KEYNOTE)
                .done(),
        ),
    })
    .await
}

/// `update_page` over MCP-over-HTTP with `token`; returns `structuredContent`
/// (the `{ok, issues}` shape). Panics on a JSON-RPC error envelope.
async fn update(p: &EscurelProcess, token: &str, page_id: &str, content: &str) -> Value {
    let resp = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "update_page",
                "arguments": { "page_id": page_id, "content": content } },
        }))
        .send()
        .await
        .expect("post");
    assert_eq!(resp.status(), 200, "http status");
    let body: Value = resp.json().await.unwrap();
    assert!(body.get("error").is_none(), "update_page error: {body}");
    body["result"]["structuredContent"].clone()
}

// Alice's member page with a tweaked body — same owner (credential).
const ALICE_MEMBER_EDIT: &str = "---\ntype: instance\nskill: community_member\nid: alice\n\
    credential: \"whatsapp:111\"\n---\n# Alice\nEdited by the owner.\n";
// A hijack attempt: rewrite alice's page, keeping her credential, by Bob.
// A create-for-other: a NEW member page owned by alice, written by Bob.
const NEW_MALLORY_AS_ALICE: &str = "---\ntype: instance\nskill: community_member\nid: mallory\n\
    credential: \"whatsapp:111\"\n---\n# Mallory\n";
const NEW_MALLORY_PAGE: &str = "markdown/instances/community_member/mallory.md";

#[tokio::test]
async fn owner_writes_own_instance() {
    let p = start(WriteAclMode::Enforce).await;
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let r = update(&p, &alice, ALICE_PAGE, ALICE_MEMBER_EDIT).await;
    assert_eq!(r["ok"], json!(true), "alice may edit her own record: {r}");
}

#[tokio::test]
async fn non_owner_write_denied() {
    let p = start(WriteAclMode::Enforce).await;
    let bob = p.mint_token_with_sub(TENANT, Role::Agent, BOB);
    // Bob overwrites Alice's owner-private record → denied (no hijack).
    let r = update(&p, &bob, ALICE_PAGE, ALICE_MEMBER_EDIT).await;
    assert_eq!(r["ok"], json!(false), "bob must NOT overwrite alice: {r}");
    assert_eq!(r["issues"][0]["code"], json!("forbidden"), "{r}");
}

#[tokio::test]
async fn create_for_another_owner_denied() {
    let p = start(WriteAclMode::Enforce).await;
    let bob = p.mint_token_with_sub(TENANT, Role::Agent, BOB);
    // Bob creates a NEW record whose owner is alice → denied (no transfer).
    let r = update(&p, &bob, NEW_MALLORY_PAGE, NEW_MALLORY_AS_ALICE).await;
    assert_eq!(
        r["ok"],
        json!(false),
        "bob must NOT create an instance owned by alice: {r}"
    );
}

#[tokio::test]
async fn public_instance_write_is_admin_only() {
    let p = start(WriteAclMode::Enforce).await;
    let bob = p.mint_token_with_sub(TENANT, Role::Agent, BOB);
    // A public talk has no owner → only admin may write it.
    let edited = "---\ntype: instance\nskill: talk\nid: keynote\n---\n# Keynote\nHacked.\n";
    let r = update(&p, &bob, KEYNOTE_PAGE, edited).await;
    assert_eq!(
        r["ok"],
        json!(false),
        "a non-admin must NOT write a public talk: {r}"
    );
}

#[tokio::test]
async fn admin_writes_anything() {
    let p = start(WriteAclMode::Enforce).await;
    let admin = p.mint_token(TENANT, Role::Admin);
    // Admin edits alice's owner-private record AND a public talk.
    let r1 = update(&p, &admin, ALICE_PAGE, ALICE_MEMBER_EDIT).await;
    assert_eq!(r1["ok"], json!(true), "admin edits any member: {r1}");
    let edited = "---\ntype: instance\nskill: talk\nid: keynote\n---\n# Keynote\nCurated.\n";
    let r2 = update(&p, &admin, KEYNOTE_PAGE, edited).await;
    assert_eq!(r2["ok"], json!(true), "admin curates public talks: {r2}");
}

/// Raw `tools/call` returning the full JSON-RPC body — needed where the
/// expected outcome is an error envelope, which `update` panics on.
async fn call(p: &EscurelProcess, token: &str, tool: &str, args: Value) -> Value {
    let resp = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": tool, "arguments": args },
        }))
        .send()
        .await
        .expect("post");
    assert_eq!(resp.status(), 200, "http status");
    resp.json().await.unwrap()
}

/// `purge_page` destroys the audit husk a soft delete retained — an
/// operator act, not an agent one. The dispatch arm must refuse an
/// agent-role token with `-32001` (admin required), exactly like the
/// other audit-destroying admin tools; admin then purges the husk.
#[tokio::test]
async fn purge_page_requires_admin_role() {
    let p = start(WriteAclMode::Enforce).await;
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);

    // Alice retracts her own page — allowed, leaves the archived husk.
    let del = call(&p, &alice, "delete_page", json!({ "page_id": ALICE_PAGE })).await;
    assert_eq!(
        del["result"]["structuredContent"]["ok"],
        json!(true),
        "owner retracts her own record: {del}"
    );

    // An agent token — even the page's owner — must not purge the husk.
    let purge = call(&p, &alice, "purge_page", json!({ "page_id": ALICE_PAGE })).await;
    assert_eq!(
        purge["error"]["code"],
        json!(-32001),
        "agent purge must be refused with admin-required: {purge}"
    );

    // Admin finishes the job.
    let admin = p.mint_token(TENANT, Role::Admin);
    let purged = call(&p, &admin, "purge_page", json!({ "page_id": ALICE_PAGE })).await;
    assert_eq!(
        purged["result"]["structuredContent"]["ok"],
        json!(true),
        "admin purges the archived husk: {purged}"
    );
}

#[tokio::test]
async fn off_mode_does_not_gate_writes() {
    // The same non-owner write that Enforce rejects must SUCCEED with the
    // flag off — proving the gate (not some other guard) is what blocks it.
    let p = start(WriteAclMode::Off).await;
    let bob = p.mint_token_with_sub(TENANT, Role::Agent, BOB);
    let r = update(&p, &bob, ALICE_PAGE, ALICE_MEMBER_EDIT).await;
    assert_eq!(
        r["ok"],
        json!(true),
        "off mode allows the legacy write: {r}"
    );
}
