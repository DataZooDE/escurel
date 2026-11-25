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

/// Another member and another tenant administrator: the reviewers of the first ones' proposals (nobody
/// accepts their own).
fn other_member(p: &EscurelProcess) -> String {
    p.mint_token_with_sub(TENANT, Role::Agent, "member:ben")
}

fn other_tenant_admin(p: &EscurelProcess) -> String {
    p.mint_token_with_groups(TENANT, "member:tina", &[TENANT_ADMIN], false)
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
        &other_tenant_admin(&p),
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
    // Bob does not accept his own draft; another member may not write his note; an admin may.
    let own = rpc(
        &p,
        &member(&p),
        "promote_draft",
        json!({ "draft_id": draft_id }),
    )
    .await;
    assert_eq!(
        own["result"]["structuredContent"]["issues"][0]["code"], "self_review",
        "{own}"
    );
    let other = rpc(
        &p,
        &other_member(&p),
        "promote_draft",
        json!({ "draft_id": draft_id }),
    )
    .await;
    assert_ne!(other["result"]["structuredContent"]["ok"], true, "{other}");
    ok(
        &p,
        &admin(&p),
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
        &other_member(&p),
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

/// A new skill a release adds: it sets authority keys (`acl`, `autonomy`) and links to another
/// new skill of the same changeset.
fn new_skill(id: &str, extra: &str) -> String {
    format!(
        "---\nkind: skill\nid: {id}\ndescription: A skill a release adds.\nautonomy: review\n\
         acl:\n  read: [public]\n  update: [{TENANT_ADMIN}]\nfields:\n  - {{name: name, kind: string}}\n\
         {extra}---\n# {id}\n"
    )
}

async fn create_draft_in(
    p: &EscurelProcess,
    token: &str,
    target: &str,
    content: &str,
    changeset: Option<&str>,
) -> Value {
    let mut args = json!({ "target_page_id": target, "content": content });
    match changeset {
        Some(id) => args["changeset_id"] = json!(id),
        None => args["new_changeset"] = json!(true),
    }
    if let Some(base) = stored(p, target).await["content_sha256"].as_str() {
        args["base_sha256"] = json!(base);
    }
    ok(p, token, "create_draft", args).await["draft"].clone()
}

/// A skill release that ADDS skills is accepted by a member of `ESCUREL_SKILL_ADMIN_GROUPS`: the
/// group is who administers skill pages, so it also sets the authority keys a new skill carries
/// (a new page has no stored `acl.update` to grant them). Before, every create was refused with
/// `authority_keys_need_admin` while the changes of existing skills landed.
#[tokio::test]
async fn a_configured_group_lands_a_release_that_adds_skills() {
    let p = gateway(Some(TENANT_ADMIN)).await;
    let publisher = admin(&p);
    let tia = tenant_admin(&p);
    // As a changeset: one change of an existing skill and two new skills, the first linking to the second.
    let first = create_draft_in(&p, &publisher, PAGE, &reworded(), None).await;
    let changeset = first["changeset_id"]
        .as_str()
        .expect("changeset")
        .to_owned();
    let child = "markdown/skills/stage.md";
    let parent = "markdown/skills/value-stream.md";
    create_draft_in(
        &p,
        &publisher,
        child,
        &new_skill(
            "stage",
            "  - {name: value_stream, kind: link, to: value-stream}\n",
        ),
        Some(&changeset),
    )
    .await;
    create_draft_in(
        &p,
        &publisher,
        parent,
        &new_skill("value-stream", ""),
        Some(&changeset),
    )
    .await;
    // A member is refused the whole changeset, by the skill-page rule.
    refused(
        &p,
        &member(&p),
        "promote_changeset",
        json!({ "changeset_id": changeset }),
    )
    .await;
    assert!(stored(&p, parent).await["frontmatter"].is_null());
    ok(
        &p,
        &tia,
        "promote_changeset",
        json!({ "changeset_id": changeset }),
    )
    .await;
    assert_eq!(description(&p).await, "How we ship, carefully.");
    assert_eq!(stored(&p, child).await["frontmatter"]["id"], "stage");
    assert_eq!(
        stored(&p, parent).await["frontmatter"]["id"],
        "value-stream"
    );

    // Draft by draft, as a client that promotes each member on its own does.
    let single = new_skill("discovery-row", "");
    let draft = create_draft_in(
        &p,
        &publisher,
        "markdown/skills/discovery-row.md",
        &single,
        None,
    )
    .await;
    ok(
        &p,
        &tia,
        "promote_draft",
        json!({ "draft_id": draft["draft_id"] }),
    )
    .await;
    assert_eq!(
        stored(&p, "markdown/skills/discovery-row.md").await["frontmatter"]["id"],
        "discovery-row"
    );

    // The group also changes an authority key on an existing skill whose `acl.update` does not name it.
    let with_autonomy = reworded().replace("description:", "autonomy: confirm\ndescription:");
    ok(&p, &tia, "update_page", write(PAGE, &with_autonomy)).await;
    p.shutdown().await;
}

/// Under `*` every writer may write a skill page, but the authority keys a new skill sets stay with
/// `escurel:admin`: `*` names no administrators. (A run token is never granted by the group:
/// `a_run_token_is_never_granted_by_the_group`.)
#[tokio::test]
async fn under_a_star_a_member_still_may_not_set_authority_keys() {
    let p = gateway(Some("*")).await;
    let body = rpc(
        &p,
        &member(&p),
        "update_page",
        write("markdown/skills/stage.md", &new_skill("stage", "")),
    )
    .await;
    assert_eq!(
        body["error"]["data"]["code"], "authority_keys_need_admin",
        "{body}"
    );
    p.shutdown().await;
}
