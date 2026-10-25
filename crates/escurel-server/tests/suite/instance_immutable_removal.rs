//! `immutable: true` on a skill also protects its instances from removal:
//! `delete_page` (archive) and `move_page` (rename) are refused with
//! `instance_immutable`, and so are archives that arrive through
//! `update_page`, a draft or a changeset. Real gateway, neutral skills.

use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const TENANT: &str = "stuttgart-ai";
const LOG: &str = "---\nkind: skill\nid: logentry\ndescription: An append-only record.\n\
    visibility: public\nimmutable: true\n---\n# logentry\n";
const NOTE: &str =
    "---\nkind: skill\nid: note\ndescription: A note.\nvisibility: public\n---\n# note\n";
const E1: &str = "markdown/instances/logentry/e1.md";
const N1: &str = "markdown/instances/note/n1.md";

fn entry(extra: &str) -> String {
    format!("---\nkind: instance\nskill: logentry\nid: e1\n{extra}---\n# e1\n")
}

fn note(extra: &str) -> String {
    format!("---\nkind: instance\nskill: note\nid: n1\n{extra}---\n# n1\n")
}

fn sha(s: &str) -> String {
    format!("{:x}", Sha256::digest(s.as_bytes()))
}

async fn start() -> EscurelProcess {
    let fixtures = FixtureBuilder::new()
        .tenant(TENANT)
        .skill("logentry", LOG)
        .skill("note", NOTE)
        .instance("logentry", "e1", entry("").as_str())
        .instance("note", "n1", note("").as_str())
        .done();
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides::default(),
        fixtures: Some(fixtures),
    })
    .await
}

async fn call_as(p: &EscurelProcess, role: Role, tool: &str, args: Value) -> Value {
    let token = p.mint_token(TENANT, role);
    let body: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": tool, "arguments": args } }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(body.get("error").is_none(), "{tool} error: {body}");
    body["result"]["structuredContent"].clone()
}

async fn call(p: &EscurelProcess, tool: &str, args: Value) -> Value {
    call_as(p, Role::Agent, tool, args).await
}

fn refused(v: &Value) {
    assert_eq!(v["ok"], json!(false), "{v}");
    assert_eq!(v["issues"][0]["code"], json!("instance_immutable"), "{v}");
    assert_eq!(v["issues"][0]["family"], json!("instance_immutable"), "{v}");
}

async fn still_there(p: &EscurelProcess, page: &str, skill: &str) {
    let listed = call(p, "list_instances", json!({ "skill_id": skill })).await;
    assert_eq!(
        listed["instances"].as_array().map(Vec::len),
        Some(1),
        "{listed}"
    );
    let expanded = call(p, "expand", json!({ "page_id": page })).await;
    assert!(expanded.get("content_sha256").is_some(), "{expanded}");
}

#[tokio::test]
async fn delete_page_of_an_immutable_instance_is_refused_for_every_role() {
    let p = start().await;
    for role in [Role::Agent, Role::Admin] {
        let r = call_as(&p, role, "delete_page", json!({ "page_id": E1 })).await;
        refused(&r);
    }
    still_there(&p, E1, "logentry").await;
}

#[tokio::test]
async fn move_page_of_an_immutable_instance_is_refused() {
    let p = start().await;
    let r = call(
        &p,
        "move_page",
        json!({ "from": E1, "to": "markdown/instances/logentry/e2.md" }),
    )
    .await;
    refused(&r);
    still_there(&p, E1, "logentry").await;
}

#[tokio::test]
async fn archiving_through_update_page_is_refused() {
    let p = start().await;
    let r = call(
        &p,
        "update_page",
        json!({ "page_id": E1, "content": entry("archived: true\n") }),
    )
    .await;
    refused(&r);
    still_there(&p, E1, "logentry").await;
}

#[tokio::test]
async fn a_mutable_skill_is_not_affected() {
    let p = start().await;
    let r = call(&p, "delete_page", json!({ "page_id": N1 })).await;
    assert_eq!(r["ok"], json!(true), "{r}");
}

#[tokio::test]
async fn a_draft_that_archives_an_immutable_instance_is_refused_and_not_queued() {
    let p = start().await;
    let d = call(
        &p,
        "create_draft",
        json!({ "target_page_id": E1, "content": entry("archived: true\n"),
                "base_sha256": sha(&entry("")) }),
    )
    .await;
    refused(&d);
    let queue = call(&p, "list_drafts", json!({})).await;
    assert_eq!(queue["drafts"].as_array().map(Vec::len), Some(0), "{queue}");
    still_there(&p, E1, "logentry").await;
}

#[tokio::test]
async fn a_changeset_cannot_carry_an_archive_of_an_immutable_instance() {
    let p = start().await;
    let first = call(
        &p,
        "create_draft",
        json!({ "target_page_id": N1, "content": note("archived: true\n"),
                "base_sha256": sha(&note("")), "new_changeset": true }),
    )
    .await;
    assert_eq!(first["ok"], json!(true), "{first}");
    let cs = first["draft"]["changeset_id"]
        .as_str()
        .expect("changeset")
        .to_owned();
    let second = call(
        &p,
        "create_draft",
        json!({ "target_page_id": E1, "content": entry("archived: true\n"),
                "base_sha256": sha(&entry("")), "changeset_id": cs }),
    )
    .await;
    refused(&second);
    // The refused member is not in the changeset; promoting what is there
    // never touches the immutable page.
    let r = call_as(
        &p,
        Role::Admin,
        "promote_changeset",
        json!({ "changeset_id": cs }),
    )
    .await;
    assert_eq!(r["ok"], json!(true), "{r}");
    still_there(&p, E1, "logentry").await;
}

#[tokio::test]
async fn a_changeset_drafted_before_the_skill_became_immutable_lands_nothing() {
    let p = start().await;
    let mutable_log = LOG.replace("immutable: true\n", "");
    let upd = call_as(
        &p,
        Role::Admin,
        "update_page",
        json!({ "page_id": "markdown/skills/logentry.md", "content": mutable_log }),
    )
    .await;
    assert_eq!(upd["ok"], json!(true), "{upd}");
    let mut changeset: Option<String> = None;
    for (page, content, base) in [
        (N1, note("archived: true\n"), note("")),
        (E1, entry("archived: true\n"), entry("")),
    ] {
        let mut args = json!({ "target_page_id": page, "content": content,
            "base_sha256": sha(&base) });
        match &changeset {
            Some(cs) => args["changeset_id"] = json!(cs),
            None => args["new_changeset"] = json!(true),
        }
        let d = call(&p, "create_draft", args).await;
        assert_eq!(d["ok"], json!(true), "{d}");
        changeset = d["draft"]["changeset_id"].as_str().map(str::to_owned);
    }
    let upd = call_as(
        &p,
        Role::Admin,
        "update_page",
        json!({ "page_id": "markdown/skills/logentry.md", "content": LOG }),
    )
    .await;
    assert_eq!(upd["ok"], json!(true), "{upd}");
    let r = call_as(
        &p,
        Role::Admin,
        "promote_changeset",
        json!({ "changeset_id": changeset.expect("changeset") }),
    )
    .await;
    assert_eq!(r["ok"], json!(false), "{r}");
    assert!(
        r["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["code"] == json!("instance_immutable")),
        "{r}"
    );
    still_there(&p, N1, "note").await;
    still_there(&p, E1, "logentry").await;
    let n1 = call(&p, "expand", json!({ "page_id": N1 })).await;
    assert!(!n1.to_string().contains("archived: true"), "{n1}");
}
