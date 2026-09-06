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

/// `list_skills` still reports the resolved block — **to an admin caller**.
/// It stopped reporting it to an agent in #374: `team-acme` is a group
/// name, and a grant list is the tenant's authorisation topology rather
/// than the skill's schema.
#[tokio::test]
async fn list_skills_reports_resolved_acl_block_to_an_admin() {
    let p = start().await;
    let admin = p.mint_token_with_groups(TENANT, "operator", &[], true);
    let skills = call_ok(&p, &admin, "list_skills", json!({})).await;
    let deal = skills["skills"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "deal_note")
        .expect("deal_note present");
    assert_eq!(deal["acl"]["read"], json!(["owner", "team-acme"]));
    assert_eq!(deal["acl"]["create"], json!(["owner"]));
    assert_eq!(deal["acl"]["update"], json!(["owner"]));
    assert_eq!(deal["acl"]["delete"], json!(["owner"]));
    assert_eq!(deal["owner_field"], json!("author"));
}

/// The agent's view of the same row (#374): the skill is **still listed** —
/// its `acl.read` names the structural `owner` group, which never hides a
/// *type* — and it still carries the schema an agent needs, but the group
/// names are gone.
#[tokio::test]
async fn list_skills_withholds_the_acl_block_from_an_agent() {
    let p = start().await;
    let agent = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let skills = call_ok(&p, &agent, "list_skills", json!({})).await;
    let deal = skills["skills"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "deal_note")
        .expect("deal_note is still discoverable");
    // Positive control: the schema half of the row survives.
    assert_eq!(deal["owner_field"], json!("author"));
    assert_eq!(deal["description"], json!("A shared deal note."));
    assert!(deal["acl"].is_null(), "no grant list for an agent: {deal}");
    assert!(
        !serde_json::to_string(&skills)
            .unwrap()
            .contains("team-acme"),
        "no group name anywhere in the catalogue: {skills}"
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

/// Membership grants **write**, not only read — the property a deployment
/// depends on when its callers' tokens carry no groups at all.
///
/// heron mints exactly such a token: it shares the platform's signing
/// identity and emits no groups claim, so for its consultants every
/// group-granted write rests on DuckDB membership alone. Measured on the
/// device before this existed: the Approve button called through and the
/// store answered `caller ... does not own instance ...`, because nothing
/// had granted the subject anything.
///
/// The read case above cannot stand in for this one. `may_read_instance` and
/// `may_write_instance` resolve different verbs against different defaults —
/// a public skill reads for everyone and writes for nobody — so "membership
/// is visible to reads" says nothing about whether a write policy sees it.
#[tokio::test]
async fn duckdb_membership_admits_a_groupless_token_to_a_group_granted_write() {
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: escurel_test_support::ConfigOverrides {
            // The deployed posture. Without it the ACL is not consulted and
            // both halves below would pass for the wrong reason.
            write_acl: Some(escurel_server::WriteAclMode::Enforce),
        },
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill(
                    "shared_note",
                    "---\ntype: skill\nid: shared_note\ndescription: A note the team may edit.\n\
                     acl:\n  read: [public]\n  create: [team-acme]\n  update: [team-acme]\n---\n# shared_note\n",
                )
                .instance(
                    "shared_note",
                    "q3",
                    "---\ntype: instance\nskill: shared_note\nid: q3\n---\n# Q3\nOriginal.\n",
                )
                .done(),
        ),
        ..Default::default()
    })
    .await;

    // A token with NO groups — what heron mints for a consultant.
    let groupless = p.mint_token_with_sub(TENANT, Role::Agent, BOB);
    let page = "markdown/instances/shared_note/q3.md";
    let revised = "---\ntype: instance\nskill: shared_note\nid: q3\n---\n# Q3\nRevised.\n";

    let refused = call_ok(
        &p,
        &groupless,
        "update_page",
        json!({ "page_id": page, "content": revised }),
    )
    .await;
    assert_eq!(
        refused["ok"],
        json!(false),
        "a groupless token must not pass a group-granted write: {refused}"
    );
    assert_eq!(
        refused["issues"][0]["code"],
        json!("forbidden"),
        "{refused}"
    );

    // The ONE change: membership, granted server-side by an admin.
    let admin = p.mint_token(TENANT, Role::Admin);
    call_ok(
        &p,
        &admin,
        "add_group_member",
        json!({ "group_id": "team-acme", "subject": BOB }),
    )
    .await;

    let allowed = call_ok(
        &p,
        &groupless,
        "update_page",
        json!({ "page_id": page, "content": revised }),
    )
    .await;
    assert_eq!(
        allowed["ok"],
        json!(true),
        "membership must admit the same token to the same write: {allowed}"
    );

    // ...and it really landed, rather than answering ok on a write that did
    // nothing. Read back through `expand`, as any client would.
    let body = call_ok(&p, &groupless, "expand", json!({ "page_id": page })).await;
    assert!(
        body["body"]
            .as_str()
            .unwrap_or_default()
            .contains("Revised."),
        "the write must be in the page: {body}"
    );

    p.shutdown().await;
}
