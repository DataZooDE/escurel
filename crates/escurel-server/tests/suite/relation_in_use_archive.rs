//! The in-use guard also covers the archive that arrives as `update_page` with `archived: true`.
//! A direct write is judged against the saved corpus; a changeset member is judged once, as a batch,
//! by `promote_changeset`. Real gateway, real DuckDB, neutral skills.

use std::time::Duration;

use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const TENANT: &str = "stuttgart-ai";
const TICKET: &str = "---\nkind: skill\nid: ticket\ndescription: A work item.\nvisibility: public\n\
    fields:\n  - {name: parent, kind: link, to: note}\n---\n# ticket\n";
const N1: &str = "markdown/instances/note/n1.md";
const T1: &str = "markdown/instances/ticket/t1.md";

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
fn sha(s: &str) -> String {
    format!("{:x}", Sha256::digest(s.as_bytes()))
}

async fn start_with(note_extra: &str, extra_notes: usize) -> EscurelProcess {
    let mut b = FixtureBuilder::new()
        .tenant(TENANT)
        .skill("note", note_skill(note_extra).as_str())
        .skill("ticket", TICKET)
        .instance("note", "n1", note("n1", "").as_str())
        .instance("ticket", "t1", ticket("t1", LINKED).as_str());
    for i in 0..extra_notes {
        let id = format!("free{i}");
        b = b.instance("note", &id, note(&id, "").as_str());
    }
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides::default(),
        fixtures: Some(b.done()),
    })
    .await
}
async fn start(note_extra: &str) -> EscurelProcess {
    start_with(note_extra, 0).await
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
fn ok(v: &Value) {
    assert_eq!(v["ok"], json!(true), "{v}");
}
fn code(v: &Value) -> Option<&str> {
    v["issues"][0]["code"].as_str()
}
/// Whether the stored page carries `archived: true` (an archive through `update_page` keeps the
/// page indexed, flagged; `delete_page` retracts it).
async fn archived(p: &EscurelProcess, page: &str) -> bool {
    let e = call(p, "expand", json!({ "page_id": page, "raw": true })).await;
    e["content"]
        .as_str()
        .is_some_and(|c| c.contains("archived: true"))
}

#[tokio::test]
async fn archiving_through_update_page_is_refused_while_a_declared_link_points_at_it() {
    let p = start("").await;
    let r = call(
        &p,
        "update_page",
        json!({ "page_id": N1, "content": note("n1", "archived: true\n") }),
    )
    .await;
    assert_eq!(r["ok"], json!(false), "{r}");
    assert_eq!(code(&r), Some("relation_in_use"), "{r}");
    assert_eq!(
        r["referrers"],
        json!([{ "page_id": T1, "field": "parent" }])
    );
    assert!(!archived(&p, N1).await, "still live");

    // An ordinary edit of the same page is not an archive.
    ok(&call(
        &p,
        "update_page",
        json!({ "page_id": N1, "content": note("n1", "x: 1\n") }),
    )
    .await);

    // Release the link; the archive then lands.
    ok(&call(
        &p,
        "update_page",
        json!({ "page_id": T1, "content": ticket("t1", "") }),
    )
    .await);
    ok(&call(
        &p,
        "update_page",
        json!({ "page_id": N1, "content": note("n1", "archived: true\n") }),
    )
    .await);
    assert!(archived(&p, N1).await);
}

#[tokio::test]
async fn the_opt_out_and_an_unreferenced_page_archive_directly() {
    let p = start("relation_guard: false\n").await;
    ok(&call(
        &p,
        "update_page",
        json!({ "page_id": N1, "content": note("n1", "archived: true\n") }),
    )
    .await);
    let p = start_with("", 1).await;
    ok(&call(&p, "update_page", json!({ "page_id": "markdown/instances/note/free0.md", "content": note("free0", "archived: true\n") })).await);
}

#[tokio::test]
async fn a_page_that_is_already_archived_can_still_be_edited() {
    let p = start("relation_guard: false\n").await;
    ok(&call(
        &p,
        "update_page",
        json!({ "page_id": N1, "content": note("n1", "archived: true\n") }),
    )
    .await);
    // Switch the guard on while a referrer still holds the link to the archived page.
    ok(&call_as(
        &p,
        Role::Admin,
        "update_page",
        json!({ "page_id": "markdown/skills/note.md", "content": note_skill("") }),
    )
    .await);
    ok(&call(
        &p,
        "update_page",
        json!({ "page_id": N1, "content": note("n1", "archived: true\nx: 1\n") }),
    )
    .await);
}

#[tokio::test]
async fn a_pending_draft_is_not_a_referrer_for_a_direct_write() {
    let p = start_with("", 1).await;
    let free = "markdown/instances/note/free0.md";
    // t2 is only a draft: nothing saved links to free0.
    let d = call(&p, "create_draft", json!({ "target_page_id": "markdown/instances/ticket/t2.md", "content": ticket("t2", "parent: \"[[note::free0]]\"\n"), "base_sha256": "" })).await;
    ok(&d);
    ok(&call(
        &p,
        "update_page",
        json!({ "page_id": free, "content": note("free0", "archived: true\n") }),
    )
    .await);
}

async fn changeset_with(p: &EscurelProcess, members: &[(&str, String, String)]) -> String {
    let mut changeset: Option<String> = None;
    for (page, content, base) in members {
        let mut args =
            json!({ "target_page_id": page, "content": content, "base_sha256": sha(base) });
        match &changeset {
            Some(cs) => args["changeset_id"] = json!(cs),
            None => args["new_changeset"] = json!(true),
        }
        let d = call(p, "create_draft", args).await;
        ok(&d);
        changeset = d["draft"]["changeset_id"].as_str().map(str::to_owned);
    }
    changeset.unwrap()
}

#[tokio::test]
async fn a_changeset_archiving_target_and_referrer_lands_in_either_member_order() {
    for target_first in [true, false] {
        let p = start("").await;
        let target = (N1, note("n1", "archived: true\n"), note("n1", ""));
        let referrer = (T1, ticket("t1", "archived: true\n"), ticket("t1", LINKED));
        let members = if target_first {
            [target, referrer]
        } else {
            [referrer, target]
        };
        let cs = changeset_with(&p, &members).await;
        let r = call(&p, "promote_changeset", json!({ "changeset_id": cs })).await;
        ok(&r);
        assert!(archived(&p, N1).await, "target_first={target_first}");
        assert!(archived(&p, T1).await, "target_first={target_first}");
    }
}

#[tokio::test]
async fn a_changeset_archiving_a_target_that_stays_referenced_lands_nothing() {
    let p = start_with("", 1).await;
    let cs = changeset_with(
        &p,
        &[
            (
                "markdown/instances/note/free0.md",
                note("free0", "archived: true\n"),
                note("free0", ""),
            ),
            (N1, note("n1", "archived: true\n"), note("n1", "")),
        ],
    )
    .await;
    let r = call(&p, "promote_changeset", json!({ "changeset_id": cs })).await;
    assert_eq!(r["ok"], json!(false), "{r}");
    assert_eq!(code(&r), Some("relation_in_use"), "{r}");
    assert!(
        !archived(&p, N1).await && !archived(&p, "markdown/instances/note/free0.md").await,
        "nothing landed"
    );
}

#[tokio::test]
async fn a_single_draft_that_archives_is_refused_at_promotion_until_released() {
    let p = start("").await;
    let d = call(&p, "create_draft", json!({ "target_page_id": N1, "content": note("n1", "archived: true\n"), "base_sha256": sha(&note("n1", "")) })).await;
    ok(&d);
    let id = d["draft"]["draft_id"].as_str().unwrap().to_owned();
    let r = call_as(&p, Role::Admin, "promote_draft", json!({ "draft_id": id })).await;
    assert_eq!(code(&r), Some("relation_in_use"), "{r}");
    ok(&call(
        &p,
        "update_page",
        json!({ "page_id": T1, "content": ticket("t1", "") }),
    )
    .await);
    let r = call_as(&p, Role::Admin, "promote_draft", json!({ "draft_id": id })).await;
    ok(&r);
}

#[tokio::test]
async fn concurrent_archives_finish_and_each_is_judged_on_its_own() {
    let p = start_with("", 6).await;
    let mut calls = Vec::new();
    for i in 0..6 {
        let id = format!("free{i}");
        calls.push((
            format!("markdown/instances/note/{id}.md"),
            note(&id, "archived: true\n"),
            true,
        ));
    }
    calls.push((N1.to_owned(), note("n1", "archived: true\n"), false));
    calls.push((N1.to_owned(), note("n1", "archived: true\n"), false));
    let results = tokio::time::timeout(Duration::from_secs(60), async {
        futures_util::future::join_all(calls.iter().map(|(page, content, _)| {
            call(
                &p,
                "update_page",
                json!({ "page_id": page, "content": content }),
            )
        }))
        .await
    })
    .await
    .expect("no deadlock between the update gate and the guard");
    for ((_, _, allowed), r) in calls.iter().zip(&results) {
        if *allowed {
            ok(r);
        } else {
            assert_eq!(code(r), Some("relation_in_use"), "{r}");
        }
    }
}

#[tokio::test]
async fn a_frozen_page_answers_instance_frozen_before_relation_in_use() {
    let frozen_note = note_skill("freeze_when: {state: closed}\n");
    let fixtures = FixtureBuilder::new()
        .tenant(TENANT)
        .skill("note", frozen_note.as_str())
        .skill("ticket", TICKET)
        .instance("note", "n1", note("n1", "state: closed\n").as_str())
        .instance("ticket", "t1", ticket("t1", LINKED).as_str())
        .done();
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides::default(),
        fixtures: Some(fixtures),
    })
    .await;
    let r = call_as(
        &p,
        Role::Admin,
        "update_page",
        json!({ "page_id": N1, "content": note("n1", "state: closed\narchived: true\n") }),
    )
    .await;
    assert_eq!(r["issues"].as_array().map(Vec::len), Some(1), "{r}");
    assert_eq!(code(&r), Some("instance_frozen"), "{r}");
}

#[tokio::test]
async fn an_immutable_page_answers_instance_immutable_before_relation_in_use() {
    let imm = note_skill("immutable: true\n");
    let fixtures = FixtureBuilder::new()
        .tenant(TENANT)
        .skill("note", imm.as_str())
        .skill("ticket", TICKET)
        .instance("note", "n1", note("n1", "").as_str())
        .instance("ticket", "t1", ticket("t1", LINKED).as_str())
        .done();
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides::default(),
        fixtures: Some(fixtures),
    })
    .await;
    let r = call(
        &p,
        "update_page",
        json!({ "page_id": N1, "content": note("n1", "archived: true\n") }),
    )
    .await;
    assert_eq!(code(&r), Some("instance_immutable"), "{r}");
}
