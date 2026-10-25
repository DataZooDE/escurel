//! The generic in-use guard: archiving or renaming a page is refused while a
//! live instance holds a declared `kind: link` field pointing at it, unless
//! the same changeset releases it. Real gateway, real DuckDB, neutral skills.

use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const TENANT: &str = "stuttgart-ai";
const TICKET: &str = "---\nkind: skill\nid: ticket\ndescription: A work item.\nvisibility: public\n\
    fields:\n  - {name: parent, kind: link, to: note}\n---\n# ticket\n";

fn note_skill(extra: &str) -> String {
    format!(
        "---\nkind: skill\nid: note\ndescription: A note.\nvisibility: public\n{extra}---\n# note\n"
    )
}

fn note(id: &str, extra: &str) -> String {
    format!("---\nkind: instance\nskill: note\nid: {id}\n{extra}---\n# {id}\n")
}

fn ticket(id: &str, extra: &str) -> String {
    format!("---\nkind: instance\nskill: ticket\nid: {id}\n{extra}---\n# {id}\n")
}

const LINKED: &str = "parent: \"[[note::n1]]\"\n";
const N1: &str = "markdown/instances/note/n1.md";

fn sha(s: &str) -> String {
    format!("{:x}", Sha256::digest(s.as_bytes()))
}

async fn start(note_extra: &str) -> EscurelProcess {
    let fixtures = FixtureBuilder::new()
        .tenant(TENANT)
        .skill("note", note_skill(note_extra).as_str())
        .skill("ticket", TICKET)
        .instance("note", "n1", note("n1", "").as_str())
        .instance("ticket", "t1", ticket("t1", LINKED).as_str())
        .done();
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides::default(),
        fixtures: Some(fixtures),
    })
    .await
}

async fn call(p: &EscurelProcess, tool: &str, args: Value) -> Value {
    let token = p.mint_token(TENANT, Role::Agent);
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

fn code(v: &Value) -> Option<&str> {
    v["issues"][0]["code"].as_str()
}

#[tokio::test]
async fn delete_page_archive_is_refused_while_a_declared_link_points_at_it() {
    let p = start("").await;
    let r = call(&p, "delete_page", json!({ "page_id": N1 })).await;
    assert_eq!(r["ok"], json!(false), "{r}");
    assert_eq!(code(&r), Some("relation_in_use"), "{r}");
    assert_eq!(
        r["referrers"],
        json!([{ "page_id": "markdown/instances/ticket/t1.md", "field": "parent" }])
    );
    // Still live.
    let listed = call(&p, "list_instances", json!({ "skill_id": "note" })).await;
    assert_eq!(
        listed["instances"].as_array().map(Vec::len),
        Some(1),
        "{listed}"
    );

    // Release the link, then the archive goes through.
    let released = call(
        &p,
        "update_page",
        json!({ "page_id": "markdown/instances/ticket/t1.md", "content": ticket("t1", "") }),
    )
    .await;
    assert_eq!(released["ok"], json!(true), "{released}");
    let r = call(&p, "delete_page", json!({ "page_id": N1 })).await;
    assert_eq!(r["ok"], json!(true), "{r}");
}

#[tokio::test]
async fn move_page_rename_is_refused_while_a_declared_link_points_at_it() {
    let p = start("").await;
    let to = "markdown/instances/note/n2.md";
    let r = call(&p, "move_page", json!({ "from": N1, "to": to })).await;
    assert_eq!(code(&r), Some("relation_in_use"), "{r}");
}

#[tokio::test]
async fn the_target_skill_can_opt_out_of_the_archive_guard() {
    let p = start("relation_guard: false\n").await;
    let r = call(&p, "delete_page", json!({ "page_id": N1 })).await;
    assert_eq!(r["ok"], json!(true), "{r}");
}

#[tokio::test]
async fn an_undeclared_wikilink_does_not_block_archive() {
    let p = start("").await;
    // Break the declared link; leave only a body wikilink.
    let body = "---\nkind: instance\nskill: ticket\nid: t1\n---\n# t1\nSee [[note::n1]].\n";
    call(
        &p,
        "update_page",
        json!({ "page_id": "markdown/instances/ticket/t1.md", "content": body }),
    )
    .await;
    let r = call(&p, "delete_page", json!({ "page_id": N1 })).await;
    assert_eq!(r["ok"], json!(true), "{r}");
}

async fn changeset_with(p: &EscurelProcess, members: &[(&str, String, String)]) -> String {
    let mut changeset: Option<String> = None;
    for (page, content, base) in members {
        let mut args = json!({
            "target_page_id": page,
            "content": content,
            "base_sha256": sha(base),
        });
        match &changeset {
            Some(cs) => args["changeset_id"] = json!(cs),
            None => args["new_changeset"] = json!(true),
        }
        let d = call(p, "create_draft", args).await;
        assert_eq!(d["ok"], json!(true), "{d}");
        changeset = d["draft"]["changeset_id"].as_str().map(str::to_owned);
    }
    changeset.expect("changeset id")
}

#[tokio::test]
async fn changeset_archiving_a_target_and_its_referrer_lands() {
    let p = start("").await;
    let t1 = "markdown/instances/ticket/t1.md";
    let cs = changeset_with(
        &p,
        &[
            (N1, note("n1", "archived: true\n"), note("n1", "")),
            (t1, ticket("t1", "archived: true\n"), ticket("t1", LINKED)),
        ],
    )
    .await;
    let r = call(&p, "promote_changeset", json!({ "changeset_id": cs })).await;
    assert_eq!(r["ok"], json!(true), "{r}");
}

#[tokio::test]
async fn changeset_archiving_a_target_still_in_use_lands_nothing() {
    let p = start("").await;
    let cs = changeset_with(&p, &[(N1, note("n1", "archived: true\n"), note("n1", ""))]).await;
    let r = call(&p, "promote_changeset", json!({ "changeset_id": cs })).await;
    assert_eq!(r["ok"], json!(false), "{r}");
    assert_eq!(code(&r), Some("relation_in_use"), "{r}");
    let listed = call(&p, "list_instances", json!({ "skill_id": "note" })).await;
    assert_eq!(
        listed["instances"].as_array().map(Vec::len),
        Some(1),
        "{listed}"
    );
}

#[tokio::test]
async fn list_skills_exposes_every_target_of_a_link_field() {
    let multi = "---\nkind: skill\nid: multi\ndescription: Many targets.\nvisibility: public\n\
        fields:\n  - {name: about, kind: link, to: [note, ticket]}\n  - {name: one, kind: link, to: note}\n---\n# multi\n";
    let fixtures = FixtureBuilder::new()
        .tenant(TENANT)
        .skill("note", note_skill("").as_str())
        .skill("ticket", TICKET)
        .skill("multi", multi)
        .done();
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides::default(),
        fixtures: Some(fixtures),
    })
    .await;
    let r = call(&p, "list_skills", json!({})).await;
    let skill = r["skills"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "multi")
        .unwrap();
    let field = |n: &str| {
        skill["fields"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["name"] == n)
            .unwrap()
            .clone()
    };
    assert_eq!(field("about")["target_skills"], json!(["note", "ticket"]));
    assert_eq!(field("one")["target_skills"], json!(["note"]));
    assert_eq!(field("one")["target_skill"], json!("note"));
}
