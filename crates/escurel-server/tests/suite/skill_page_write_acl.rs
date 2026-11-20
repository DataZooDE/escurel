//! Who may write a SKILL page and promote a draft of one (`ESCUREL_SKILL_ADMIN_GROUPS`).
//!
//! A skill page is `markdown/skills/*`, or any page whose stored or incoming
//! frontmatter says `kind: skill`. Writing one — `update_page`, `delete_page`,
//! `move_page`, a session commit, and the promotions `promote_draft` (with or
//! without corrected content) and `promote_changeset` — needs `escurel:admin`
//! or a group the operator names in `ESCUREL_SKILL_ADMIN_GROUPS`. Unset means
//! `escurel:admin` only; `*` keeps the earlier open behaviour. PROPOSING stays
//! open: anyone who could write the page before may still `create_draft` it.
//! A changeset holding a skill draft the promoter may not land is refused as a
//! whole, before any member lands. Instance pages are not affected.
//!
//! Real gateway over HTTP, `ESCUREL_WRITE_ACL=enforce`.

use escurel_server::WriteAclMode;
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const TENANT: &str = "acme";
const PAGE: &str = "markdown/skills/playbook.md";
const PLAYBOOK: &str =
    "---\nkind: skill\nid: playbook\ndescription: How we ship.\n---\n# playbook\n";
/// Owner-written notes: Bob may write a note he owns.
const NOTE: &str = "---\nkind: skill\nid: note\ndescription: A note.\nowner_field: owner\n\
    acl:\n  create: [owner]\n  update: [owner]\n---\n# note\n";
const NOTE_PAGE: &str = "markdown/instances/note/n1.md";
const NOTE_N1: &str = "---\nkind: instance\nskill: note\nid: n1\nowner: member:bob\n---\n# n1\n";
const TENANT_ADMIN: &str = "portfolio:tenant-admin";

async fn gateway(skill_admin_groups: Option<&str>) -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            write_acl: Some(WriteAclMode::Enforce),
            skill_admin_groups: skill_admin_groups.map(str::to_owned),
            ..Default::default()
        },
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("playbook", PLAYBOOK)
                .skill("note", NOTE)
                .done(),
        ),
    })
    .await
}

async fn rpc(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": { "name": name, "arguments": args } }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json")
}

async fn ok(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    let body = rpc(p, token, name, args).await;
    assert!(body.get("error").is_none(), "{name}: {body}");
    let result = body["result"]["structuredContent"].clone();
    assert_eq!(result["ok"], true, "{name}: {result}");
    result
}

/// Refused with `skill_page_needs_admin`, naming the page.
async fn refused(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    let body = rpc(p, token, name, args).await;
    let result = &body["result"]["structuredContent"];
    assert_eq!(result["ok"], false, "{name} must be refused: {body}");
    let issue = result["issues"]
        .as_array()
        .and_then(|issues| {
            issues
                .iter()
                .find(|i| i["code"] == "skill_page_needs_admin")
        })
        .unwrap_or_else(|| panic!("{name}: no skill_page_needs_admin issue: {body}"))
        .clone();
    assert!(
        issue["message"]
            .as_str()
            .unwrap_or_default()
            .contains("markdown/skills/"),
        "{issue}"
    );
    result.clone()
}

fn member(p: &EscurelProcess) -> String {
    p.mint_token_with_sub(TENANT, Role::Agent, "member:bob")
}

fn tenant_admin(p: &EscurelProcess) -> String {
    p.mint_token_with_groups(TENANT, "member:tia", &[TENANT_ADMIN], false)
}

fn admin(p: &EscurelProcess) -> String {
    p.mint_token(TENANT, Role::Admin)
}

fn write(page_id: &str, content: &str) -> Value {
    json!({ "page_id": page_id, "content": content })
}

fn reworded() -> String {
    PLAYBOOK.replace("How we ship.", "How we ship, carefully.")
}

async fn stored(p: &EscurelProcess, page_id: &str) -> Value {
    rpc(p, &admin(p), "expand", json!({ "page_id": page_id })).await["result"]["structuredContent"]
        .clone()
}

async fn description(p: &EscurelProcess) -> String {
    stored(p, PAGE).await["frontmatter"]["description"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

async fn draft(p: &EscurelProcess, token: &str, target: &str, content: &str) -> String {
    let made = ok(
        p,
        token,
        "create_draft",
        json!({ "target_page_id": target, "content": content }),
    )
    .await;
    made["draft"]["draft_id"]
        .as_str()
        .expect("draft id")
        .to_owned()
}

#[tokio::test]
async fn by_default_only_an_escurel_admin_writes_a_skill_page() {
    let p = gateway(None).await;
    refused(&p, &member(&p), "update_page", write(PAGE, &reworded())).await;
    // A group the operator has not named grants nothing.
    refused(
        &p,
        &tenant_admin(&p),
        "update_page",
        write(PAGE, &reworded()),
    )
    .await;
    assert_eq!(description(&p).await, "How we ship.");
    ok(&p, &admin(&p), "update_page", write(PAGE, &reworded())).await;
    assert_eq!(description(&p).await, "How we ship, carefully.");
    // A new skill page is a skill page too.
    let fresh = "---\nkind: skill\nid: fresh\ndescription: f\n---\n# fresh\n";
    refused(
        &p,
        &member(&p),
        "update_page",
        write("markdown/skills/fresh.md", fresh),
    )
    .await;
    p.shutdown().await;
}

#[tokio::test]
async fn a_member_proposes_a_skill_change_but_cannot_promote_it() {
    let p = gateway(Some(TENANT_ADMIN)).await;
    let bob = member(&p);
    let draft_id = draft(&p, &bob, PAGE, &reworded()).await;
    refused(&p, &bob, "promote_draft", json!({ "draft_id": draft_id })).await;
    // Correcting the content on approval is a promotion too.
    let corrected = PLAYBOOK.replace("How we ship.", "Bob's way.");
    refused(
        &p,
        &bob,
        "promote_draft",
        json!({ "draft_id": draft_id, "content": corrected }),
    )
    .await;
    assert_eq!(description(&p).await, "How we ship.", "nothing landed");
    // The draft is still open: the tenant administrator promotes it.
    ok(
        &p,
        &tenant_admin(&p),
        "promote_draft",
        json!({ "draft_id": draft_id }),
    )
    .await;
    assert_eq!(description(&p).await, "How we ship, carefully.");
    p.shutdown().await;
}

#[tokio::test]
async fn a_configured_group_writes_skill_pages_and_an_escurel_admin_still_does() {
    let p = gateway(Some(&format!("ops, {TENANT_ADMIN}"))).await;
    ok(
        &p,
        &tenant_admin(&p),
        "update_page",
        write(PAGE, &reworded()),
    )
    .await;
    let ops = p.mint_token_with_groups(TENANT, "member:olga", &["ops"], false);
    ok(&p, &ops, "update_page", write(PAGE, PLAYBOOK)).await;
    ok(&p, &admin(&p), "update_page", write(PAGE, &reworded())).await;
    refused(&p, &member(&p), "update_page", write(PAGE, PLAYBOOK)).await;
    p.shutdown().await;
}

#[tokio::test]
async fn a_changeset_with_a_skill_draft_is_refused_as_a_whole() {
    let p = gateway(Some(TENANT_ADMIN)).await;
    let bob = member(&p);
    let first = ok(
        &p,
        &bob,
        "create_draft",
        json!({ "target_page_id": NOTE_PAGE, "new_changeset": true, "content": NOTE_N1 }),
    )
    .await;
    let changeset = first["draft"]["changeset_id"]
        .as_str()
        .expect("changeset")
        .to_owned();
    ok(
        &p,
        &bob,
        "create_draft",
        json!({ "target_page_id": PAGE, "changeset_id": changeset, "content": reworded(),
                "base_sha256": format!("{:x}", Sha256::digest(PLAYBOOK.as_bytes())) }),
    )
    .await;
    refused(
        &p,
        &bob,
        "promote_changeset",
        json!({ "changeset_id": changeset }),
    )
    .await;
    assert!(
        stored(&p, NOTE_PAGE).await["body"].is_null(),
        "Bob's own note did not land either"
    );
    assert_eq!(description(&p).await, "How we ship.");
    // An escurel admin lands the whole batch (the note is Bob's, so a tenant administrator who is
    // not its owner could not land that member: instance pages keep their own ACL).
    ok(
        &p,
        &admin(&p),
        "promote_changeset",
        json!({ "changeset_id": changeset }),
    )
    .await;
    assert_eq!(description(&p).await, "How we ship, carefully.");
    assert!(!stored(&p, NOTE_PAGE).await["body"].is_null());
    // A skill release as a changeset: the tenant administrator promotes it.
    let release = ok(
        &p,
        &bob,
        "create_draft",
        json!({ "target_page_id": PAGE, "new_changeset": true, "content": PLAYBOOK,
                "base_sha256": format!("{:x}", Sha256::digest(reworded().as_bytes())) }),
    )
    .await;
    let release = release["draft"]["changeset_id"]
        .as_str()
        .expect("changeset");
    refused(
        &p,
        &bob,
        "promote_changeset",
        json!({ "changeset_id": release }),
    )
    .await;
    ok(
        &p,
        &tenant_admin(&p),
        "promote_changeset",
        json!({ "changeset_id": release }),
    )
    .await;
    assert_eq!(description(&p).await, "How we ship.");
    p.shutdown().await;
}

#[tokio::test]
async fn a_run_token_is_never_granted_by_the_group() {
    let p = gateway(Some(TENANT_ADMIN)).await;
    // A tenant administrator's groups on a token minted for a run: a machine, not the person. Its
    // skill-page edit is held as a draft (as before), and it may not promote it.
    let run = p.resign_token_with_claim(&tenant_admin(&p), "run_id", json!("run-1"));
    let held = ok(&p, &run, "update_page", write(PAGE, &reworded())).await;
    assert_eq!(held["held_for_review"], true, "{held}");
    assert_eq!(description(&p).await, "How we ship.");
    let draft_id = held["draft"]["draft_id"].as_str().expect("draft id");
    let body = rpc(&p, &run, "promote_draft", json!({ "draft_id": draft_id })).await;
    assert_ne!(body["result"]["structuredContent"]["ok"], true, "{body}");
    assert_eq!(description(&p).await, "How we ship.");
    ok(
        &p,
        &tenant_admin(&p),
        "promote_draft",
        json!({ "draft_id": draft_id }),
    )
    .await;
    assert_eq!(description(&p).await, "How we ship, carefully.");
    p.shutdown().await;
}

#[tokio::test]
async fn a_branch_with_a_skill_page_merges_only_for_the_group() {
    let p = gateway(Some(TENANT_ADMIN)).await;
    let bob = member(&p);
    ok(&p, &bob, "create_branch", json!({ "name": "wip" })).await;
    let mut note = write(NOTE_PAGE, NOTE_N1);
    note["branch"] = json!("wip");
    ok(&p, &bob, "update_page", note).await;
    let mut skill = write(PAGE, &reworded());
    skill["branch"] = json!("wip");
    // Bob's branch write of a skill page is a skill-page write too.
    refused(&p, &bob, "update_page", skill.clone()).await;
    ok(&p, &admin(&p), "update_page", skill).await;
    refused(&p, &bob, "merge_branch", json!({ "name": "wip" })).await;
    assert_eq!(description(&p).await, "How we ship.");
    assert!(
        stored(&p, NOTE_PAGE).await["body"].is_null(),
        "Bob's note did not land either"
    );
    ok(&p, &admin(&p), "merge_branch", json!({ "name": "wip" })).await;
    assert_eq!(description(&p).await, "How we ship, carefully.");
    p.shutdown().await;
}

#[tokio::test]
async fn instance_pages_keep_their_own_acl() {
    let p = gateway(Some(TENANT_ADMIN)).await;
    // Bob writes the note he owns, through every door, as before.
    ok(&p, &member(&p), "update_page", write(NOTE_PAGE, NOTE_N1)).await;
    let draft_id = draft(
        &p,
        &member(&p),
        NOTE_PAGE,
        &NOTE_N1.replace("# n1", "# n1\n\nv2"),
    )
    .await;
    ok(
        &p,
        &member(&p),
        "promote_draft",
        json!({ "draft_id": draft_id }),
    )
    .await;
    ok(
        &p,
        &member(&p),
        "delete_page",
        json!({ "page_id": NOTE_PAGE }),
    )
    .await;
    p.shutdown().await;
}

#[tokio::test]
async fn deleting_moving_or_relabelling_a_skill_page_needs_the_group() {
    let p = gateway(Some(TENANT_ADMIN)).await;
    let bob = member(&p);
    let body = rpc(&p, &bob, "delete_page", json!({ "page_id": PAGE })).await;
    assert_eq!(body["result"]["structuredContent"]["ok"], false, "{body}");
    let body = rpc(
        &p,
        &bob,
        "move_page",
        json!({ "from": PAGE, "to": "markdown/skills/other.md" }),
    )
    .await;
    assert_eq!(body["result"]["structuredContent"]["ok"], false, "{body}");
    assert_eq!(description(&p).await, "How we ship.");
    // A page outside `markdown/skills/` that declares `kind: skill` is a skill page as well.
    let elsewhere = "markdown/notes/runbook.md";
    let as_skill = "---\nkind: skill\nid: runbook\ndescription: d\n---\n# runbook\n";
    let body = rpc(&p, &bob, "update_page", write(elsewhere, as_skill)).await;
    assert_eq!(body["result"]["structuredContent"]["ok"], false, "{body}");
    ok(
        &p,
        &tenant_admin(&p),
        "delete_page",
        json!({ "page_id": PAGE }),
    )
    .await;
    p.shutdown().await;
}

#[tokio::test]
async fn a_member_cannot_open_a_session_on_a_skill_page() {
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            live_crdt: true,
            write_acl: Some(WriteAclMode::Enforce),
            skill_admin_groups: Some(TENANT_ADMIN.to_owned()),
            ..Default::default()
        },
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("playbook", PLAYBOOK)
                .done(),
        ),
    })
    .await;
    let body = rpc(&p, &member(&p), "open_session", json!({ "page_id": PAGE })).await;
    assert_eq!(body["error"]["data"]["code"], "forbidden", "{body}");
    let opened = rpc(
        &p,
        &tenant_admin(&p),
        "open_session",
        json!({ "page_id": PAGE }),
    )
    .await;
    assert!(
        opened["result"]["structuredContent"]["session"].is_string(),
        "{opened}"
    );
    p.shutdown().await;
}

#[tokio::test]
async fn a_star_keeps_skill_pages_open_to_every_writer() {
    let p = gateway(Some("*")).await;
    ok(&p, &member(&p), "update_page", write(PAGE, &reworded())).await;
    let draft_id = draft(&p, &member(&p), PAGE, PLAYBOOK).await;
    ok(
        &p,
        &member(&p),
        "promote_draft",
        json!({ "draft_id": draft_id }),
    )
    .await;
    p.shutdown().await;
}

#[tokio::test]
async fn write_acl_off_leaves_skill_pages_ungated() {
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            write_acl: Some(WriteAclMode::Off),
            ..Default::default()
        },
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("playbook", PLAYBOOK)
                .done(),
        ),
    })
    .await;
    ok(&p, &member(&p), "update_page", write(PAGE, &reworded())).await;
    p.shutdown().await;
}
