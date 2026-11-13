//! Authority keys on a SKILL page (owner decision 2026-11-13): a write that
//! adds, removes or changes `acl`, `agent`, `autonomy`, `schedule`, … on a
//! skill page needs `escurel:admin`, or membership of the STORED page's
//! `acl.update` groups — never the incoming page's, or a draft could add its
//! author to the ACL and promote itself. `description` and the body stay free.
//!
//! Real gateway over HTTP; every write door a skill page has: `update_page`,
//! `promote_draft`, `delete_page`, `move_page`.

use escurel_server::WriteAclMode;
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const PAGE: &str = "markdown/skills/playbook.md";
/// A governed skill: the `ops` group may change its authority.
const PLAYBOOK: &str = "---\nkind: skill\nid: playbook\ndescription: How we ship.\n\
    acl:\n  update: [ops]\n---\n# playbook\n";
/// Owner-written notes: Bob may write a note he owns.
const NOTE: &str = "---\nkind: skill\nid: note\ndescription: A note.\nowner_field: owner\n\
    acl:\n  create: [owner]\n  update: [owner]\n---\n# note\n";
/// A page outside `markdown/skills/`, so only its `kind` says whether it is a skill.
const RUNBOOK: &str = "markdown/notes/runbook.md";
/// Bob's note, carrying a schedule and an autonomy as plain data.
const RUNBOOK_NOTE: &str = "---\nkind: instance\nskill: note\nid: runbook\nowner: member:bob\n\
    schedule: \"0 9 * * 1\"\nautonomy: auto\n---\n# runbook\n";
/// The same page as a skill: the schedule now runs it.
const RUNBOOK_SKILL: &str = "---\nkind: skill\nid: runbook\ndescription: d\nowner: member:bob\n\
    schedule: \"0 9 * * 1\"\nautonomy: auto\n---\n# runbook\n";

async fn gateway(mode: WriteAclMode) -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            write_acl: Some(mode),
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

/// The raw JSON-RPC answer: `result.structuredContent` or `error`.
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

/// The refusal names every key the write would change.
async fn refused(p: &EscurelProcess, token: &str, name: &str, args: Value, keys: &[&str]) {
    let body = rpc(p, token, name, args).await;
    let error = &body["error"];
    assert_eq!(error["code"], -32003, "{name} must be refused: {body}");
    assert_eq!(error["data"]["code"], "authority_keys_need_admin", "{body}");
    assert_eq!(error["data"]["keys"], json!(keys), "{body}");
    for key in keys {
        let message = error["message"].as_str().unwrap_or_default();
        assert!(message.contains(&format!("`{key}`")), "{message}");
    }
}

fn member(p: &EscurelProcess) -> String {
    p.mint_token_with_sub(TENANT, Role::Agent, "member:bob")
}

fn ops(p: &EscurelProcess) -> String {
    p.mint_token_with_groups(TENANT, "member:olga", &["ops"], false)
}

fn admin(p: &EscurelProcess) -> String {
    p.mint_token(TENANT, Role::Admin)
}

fn write(page_id: &str, content: &str) -> Value {
    json!({ "page_id": page_id, "content": content })
}

async fn stored(p: &EscurelProcess) -> String {
    let page = rpc(p, &admin(p), "expand", json!({ "page_id": PAGE })).await;
    serde_json::to_string(&page["result"]["structuredContent"]["frontmatter"]).unwrap()
}

#[tokio::test]
async fn a_member_edits_a_skills_description_and_body() {
    let p = gateway(WriteAclMode::Enforce).await;
    let reworded = PLAYBOOK
        .replace("How we ship.", "How we ship, carefully.")
        .replace("# playbook", "# playbook\n\nStep one.");
    ok(&p, &member(&p), "update_page", write(PAGE, &reworded)).await;
    p.shutdown().await;
}

#[tokio::test]
async fn a_member_cannot_bind_an_agent_by_promoting_their_own_draft() {
    let p = gateway(WriteAclMode::Enforce).await;
    let bob = member(&p);
    let bound = PLAYBOOK.replace("acl:", "agent: \"[[agent::deploy-bot]]\"\nacl:");
    let drafted = ok(
        &p,
        &bob,
        "create_draft",
        json!({ "target_page_id": PAGE, "content": bound }),
    )
    .await;
    let draft_id = drafted["draft"]["draft_id"].as_str().expect("draft id");
    refused(
        &p,
        &bob,
        "promote_draft",
        json!({ "draft_id": draft_id }),
        &["agent"],
    )
    .await;
    assert!(!stored(&p).await.contains("deploy-bot"), "nothing landed");
    // The same draft, promoted by an admin, lands: the PROMOTER is judged.
    ok(
        &p,
        &admin(&p),
        "promote_draft",
        json!({ "draft_id": draft_id }),
    )
    .await;
    assert!(stored(&p).await.contains("deploy-bot"));
    p.shutdown().await;
}

#[tokio::test]
async fn a_member_of_the_stored_acl_update_changes_the_schedule() {
    let p = gateway(WriteAclMode::Enforce).await;
    let scheduled = PLAYBOOK.replace("acl:", "schedule: \"0 9 * * 1\"\nacl:");
    ok(&p, &ops(&p), "update_page", write(PAGE, &scheduled)).await;
    assert!(stored(&p).await.contains("0 9 * * 1"));
    p.shutdown().await;
}

#[tokio::test]
async fn a_member_cannot_remove_the_acl() {
    let p = gateway(WriteAclMode::Enforce).await;
    let open = "---\nkind: skill\nid: playbook\ndescription: How we ship.\n---\n# playbook\n";
    refused(&p, &member(&p), "update_page", write(PAGE, open), &["acl"]).await;
    assert!(stored(&p).await.contains("ops"), "the ACL is still there");
    ok(&p, &admin(&p), "update_page", write(PAGE, open)).await;
    p.shutdown().await;
}

#[tokio::test]
async fn the_incoming_acl_does_not_authorise_its_own_write() {
    let p = gateway(WriteAclMode::Enforce).await;
    // Bob lists his own group in the new ACL: the STORED one is asked.
    let bob = p.mint_token_with_groups(TENANT, "member:bob", &["bobs"], false);
    let widened = PLAYBOOK.replace("[ops]", "[ops, bobs]");
    refused(&p, &bob, "update_page", write(PAGE, &widened), &["acl"]).await;
    p.shutdown().await;
}

#[tokio::test]
async fn a_new_skill_with_authority_keys_is_an_admins() {
    let p = gateway(WriteAclMode::Enforce).await;
    let page = "markdown/skills/mine.md";
    let mine =
        "---\nkind: skill\nid: mine\ndescription: m\nacl:\n  update: [public]\n---\n# mine\n";
    refused(&p, &ops(&p), "update_page", write(page, mine), &["acl"]).await;
    // Without authority keys a member may still author a skill.
    let plain = "---\nkind: skill\nid: mine\ndescription: m\n---\n# mine\n";
    ok(&p, &member(&p), "update_page", write(page, plain)).await;
    ok(&p, &admin(&p), "update_page", write(page, mine)).await;
    p.shutdown().await;
}

#[tokio::test]
async fn deleting_or_moving_a_governed_skill_needs_its_acl() {
    let p = gateway(WriteAclMode::Enforce).await;
    refused(
        &p,
        &member(&p),
        "delete_page",
        json!({ "page_id": PAGE }),
        &["acl"],
    )
    .await;
    refused(
        &p,
        &member(&p),
        "move_page",
        json!({ "from": PAGE, "to": "markdown/skills/elsewhere.md" }),
        &["acl"],
    )
    .await;
    assert!(stored(&p).await.contains("ops"), "still there");
    ok(&p, &ops(&p), "delete_page", json!({ "page_id": PAGE })).await;
    p.shutdown().await;
}

#[tokio::test]
async fn log_mode_logs_and_allows() {
    let p = gateway(WriteAclMode::Log).await;
    let open = "---\nkind: skill\nid: playbook\ndescription: How we ship.\n---\n# playbook\n";
    ok(&p, &member(&p), "update_page", write(PAGE, open)).await;
    p.shutdown().await;
}

/// An enforcing gateway with the CRDT backend: sessions, versions, auto-merge.
async fn crdt_gateway() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            live_crdt: true,
            write_acl: Some(WriteAclMode::Enforce),
            ..Default::default()
        },
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("playbook", PLAYBOOK)
                .done(),
        ),
    })
    .await
}

/// Bob opens a session on the playbook, cuts its `acl:` block and commits; the commit is refused.
/// The op stays in the CRDT head.
async fn refused_session_cutting_the_acl(p: &EscurelProcess) {
    let bob = member(p);
    let opened = rpc(p, &bob, "open_session", json!({ "page_id": PAGE })).await;
    let opened = &opened["result"]["structuredContent"];
    let session = opened["session"].as_str().expect("session id");
    // Edit the hydrated page the way a client would: cut the `acl:` block out of it.
    let op = {
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD;
        let doc = loro::LoroDoc::new();
        doc.import(&b64.decode(opened["snapshot"].as_str().unwrap()).unwrap())
            .unwrap();
        let before = doc.oplog_vv();
        let body = doc.get_text("body");
        let acl = "acl:\n  update: [ops]\n";
        let at = body.to_string().find(acl).expect("hydrated with the acl");
        body.delete(at, acl.len()).unwrap();
        doc.commit();
        b64.encode(doc.export(loro::ExportMode::updates(&before)).unwrap())
    };
    let applied = rpc(p, &bob, "apply_op", json!({ "session": session, "op": op })).await;
    assert!(applied.get("error").is_none(), "{applied}");
    refused(
        p,
        &bob,
        "close_session",
        json!({ "session": session, "commit": true }),
        &["acl"],
    )
    .await;
}

#[tokio::test]
async fn a_session_commit_cannot_drop_a_skills_acl() {
    let p = crdt_gateway().await;
    refused_session_cutting_the_acl(&p).await;
    assert!(stored(&p).await.contains("ops"), "the ACL is still there");
    p.shutdown().await;
}

#[tokio::test]
async fn a_refused_session_cannot_land_through_an_auto_merge() {
    let p = crdt_gateway().await;
    let v1 = PLAYBOOK.replace("# playbook\n", "# playbook\n\nv1.\n");
    let base = ok(&p, &admin(&p), "update_page", write(PAGE, &v1)).await["new_version"].clone();
    refused_session_cutting_the_acl(&p).await;
    // A body-only edit on the pre-session base: its own bytes keep the ACL, but the auto-merge
    // folds in the refused session's op, so what would persist drops it.
    let mut args = write(PAGE, &v1.replace("v1.", "v2."));
    args["base_version"] = base;
    refused(&p, &member(&p), "update_page", args, &["acl"]).await;
    assert!(stored(&p).await.contains("ops"), "the ACL is still there");
    p.shutdown().await;
}

#[tokio::test]
async fn a_members_stale_write_does_not_revert_an_admins_change() {
    let p = crdt_gateway().await;
    let v1 = PLAYBOOK.replace("# playbook\n", "# playbook\n\nv1.\n");
    let base = ok(&p, &admin(&p), "update_page", write(PAGE, &v1)).await["new_version"].clone();
    // The admin schedules it after Bob read v1.
    let scheduled = v1.replace("acl:", "schedule: \"0 9 * * 1\"\nacl:");
    ok(&p, &admin(&p), "update_page", write(PAGE, &scheduled)).await;
    // Bob's body edit on his stale base merges with the admin's schedule and keeps it.
    let mut args = write(PAGE, &v1.replace("v1.", "v2."));
    args["base_version"] = base;
    let merged = ok(&p, &member(&p), "update_page", args).await;
    assert_eq!(merged["auto_merged"], true, "{merged}");
    assert!(stored(&p).await.contains("0 9 * * 1"), "the schedule stays");
    // The same bytes with no base would overwrite the schedule away: judged against the head.
    refused(
        &p,
        &member(&p),
        "update_page",
        write(PAGE, &v1.replace("v1.", "v3.")),
        &["schedule"],
    )
    .await;
    assert!(stored(&p).await.contains("0 9 * * 1"), "the schedule stays");
    p.shutdown().await;
}

#[tokio::test]
async fn relabelling_an_instance_as_a_skill_is_a_create() {
    let p = gateway(WriteAclMode::Enforce).await;
    ok(&p, &admin(&p), "update_page", write(RUNBOOK, RUNBOOK_NOTE)).await;
    // Nothing "changed" key by key, but the schedule now runs a skill.
    refused(
        &p,
        &member(&p),
        "update_page",
        write(RUNBOOK, RUNBOOK_SKILL),
        &["autonomy", "schedule"],
    )
    .await;
    p.shutdown().await;
}

#[tokio::test]
async fn relabelling_a_skill_as_an_instance_removes_its_keys() {
    let p = gateway(WriteAclMode::Enforce).await;
    ok(&p, &admin(&p), "update_page", write(RUNBOOK, RUNBOOK_SKILL)).await;
    // Bob owns the note it would become, so the ordinary write ACL lets him write it.
    refused(
        &p,
        &member(&p),
        "update_page",
        write(RUNBOOK, RUNBOOK_NOTE),
        &["autonomy", "schedule"],
    )
    .await;
    p.shutdown().await;
}

#[tokio::test]
async fn a_machine_with_the_admin_role_never_writes_a_pack() {
    let p = gateway(WriteAclMode::Enforce).await;
    let run = p.mint_token_for_run(TENANT, Role::Admin, "agent:bot", "run-1", "evt-1");
    let packs = [
        (
            "import_pack",
            json!({ "tenant_id": TENANT, "manifest": {}, "tarball_b64": "" }),
        ),
        (
            "rebase_pack",
            json!({ "tenant_id": TENANT, "manifest": {}, "tarball_b64": "" }),
        ),
        (
            "unsubscribe_pack",
            json!({ "tenant_id": TENANT, "pack_id": "crm" }),
        ),
    ];
    for (tool, args) in packs {
        let body = rpc(&p, &run, tool, args.clone()).await;
        assert_eq!(body["error"]["code"], -32003, "{tool}: {body}");
        assert_eq!(body["error"]["data"]["code"], "forbidden", "{tool}: {body}");
        // A person with the same role gets past the caller check (and fails on the bogus pack).
        let person = rpc(&p, &admin(&p), tool, args).await;
        assert_ne!(person["error"]["code"], -32003, "{tool}: {person}");
    }
    p.shutdown().await;
}

#[tokio::test]
async fn a_member_cannot_unfreeze_a_skills_instances() {
    let p = gateway(WriteAclMode::Enforce).await;
    let frozen = PLAYBOOK.replace("acl:", "freeze_when: {status: [closed]}\nacl:");
    ok(&p, &admin(&p), "update_page", write(PAGE, &frozen)).await;
    refused(
        &p,
        &member(&p),
        "update_page",
        write(PAGE, PLAYBOOK),
        &["freeze_when"],
    )
    .await;
    p.shutdown().await;
}
