//! Deterministic per-instance ACL, enforced at the MCP boundary over
//! real HTTP. A running gateway (TestIssuer auth) + real Indexer; tokens
//! carry the owning subject. Proves a member sees only their own
//! owner-private records (profile, event_profile) while public program
//! data stays world-readable, and the admin role bypasses — no LLM, no
//! agent in the decision.

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "stuttgart-ai";
const ALICE: &str = "whatsapp:111";
const BOB: &str = "whatsapp:222";

const MEMBER_SKILL: &str = "---\ntype: skill\nid: community_member\n\
    description: A member.\nvisibility: owner\nowner_field: credential\n---\n# community_member\n";
const EVENT_PROFILE_SKILL: &str = "---\ntype: skill\nid: event_profile\n\
    description: Per-event profile.\nvisibility: owner\nowner_field: member\n\
    required_frontmatter: [member]\n---\n# event_profile\n";
const TALK_SKILL: &str = "---\ntype: skill\nid: talk\ndescription: A program item.\n\
    visibility: public\n---\n# talk\n";

const ALICE_MEMBER: &str = "---\ntype: instance\nskill: community_member\nid: alice\n\
    credential: \"whatsapp:111\"\n---\n# Alice\n";
const BOB_MEMBER: &str = "---\ntype: instance\nskill: community_member\nid: bob\n\
    credential: \"whatsapp:222\"\n---\n# Bob\n";
const ALICE_PROFILE: &str = "---\ntype: instance\nskill: event_profile\nid: alice-ki\n\
    member: \"[[community_member::alice]]\"\n---\n# Alice @ KI-Gipfel\nInnovation Managerin.\n";
const KEYNOTE: &str =
    "---\ntype: instance\nskill: talk\nid: keynote\n---\n# Keynote\nDie Eröffnung.\n";

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("community_member", MEMBER_SKILL)
                .skill("event_profile", EVENT_PROFILE_SKILL)
                .skill("talk", TALK_SKILL)
                .instance("community_member", "alice", ALICE_MEMBER)
                .instance("community_member", "bob", BOB_MEMBER)
                .instance("event_profile", "alice-ki", ALICE_PROFILE)
                .instance("talk", "keynote", KEYNOTE)
                .done(),
        ),
        ..Default::default()
    })
    .await
}

/// Call a tool over MCP-over-HTTP with `token`, returning the
/// `structuredContent` payload. Panics on a JSON-RPC error envelope.
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

const ALICE_PAGE: &str = "markdown/instances/community_member/alice.md";
const ALICE_PROFILE_PAGE: &str = "markdown/instances/event_profile/alice-ki.md";
const KEYNOTE_PAGE: &str = "markdown/instances/talk/keynote.md";

#[tokio::test]
async fn owner_expands_own_profile_non_owner_sees_null() {
    let p = start().await;
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let bob = p.mint_token_with_sub(TENANT, Role::Agent, BOB);

    let own = call(&p, &alice, "expand", json!({ "page_id": ALICE_PAGE })).await;
    assert!(own["page"].is_object(), "alice expands her own profile");

    let other = call(&p, &bob, "expand", json!({ "page_id": ALICE_PAGE })).await;
    assert!(
        other["page"].is_null(),
        "bob must see alice's owner-private profile as absent, got: {other}"
    );
}

#[tokio::test]
async fn event_profile_owner_resolved_through_wikilink() {
    let p = start().await;
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let bob = p.mint_token_with_sub(TENANT, Role::Agent, BOB);

    let own = call(
        &p,
        &alice,
        "expand",
        json!({ "page_id": ALICE_PROFILE_PAGE }),
    )
    .await;
    assert!(
        own["page"].is_object(),
        "alice owns the event_profile via member→credential"
    );

    let other = call(&p, &bob, "expand", json!({ "page_id": ALICE_PROFILE_PAGE })).await;
    assert!(
        other["page"].is_null(),
        "bob must NOT read alice's event_profile"
    );
}

#[tokio::test]
async fn list_instances_filters_owner_private_rows() {
    let p = start().await;
    let bob = p.mint_token_with_sub(TENANT, Role::Agent, BOB);

    let listed = call(
        &p,
        &bob,
        "list_instances",
        json!({ "skill_id": "community_member" }),
    )
    .await;
    let ids: Vec<&str> = listed["instances"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|i| i["frontmatter"]["id"].as_str())
        .collect();
    assert_eq!(
        ids,
        vec!["bob"],
        "bob enumerates only his own member row, got: {ids:?}"
    );
}

#[tokio::test]
async fn admin_bypasses_and_public_is_world_readable() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);
    let bob = p.mint_token_with_sub(TENANT, Role::Agent, BOB);

    // Admin (operator dashboard) reads anyone's owner-private profile.
    let as_admin = call(&p, &admin, "expand", json!({ "page_id": ALICE_PAGE })).await;
    assert!(
        as_admin["page"].is_object(),
        "admin bypasses owner-visibility"
    );

    // Admin enumerates every member.
    let all = call(
        &p,
        &admin,
        "list_instances",
        json!({ "skill_id": "community_member" }),
    )
    .await;
    assert_eq!(
        all["instances"].as_array().unwrap().len(),
        2,
        "admin sees both members"
    );

    // Public program data is readable by any member.
    let talk = call(&p, &bob, "expand", json!({ "page_id": KEYNOTE_PAGE })).await;
    assert!(talk["page"].is_object(), "a public talk is world-readable");
}
