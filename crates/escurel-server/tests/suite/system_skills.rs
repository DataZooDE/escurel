//! System skills (#823 prerequisite): every served tenant ships the `agent`
//! skill alongside the `escurel` meta-skill, and agent pages are authority —
//! writable by the admin role alone, whatever the skill page's own `acl:`
//! block says, because a widened grant there would let a member hand a run
//! whatever harness and scopes they liked.
//!
//! Real gateway over HTTP under `ESCUREL_WRITE_ACL=enforce`.

use escurel_server::WriteAclMode;
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const MEMBER: &str = "member:alice";
const AGENT_PAGE: &str = "markdown/instances/agent/researcher.md";
const AGENT_SKILL_PAGE: &str = "markdown/skills/agent.md";
const RESEARCHER: &str = "---\nkind: instance\nskill: agent\nid: researcher\nharness: echo\n\
    skills: [\"[[skill::note]]\"]\nscopes:\n  write: [\"markdown/instances/note/*\"]\n---\n\
    # researcher\n";
/// The agent skill page, rewritten by an admin to grant writes to everyone.
const WIDENED_AGENT_SKILL: &str = "---\nkind: skill\nid: agent\ndescription: widened\n\
    acl:\n  create: [public]\n  update: [public]\n---\n# agent\n";
const NOTE: &str = "---\nkind: skill\nid: note\ndescription: n\n\
    acl:\n  create: [public]\n  update: [public]\n---\n# note\n";

/// A skill that declares no `acl:`, so the shipped default (admin-only
/// writes) governs it.
const CUSTOMER: &str = "---\nkind: skill\nid: customer\ndescription: c\n---\n# customer\n";

async fn gateway() -> EscurelProcess {
    gateway_in(WriteAclMode::Enforce).await
}

async fn gateway_in(mode: WriteAclMode) -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            write_acl: Some(mode),
            ..Default::default()
        },
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("note", NOTE)
                .skill("customer", CUSTOMER)
                .done(),
        ),
    })
    .await
}

async fn call(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    let body: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": { "name": name, "arguments": args } }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json");
    assert!(body.get("error").is_none(), "{name}: {body}");
    body["result"]["structuredContent"].clone()
}

fn member(p: &EscurelProcess) -> String {
    p.mint_token_with_sub(TENANT, Role::Agent, MEMBER)
}

fn admin(p: &EscurelProcess) -> String {
    p.mint_token(TENANT, Role::Admin)
}

async fn write(p: &EscurelProcess, token: &str, page_id: &str, content: &str) -> Value {
    call(
        p,
        token,
        "update_page",
        json!({ "page_id": page_id, "content": content }),
    )
    .await
}

#[tokio::test]
async fn a_fresh_tenant_lists_the_agent_skill() {
    let p = gateway().await;
    let skills = call(&p, &member(&p), "list_skills", json!({})).await;
    let ids: Vec<&str> = skills["skills"]
        .as_array()
        .expect("skills")
        .iter()
        .filter_map(|s| s["id"].as_str())
        .collect();
    assert!(ids.contains(&"agent"), "agent skill shipped; got {ids:?}");
    assert!(
        ids.contains(&"escurel"),
        "meta-skill still shipped; got {ids:?}"
    );
}

#[tokio::test]
async fn only_an_admin_creates_an_agent() {
    let p = gateway().await;
    // The member CAN write where the skill grants it — so the refusal below is
    // about the agent skill, not about this caller writing nothing at all.
    let note = "---\nkind: instance\nskill: note\nid: n1\n---\n# n1\n";
    let ok = write(&p, &member(&p), "markdown/instances/note/n1.md", note).await;
    assert_eq!(ok["ok"], true, "{ok}");

    let refused = write(&p, &member(&p), AGENT_PAGE, RESEARCHER).await;
    assert_eq!(
        refused["ok"], false,
        "a member must not create an agent: {refused}"
    );

    let created = write(&p, &admin(&p), AGENT_PAGE, RESEARCHER).await;
    assert_eq!(created["ok"], true, "{created}");

    let edit = RESEARCHER.replace("harness: echo", "harness: claude");
    let refused = write(&p, &member(&p), AGENT_PAGE, &edit).await;
    assert_eq!(
        refused["ok"], false,
        "a member must not edit an agent: {refused}"
    );
}

#[tokio::test]
async fn a_widened_agent_skill_page_does_not_open_agents_to_members() {
    let p = gateway().await;
    // A member cannot rewrite the agent skill page at all.
    let refused = write(&p, &member(&p), AGENT_SKILL_PAGE, WIDENED_AGENT_SKILL).await;
    assert_eq!(refused["ok"], false, "{refused}");

    // Even an admin's widening of the page's acl does not reach agents: the
    // gate is the system skill, not the block.
    let widened = write(&p, &admin(&p), AGENT_SKILL_PAGE, WIDENED_AGENT_SKILL).await;
    assert_eq!(widened["ok"], true, "{widened}");
    let refused = write(&p, &member(&p), AGENT_PAGE, RESEARCHER).await;
    assert_eq!(refused["ok"], false, "{refused}");
}

#[tokio::test]
async fn a_member_cannot_move_a_page_into_the_agent_path() {
    let p = gateway().await;
    let note = "---\nkind: instance\nskill: note\nid: n2\n---\n# n2\n";
    let ok = write(&p, &member(&p), "markdown/instances/note/n2.md", note).await;
    assert_eq!(ok["ok"], true, "{ok}");
    let moved = call(
        &p,
        &member(&p),
        "move_page",
        json!({ "from": "markdown/instances/note/n2.md", "to": "markdown/instances/agent/n2.md" }),
    )
    .await;
    assert_eq!(moved["ok"], false, "{moved}");
}

/// `off` and `log` are rollout knobs for the ordinary write ACL. An agent page
/// is authority over what a run may do, so its gate is not part of that ladder:
/// it fails closed in every mode.
#[tokio::test]
async fn the_agent_gate_holds_when_the_write_acl_is_off_or_log() {
    for mode in [WriteAclMode::Off, WriteAclMode::Log] {
        let p = gateway_in(mode).await;
        // The relaxed mode is real: a member writes a customer, which the
        // shipped default would refuse under `enforce`.
        let customer = "---\nkind: instance\nskill: customer\nid: c0\n---\n# c0\n";
        let ok = write(
            &p,
            &member(&p),
            "markdown/instances/customer/c0.md",
            customer,
        )
        .await;
        assert_eq!(ok["ok"], true, "{mode:?}: {ok}");

        let refused = write(&p, &member(&p), AGENT_PAGE, RESEARCHER).await;
        assert_eq!(
            refused["ok"], false,
            "{mode:?}: member created an agent: {refused}"
        );

        let created = write(&p, &admin(&p), AGENT_PAGE, RESEARCHER).await;
        assert_eq!(created["ok"], true, "{mode:?}: {created}");
        let edit = RESEARCHER.replace("harness: echo", "harness: claude");
        let refused = write(&p, &member(&p), AGENT_PAGE, &edit).await;
        assert_eq!(
            refused["ok"], false,
            "{mode:?}: member edited an agent: {refused}"
        );

        let refused = write(&p, &member(&p), AGENT_SKILL_PAGE, WIDENED_AGENT_SKILL).await;
        assert_eq!(
            refused["ok"], false,
            "{mode:?}: member rewrote the agent skill: {refused}"
        );

        let refused = call(
            &p,
            &member(&p),
            "delete_page",
            json!({ "page_id": AGENT_PAGE }),
        )
        .await;
        assert_eq!(
            refused["ok"], false,
            "{mode:?}: member deleted an agent: {refused}"
        );

        let note = "---\nkind: instance\nskill: note\nid: n9\n---\n# n9\n";
        write(&p, &member(&p), "markdown/instances/note/n9.md", note).await;
        let moved = call(
            &p,
            &member(&p),
            "move_page",
            json!({ "from": "markdown/instances/note/n9.md", "to": "markdown/instances/agent/n9.md" }),
        )
        .await;
        assert_eq!(
            moved["ok"], false,
            "{mode:?}: member moved into agents: {moved}"
        );

        let held = call(
            &p,
            &member(&p),
            "create_draft",
            json!({ "target_page_id": AGENT_PAGE, "content": edit }),
        )
        .await;
        assert_eq!(
            held["ok"], false,
            "{mode:?}: member drafted an agent: {held}"
        );

        let fetched = call(&p, &admin(&p), "expand", json!({ "page_id": AGENT_PAGE })).await;
        let fetched = fetched.to_string();
        assert!(
            fetched.contains("echo") && !fetched.contains("claude"),
            "{mode:?}: the agent page is unchanged: {fetched}"
        );
    }
}

/// A move creates its destination, so the destination gets the check a create
/// at that id would: the skill its PATH names, not only the one the moved
/// page's frontmatter declares (pre-existing gap, every caller).
#[tokio::test]
async fn a_move_is_gated_by_the_destination_skill() {
    let p = gateway().await;
    let note = "---\nkind: instance\nskill: note\nid: n3\n---\n# n3\n";
    let ok = write(&p, &member(&p), "markdown/instances/note/n3.md", note).await;
    assert_eq!(ok["ok"], true, "{ok}");

    let moved = call(
        &p,
        &member(&p),
        "move_page",
        json!({ "from": "markdown/instances/note/n3.md", "to": "markdown/instances/customer/n3.md" }),
    )
    .await;
    assert_eq!(
        moved["ok"], false,
        "a member moved a note into customers: {moved}"
    );
    assert_eq!(moved["issues"][0]["location"], "to", "{moved}");

    // The same bytes written straight to the customer path are refused too:
    // the path's skill gates the create, not the frontmatter's claim.
    let direct = write(&p, &member(&p), "markdown/instances/customer/n3.md", note).await;
    assert_eq!(direct["ok"], false, "{direct}");

    let moved = call(
        &p,
        &admin(&p),
        "move_page",
        json!({ "from": "markdown/instances/note/n3.md", "to": "markdown/instances/customer/n3.md" }),
    )
    .await;
    assert_eq!(moved["ok"], true, "an admin may: {moved}");
}
