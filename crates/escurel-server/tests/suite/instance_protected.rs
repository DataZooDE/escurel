//! `protected: [delete, move]` on a skill: `delete_page` (archive) and `move_page` of its instances
//! are refused with `instance_protected`, whatever the role, while updates stay allowed. An archive
//! through `update_page`, a draft or a changeset member counts as `delete`. Real gateway, real
//! DuckDB, neutral skills.

use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const TENANT: &str = "stuttgart-ai";

fn skill(id: &str, extra: &str) -> String {
    format!(
        "---\nkind: skill\nid: {id}\ndescription: A {id}.\nvisibility: public\n{extra}---\n# {id}\n"
    )
}
fn page(skill: &str, id: &str, extra: &str) -> String {
    format!("---\nkind: instance\nskill: {skill}\nid: {id}\n{extra}---\n# {id}\n")
}
fn pid(skill: &str, id: &str) -> String {
    format!("markdown/instances/{skill}/{id}.md")
}
fn sha(s: &str) -> String {
    format!("{:x}", Sha256::digest(s.as_bytes()))
}

async fn start() -> EscurelProcess {
    let both = skill("keep", "protected: [delete, move]\n");
    let del = skill("noarchive", "protected: [delete]\n");
    let mv = skill("pinned", "protected: [move]\n");
    let plain = skill("note", "");
    let imm = skill("entry", "immutable: true\nprotected: [delete]\n");
    let frz = skill(
        "closing",
        "freeze_when: {state: closed}\nprotected: [delete, move]\n",
    );
    let tgt = skill("target", "protected: [delete, move]\n");
    let refs = "---\nkind: skill\nid: refers\ndescription: x\nvisibility: public\n\
        fields:\n  - {name: to, kind: link, to: target}\n---\n# refers\n";
    let mut b = FixtureBuilder::new()
        .tenant(TENANT)
        .skill("keep", both.as_str())
        .skill("noarchive", del.as_str())
        .skill("pinned", mv.as_str())
        .skill("note", plain.as_str())
        .skill("entry", imm.as_str())
        .skill("closing", frz.as_str())
        .skill("target", tgt.as_str())
        .skill("refers", refs);
    for (s, ids) in [
        ("keep", vec!["k1"]),
        ("noarchive", vec!["a1", "a2"]),
        ("pinned", vec!["p1"]),
        ("note", vec!["n1"]),
        ("entry", vec!["e1"]),
    ] {
        for id in ids {
            b = b.instance(s, id, page(s, id, "").as_str());
        }
    }
    b = b
        .instance(
            "closing",
            "c1",
            page("closing", "c1", "state: closed\n").as_str(),
        )
        .instance("target", "t1", page("target", "t1", "").as_str())
        .instance(
            "refers",
            "r1",
            page("refers", "r1", "to: \"[[target::t1]]\"\n").as_str(),
        );
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides::default(),
        fixtures: Some(b.done()),
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

fn refused(v: &Value, code: &str) {
    assert_eq!(v["ok"], json!(false), "{v}");
    assert_eq!(v["issues"][0]["code"], json!(code), "{v}");
    assert_eq!(v["issues"][0]["family"], json!(code), "{v}");
    assert_eq!(v["issues"].as_array().map(Vec::len), Some(1), "{v}");
}
fn ok(v: &Value) {
    assert_eq!(v["ok"], json!(true), "{v}");
}

async fn live(p: &EscurelProcess, skill: &str) -> usize {
    let l = call(p, "list_instances", json!({ "skill_id": skill })).await;
    l["instances"].as_array().map_or(0, Vec::len)
}

#[tokio::test]
async fn delete_is_refused_for_every_role_and_updates_stay_allowed() {
    let p = start().await;
    for role in [Role::Agent, Role::Admin] {
        let r = call_as(
            &p,
            role,
            "delete_page",
            json!({ "page_id": pid("noarchive", "a1") }),
        )
        .await;
        refused(&r, "instance_protected");
    }
    assert_eq!(live(&p, "noarchive").await, 2);
    // Updates stay allowed.
    ok(&call(
        &p,
        "update_page",
        json!({ "page_id": pid("noarchive", "a1"), "content": page("noarchive", "a1", "x: 1\n") }),
    )
    .await);
    // `protected: [delete]` leaves `move` alone.
    ok(&call(
        &p,
        "move_page",
        json!({ "from": pid("noarchive", "a2"), "to": pid("noarchive", "a3") }),
    )
    .await);
}

#[tokio::test]
async fn move_is_refused_when_protected_and_delete_alone_is_not() {
    let p = start().await;
    let r = call_as(
        &p,
        Role::Admin,
        "move_page",
        json!({ "from": pid("pinned", "p1"), "to": pid("pinned", "p2") }),
    )
    .await;
    refused(&r, "instance_protected");
    assert_eq!(live(&p, "pinned").await, 1);
    // `protected: [move]` leaves `delete` alone.
    ok(&call(&p, "delete_page", json!({ "page_id": pid("pinned", "p1") })).await);
    // A skill that protects both refuses both.
    refused(
        &call(&p, "delete_page", json!({ "page_id": pid("keep", "k1") })).await,
        "instance_protected",
    );
    refused(
        &call(
            &p,
            "move_page",
            json!({ "from": pid("keep", "k1"), "to": pid("keep", "k2") }),
        )
        .await,
        "instance_protected",
    );
    // An unprotected skill is unaffected.
    ok(&call(&p, "delete_page", json!({ "page_id": pid("note", "n1") })).await);
}

#[tokio::test]
async fn archiving_through_update_page_counts_as_delete() {
    let p = start().await;
    let r = call(&p, "update_page", json!({ "page_id": pid("noarchive", "a1"), "content": page("noarchive", "a1", "archived: true\n") })).await;
    refused(&r, "instance_protected");
    assert_eq!(live(&p, "noarchive").await, 2);
    // A skill that protects only `move` may be archived this way.
    ok(&call(&p, "update_page", json!({ "page_id": pid("pinned", "p1"), "content": page("pinned", "p1", "archived: true\n") })).await);
    // Creating a page that is born archived is not an archive of an existing page.
    ok(&call(&p, "update_page", json!({ "page_id": pid("noarchive", "born"), "content": page("noarchive", "born", "archived: true\n") })).await);
}

#[tokio::test]
async fn a_draft_and_a_changeset_member_that_archive_are_refused() {
    let p = start().await;
    let base = sha(&page("noarchive", "a1", ""));
    let d = call(&p, "create_draft", json!({ "target_page_id": pid("noarchive", "a1"), "content": page("noarchive", "a1", "archived: true\n"), "base_sha256": base })).await;
    refused(&d, "instance_protected");
    let queue = call(&p, "list_drafts", json!({})).await;
    assert_eq!(queue["drafts"].as_array().map(Vec::len), Some(0), "{queue}");

    let first = call(&p, "create_draft", json!({ "target_page_id": pid("note", "n1"), "content": page("note", "n1", "archived: true\n"), "base_sha256": sha(&page("note", "n1", "")), "new_changeset": true })).await;
    ok(&first);
    let cs = first["draft"]["changeset_id"].as_str().unwrap().to_owned();
    let second = call(&p, "create_draft", json!({ "target_page_id": pid("noarchive", "a1"), "content": page("noarchive", "a1", "archived: true\n"), "base_sha256": base, "changeset_id": cs })).await;
    refused(&second, "instance_protected");
    // An update draft is fine.
    let upd = call(&p, "create_draft", json!({ "target_page_id": pid("noarchive", "a1"), "content": page("noarchive", "a1", "x: 2\n"), "base_sha256": base })).await;
    ok(&upd);
}

#[tokio::test]
async fn a_changeset_drafted_before_the_skill_became_protected_lands_nothing() {
    let p = start().await;
    let mut changeset: Option<String> = None;
    for (id, s) in [("n1", "note"), ("a1", "noarchive")] {
        // The skill is unprotected while the draft is taken.
        let relaxed = skill("noarchive", "");
        call_as(
            &p,
            Role::Admin,
            "update_page",
            json!({ "page_id": "markdown/skills/noarchive.md", "content": relaxed }),
        )
        .await;
        let mut args = json!({ "target_page_id": pid(s, id), "content": page(s, id, "archived: true\n"), "base_sha256": sha(&page(s, id, "")) });
        match &changeset {
            Some(cs) => args["changeset_id"] = json!(cs),
            None => args["new_changeset"] = json!(true),
        }
        let d = call(&p, "create_draft", args).await;
        ok(&d);
        changeset = d["draft"]["changeset_id"].as_str().map(str::to_owned);
    }
    ok(&call_as(&p, Role::Admin, "update_page", json!({ "page_id": "markdown/skills/noarchive.md", "content": skill("noarchive", "protected: [delete]\n") })).await);
    let r = call_as(
        &p,
        Role::Admin,
        "promote_changeset",
        json!({ "changeset_id": changeset.unwrap() }),
    )
    .await;
    assert_eq!(r["ok"], json!(false), "{r}");
    assert!(
        r["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["code"] == "instance_protected"),
        "{r}"
    );
    assert_eq!(live(&p, "note").await, 1);
    assert_eq!(live(&p, "noarchive").await, 2);
}

#[tokio::test]
async fn malformed_protected_declarations_are_linted() {
    let p = start().await;
    for bad in [
        "protected: delete\n",
        "protected: [purge]\n",
        "protected: []\n",
        "protected: [delete, 3]\n",
    ] {
        let content = skill("lint", bad);
        let v = call_as(
            &p,
            Role::Admin,
            "validate",
            json!({ "as_page_id": "markdown/skills/lint.md", "content": content }),
        )
        .await;
        assert!(
            v["issues"]
                .as_array()
                .unwrap()
                .iter()
                .any(|i| i["code"] == "skill_rule_malformed"),
            "{bad}: {v}"
        );
        let w = call_as(
            &p,
            Role::Admin,
            "update_page",
            json!({ "page_id": "markdown/skills/lint.md", "content": content }),
        )
        .await;
        assert_eq!(w["ok"], json!(false), "{w}");
    }
    ok(&call_as(&p, Role::Admin, "update_page", json!({ "page_id": "markdown/skills/lint.md", "content": skill("lint", "protected: [move, delete]\n") })).await);
}

#[tokio::test]
async fn list_skills_publishes_the_protected_actions() {
    let p = start().await;
    let r = call(&p, "list_skills", json!({})).await;
    let get = |id: &str| {
        r["skills"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"] == id)
            .unwrap()
            .clone()
    };
    assert_eq!(get("keep")["protected"], json!(["delete", "move"]));
    assert_eq!(get("pinned")["protected"], json!(["move"]));
    assert!(get("note").get("protected").is_none());
}

#[tokio::test]
async fn the_order_of_refusals_is_frozen_immutable_protected_in_use() {
    let p = start().await;
    // frozen before protected
    let r = call_as(
        &p,
        Role::Admin,
        "delete_page",
        json!({ "page_id": pid("closing", "c1") }),
    )
    .await;
    refused(&r, "instance_frozen");
    // immutable before protected
    let r = call_as(
        &p,
        Role::Admin,
        "delete_page",
        json!({ "page_id": pid("entry", "e1") }),
    )
    .await;
    refused(&r, "instance_immutable");
    // protected before relation_in_use (the target is referenced by r1)
    let r = call_as(
        &p,
        Role::Admin,
        "delete_page",
        json!({ "page_id": pid("target", "t1") }),
    )
    .await;
    refused(&r, "instance_protected");
    let r = call_as(
        &p,
        Role::Admin,
        "move_page",
        json!({ "from": pid("target", "t1"), "to": pid("target", "t2") }),
    )
    .await;
    refused(&r, "instance_protected");
}

#[tokio::test]
async fn the_operator_credential_is_not_exempt() {
    let p = start().await;
    let token = p.mint_operator_token(TENANT);
    let body: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "delete_page", "arguments": { "page_id": pid("keep", "k1") } } }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    refused(&body["result"]["structuredContent"], "instance_protected");
}
