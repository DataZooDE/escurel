//! `freeze_when` on a skill, `frozen_by_parent` on a link field: while the stored page's field holds
//! a declared value, the page (and, through `frozen_by_parent`, every instance linking to a frozen
//! page) refuses `update_page`, `delete_page`, `move_page`, draft creation and changeset members with
//! `instance_frozen`, for every role. Real gateway, real DuckDB, neutral skills.

use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const TENANT: &str = "stuttgart-ai";

const EPIC: &str = "---\nkind: skill\nid: epic\ndescription: A body of work.\nvisibility: public\n\
    freeze_when: {status: [closed, cancelled]}\n---\n# epic\n";
const TICKET: &str = "---\nkind: skill\nid: ticket\ndescription: A work item.\nvisibility: public\n\
    fields:\n  - {name: epic, kind: link, to: epic, frozen_by_parent: true}\n---\n# ticket\n";
const TASK: &str = "---\nkind: skill\nid: task\ndescription: A unit of a ticket.\nvisibility: public\n\
    fields:\n  - {name: ticket, kind: link, to: ticket, frozen_by_parent: true}\n---\n# task\n";
const NODE: &str = "---\nkind: skill\nid: node\ndescription: A tree node.\nvisibility: public\n\
    freeze_when: {state: frozen}\n\
    fields:\n  - {name: parent, kind: link, to: node, frozen_by_parent: true}\n---\n# node\n";
const NOTE: &str =
    "---\nkind: skill\nid: note\ndescription: A note.\nvisibility: public\n---\n# note\n";

fn epic(id: &str, status: &str) -> String {
    format!("---\nkind: instance\nskill: epic\nid: {id}\nstatus: {status}\n---\n# {id}\n")
}
fn epic_x(id: &str, status: &str) -> String {
    format!("---\nkind: instance\nskill: epic\nid: {id}\nstatus: {status}\nx: 1\n---\n# {id}\n")
}
fn ticket(id: &str, epic: &str) -> String {
    format!(
        "---\nkind: instance\nskill: ticket\nid: {id}\nepic: \"[[epic::{epic}]]\"\n---\n# {id}\n"
    )
}
fn task(id: &str, ticket: &str) -> String {
    format!(
        "---\nkind: instance\nskill: task\nid: {id}\nticket: \"[[ticket::{ticket}]]\"\n---\n# {id}\n"
    )
}
fn node(id: &str, parent: Option<&str>, state: &str) -> String {
    let link = parent.map_or(String::new(), |p| format!("parent: \"[[node::{p}]]\"\n"));
    format!("---\nkind: instance\nskill: node\nid: {id}\nstate: {state}\n{link}---\n# {id}\n")
}
fn note(id: &str) -> String {
    format!("---\nkind: instance\nskill: note\nid: {id}\n---\n# {id}\n")
}
fn pid(skill: &str, id: &str) -> String {
    format!("markdown/instances/{skill}/{id}.md")
}
fn sha(s: &str) -> String {
    format!("{:x}", Sha256::digest(s.as_bytes()))
}

async fn start() -> EscurelProcess {
    let mut b = FixtureBuilder::new()
        .tenant(TENANT)
        .skill("epic", EPIC)
        .skill("ticket", TICKET)
        .skill("task", TASK)
        .skill("node", NODE)
        .skill("note", NOTE)
        .instance("epic", "e-open", epic("e-open", "open").as_str())
        .instance("epic", "e-closed", epic("e-closed", "closed").as_str())
        .instance("ticket", "t-open", ticket("t-open", "e-open").as_str())
        .instance(
            "ticket",
            "t-closed",
            ticket("t-closed", "e-closed").as_str(),
        )
        .instance("task", "k-closed", task("k-closed", "t-closed").as_str())
        .instance("task", "k-open", task("k-open", "t-open").as_str())
        .instance("note", "n1", note("n1").as_str())
        .instance("note", "n2", note("n2").as_str());
    // A chain n0 (frozen) <- n1 <- ... <- n6, and a two-node cycle that is not frozen.
    b = b.instance("node", "n0", node("n0", None, "frozen").as_str());
    for i in 1..=6 {
        let (id, parent) = (format!("n{i}"), format!("n{}", i - 1));
        b = b.instance("node", &id, node(&id, Some(&parent), "live").as_str());
    }
    b = b
        .instance("node", "ca", node("ca", Some("cb"), "live").as_str())
        .instance("node", "cb", node("cb", Some("ca"), "live").as_str());
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides::default(),
        fixtures: Some(b.done()),
    })
    .await
}

async fn call_with(p: &EscurelProcess, token: &str, tool: &str, args: Value) -> Value {
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
async fn call_as(p: &EscurelProcess, role: Role, tool: &str, args: Value) -> Value {
    call_with(p, &p.mint_token(TENANT, role), tool, args).await
}
async fn call(p: &EscurelProcess, tool: &str, args: Value) -> Value {
    call_as(p, Role::Agent, tool, args).await
}

fn codes(v: &Value) -> Vec<String> {
    v["issues"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|i| i["code"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}
fn frozen(v: &Value) {
    assert_eq!(v["ok"], json!(false), "{v}");
    assert_eq!(v["issues"][0]["code"], json!("instance_frozen"), "{v}");
    assert_eq!(v["issues"][0]["family"], json!("instance_frozen"), "{v}");
}
fn ok(v: &Value) {
    assert_eq!(v["ok"], json!(true), "{v}");
}

async fn stored(p: &EscurelProcess, page: &str) -> String {
    let e = call(p, "expand", json!({ "page_id": page, "raw": true })).await;
    e["content"].as_str().unwrap_or_default().to_owned()
}

#[tokio::test]
async fn a_frozen_page_refuses_update_for_every_role() {
    let p = start().await;
    let page = pid("epic", "e-closed");
    let op = p.mint_operator_token(TENANT);
    let content = format!("{}\nmore\n", epic("e-closed", "closed"));
    let before = stored(&p, &page).await;
    for role in [Role::Agent, Role::Admin] {
        let r = call_as(
            &p,
            role,
            "update_page",
            json!({ "page_id": page, "content": content }),
        )
        .await;
        frozen(&r);
    }
    frozen(
        &call_with(
            &p,
            &op,
            "update_page",
            json!({ "page_id": page, "content": content }),
        )
        .await,
    );
    assert_eq!(stored(&p, &page).await, before);
    // Control: the same edit on an open epic lands.
    let open = pid("epic", "e-open");
    ok(&call(
        &p,
        "update_page",
        json!({ "page_id": open, "content": format!("{}\nmore\n", epic("e-open", "open")) }),
    )
    .await);
}

#[tokio::test]
async fn a_frozen_page_refuses_delete_move_and_archive() {
    let p = start().await;
    let page = pid("epic", "e-closed");
    for role in [Role::Agent, Role::Admin] {
        frozen(&call_as(&p, role, "delete_page", json!({ "page_id": page })).await);
    }
    frozen(
        &call(
            &p,
            "move_page",
            json!({ "from": page, "to": pid("epic", "e-moved") }),
        )
        .await,
    );
    let archive =
        epic("e-closed", "closed").replace("status: closed\n", "status: closed\narchived: true\n");
    frozen(
        &call(
            &p,
            "update_page",
            json!({ "page_id": page, "content": archive }),
        )
        .await,
    );
    // Control: an unfrozen page can be deleted.
    ok(&call(&p, "delete_page", json!({ "page_id": pid("note", "n1") })).await);
}

#[tokio::test]
async fn the_write_that_freezes_is_allowed_and_the_next_one_is_not() {
    let p = start().await;
    let page = pid("epic", "e-open");
    ok(&call(
        &p,
        "update_page",
        json!({ "page_id": page, "content": epic("e-open", "closed") }),
    )
    .await);
    frozen(
        &call(
            &p,
            "update_page",
            json!({ "page_id": page, "content": epic("e-open", "closed") }),
        )
        .await,
    );
    // Frozen means frozen: the owner cannot reopen it, not even as admin.
    let reopen = call_as(
        &p,
        Role::Admin,
        "update_page",
        json!({ "page_id": page, "content": epic("e-open", "open") }),
    )
    .await;
    frozen(&reopen);
    // Its children froze with it.
    frozen(&call(&p, "update_page", json!({ "page_id": pid("ticket", "t-open"), "content": format!("{}x\n", ticket("t-open", "e-open")) })).await);
}

#[tokio::test]
async fn children_of_a_frozen_page_are_frozen_through_the_declared_link() {
    let p = start().await;
    for (skill, id, content) in [
        (
            "ticket",
            "t-closed",
            format!("{}x\n", ticket("t-closed", "e-closed")),
        ),
        // Two levels: task -> ticket -> epic.
        (
            "task",
            "k-closed",
            format!("{}x\n", task("k-closed", "t-closed")),
        ),
    ] {
        frozen(
            &call(
                &p,
                "update_page",
                json!({ "page_id": pid(skill, id), "content": content }),
            )
            .await,
        );
    }
    frozen(
        &call_as(
            &p,
            Role::Admin,
            "delete_page",
            json!({ "page_id": pid("task", "k-closed") }),
        )
        .await,
    );
    frozen(
        &call(
            &p,
            "move_page",
            json!({ "from": pid("ticket", "t-closed"), "to": pid("ticket", "t-moved") }),
        )
        .await,
    );
    // Controls: children of an open epic are writable, and so is an unlinked page.
    ok(&call(&p, "update_page", json!({ "page_id": pid("task", "k-open"), "content": format!("{}x\n", task("k-open", "t-open")) })).await);
    ok(&call(
        &p,
        "update_page",
        json!({ "page_id": pid("note", "n1"), "content": format!("{}x\n", note("n1")) }),
    )
    .await);
}

#[tokio::test]
async fn a_new_child_under_a_frozen_parent_is_refused_and_a_repoint_into_one_too() {
    let p = start().await;
    frozen(
        &call(
            &p,
            "update_page",
            json!({ "page_id": pid("ticket", "t-new"), "content": ticket("t-new", "e-closed") }),
        )
        .await,
    );
    frozen(
        &call(
            &p,
            "update_page",
            json!({ "page_id": pid("task", "k-new"), "content": task("k-new", "t-closed") }),
        )
        .await,
    );
    // Re-pointing a live child at a frozen parent is the same thing.
    frozen(
        &call(
            &p,
            "update_page",
            json!({ "page_id": pid("ticket", "t-open"), "content": ticket("t-open", "e-closed") }),
        )
        .await,
    );
    // Controls.
    ok(&call(
        &p,
        "update_page",
        json!({ "page_id": pid("ticket", "t-new"), "content": ticket("t-new", "e-open") }),
    )
    .await);
}

#[tokio::test]
async fn the_parent_chain_is_capped_at_four_levels_and_a_cycle_terminates() {
    let p = start().await;
    // n1..n4 are 1..4 links below the frozen n0; n5 and n6 are beyond the cap.
    for i in 1..=4 {
        let (id, parent) = (format!("n{i}"), format!("n{}", i - 1));
        frozen(&call(&p, "update_page", json!({ "page_id": pid("node", &id), "content": format!("{}x\n", node(&id, Some(&parent), "live")) })).await);
    }
    for i in 5..=6 {
        let (id, parent) = (format!("n{i}"), format!("n{}", i - 1));
        ok(&call(&p, "update_page", json!({ "page_id": pid("node", &id), "content": format!("{}x\n", node(&id, Some(&parent), "live")) })).await);
    }
    // ca <-> cb never freezes and never loops.
    ok(&call(&p, "update_page", json!({ "page_id": pid("node", "ca"), "content": format!("{}x\n", node("ca", Some("cb"), "live")) })).await);
}

#[tokio::test]
async fn a_draft_for_a_frozen_page_is_refused_and_not_queued() {
    let p = start().await;
    let page = pid("epic", "e-closed");
    let content = epic_x("e-closed", "closed");
    let d = call(&p, "create_draft", json!({ "target_page_id": page, "content": content, "base_sha256": sha(&epic("e-closed", "closed")) })).await;
    frozen(&d);
    let child = call(&p, "create_draft", json!({ "target_page_id": pid("ticket", "t-new"), "content": ticket("t-new", "e-closed"), "base_sha256": "" })).await;
    frozen(&child);
    let queue = call(&p, "list_drafts", json!({})).await;
    assert_eq!(queue["drafts"].as_array().map(Vec::len), Some(0), "{queue}");
}

#[tokio::test]
async fn a_changeset_cannot_carry_a_frozen_member_and_one_frozen_since_lands_nothing() {
    let p = start().await;
    let first = call(&p, "create_draft", json!({ "target_page_id": pid("note", "n1"), "content": format!("{}x\n", note("n1")), "base_sha256": sha(&note("n1")), "new_changeset": true })).await;
    ok(&first);
    let cs = first["draft"]["changeset_id"].as_str().unwrap().to_owned();
    let second = call(&p, "create_draft", json!({ "target_page_id": pid("epic", "e-closed"), "content": epic_x("e-closed", "closed"), "base_sha256": sha(&epic("e-closed", "closed")), "changeset_id": cs })).await;
    frozen(&second);

    // A member drafted while the page was open, frozen by a skill edit before promotion.
    let a = call(&p, "create_draft", json!({ "target_page_id": pid("note", "n2"), "content": format!("{}y\n", note("n2")), "base_sha256": sha(&note("n2")), "new_changeset": true })).await;
    ok(&a);
    let cs2 = a["draft"]["changeset_id"].as_str().unwrap().to_owned();
    let b = call(&p, "create_draft", json!({ "target_page_id": pid("epic", "e-open"), "content": epic_x("e-open", "open"), "base_sha256": sha(&epic("e-open", "open")), "changeset_id": cs2 })).await;
    ok(&b);
    freeze_open_epics(&p).await;
    let r = call_as(
        &p,
        Role::Admin,
        "promote_changeset",
        json!({ "changeset_id": cs2 }),
    )
    .await;
    assert_eq!(r["ok"], json!(false), "{r}");
    assert!(codes(&r).contains(&"instance_frozen".to_owned()), "{r}");
    assert!(
        !stored(&p, &pid("note", "n2")).await.contains("y\n"),
        "nothing landed"
    );
}

/// Freeze every open epic by editing the skill: the stored pages do not change.
async fn freeze_open_epics(p: &EscurelProcess) {
    let skill = EPIC.replace("[closed, cancelled]", "[closed, cancelled, open]");
    ok(&call_as(
        p,
        Role::Admin,
        "update_page",
        json!({ "page_id": "markdown/skills/epic.md", "content": skill }),
    )
    .await);
}

#[tokio::test]
async fn a_single_draft_made_before_the_freeze_is_refused_at_promotion() {
    let p = start().await;
    let page = pid("epic", "e-open");
    let d = call(&p, "create_draft", json!({ "target_page_id": page, "content": epic_x("e-open", "open"), "base_sha256": sha(&epic("e-open", "open")) })).await;
    ok(&d);
    let id = d["draft"]["draft_id"].as_str().unwrap().to_owned();
    freeze_open_epics(&p).await;
    let r = call_as(&p, Role::Admin, "promote_draft", json!({ "draft_id": id })).await;
    assert_eq!(r["ok"], json!(false), "{r}");
    assert!(codes(&r).contains(&"instance_frozen".to_owned()), "{r}");
}

#[tokio::test]
async fn unfreezing_goes_through_the_skill_page_not_through_the_instance() {
    let p = start().await;
    let page = pid("epic", "e-closed");
    frozen(
        &call_as(
            &p,
            Role::Admin,
            "update_page",
            json!({ "page_id": page, "content": epic("e-closed", "open") }),
        )
        .await,
    );
    // The curator edits the skill: no declaration, no freeze.
    let relaxed = EPIC.replace("freeze_when: {status: [closed, cancelled]}\n", "");
    ok(&call_as(
        &p,
        Role::Admin,
        "update_page",
        json!({ "page_id": "markdown/skills/epic.md", "content": relaxed }),
    )
    .await);
    ok(&call_as(
        &p,
        Role::Admin,
        "update_page",
        json!({ "page_id": page, "content": epic("e-closed", "open") }),
    )
    .await);
}

#[tokio::test]
async fn malformed_declarations_are_linted_on_the_skill_page() {
    let p = start().await;
    let skill = |extra: &str, field: &str| {
        format!(
            "---\nkind: skill\nid: bad\ndescription: x\nvisibility: public\n{extra}fields:\n  - {field}\n---\n# bad\n"
        )
    };
    let cases = [
        (
            skill("freeze_when: closed\n", "{name: a, kind: string}"),
            "skill_rule_malformed",
        ),
        (
            skill("freeze_when: {a: x, b: y}\n", "{name: a, kind: string}"),
            "skill_rule_malformed",
        ),
        (
            skill("freeze_when: {a: []}\n", "{name: a, kind: string}"),
            "skill_rule_malformed",
        ),
        (
            skill("freeze_when: {a: {x: 1}}\n", "{name: a, kind: string}"),
            "skill_rule_malformed",
        ),
        (
            skill(
                "",
                "{name: a, kind: link, to: note, frozen_by_parent: \"yes\"}",
            ),
            "fields_malformed",
        ),
        (
            skill("", "{name: a, kind: string, frozen_by_parent: true}"),
            "fields_malformed",
        ),
    ];
    for (content, code) in cases {
        let v = call_as(
            &p,
            Role::Admin,
            "validate",
            json!({ "as_page_id": "markdown/skills/bad.md", "content": content }),
        )
        .await;
        assert!(
            v["issues"]
                .as_array()
                .unwrap()
                .iter()
                .any(|i| i["code"] == code),
            "{code}: {v}"
        );
        let w = call_as(
            &p,
            Role::Admin,
            "update_page",
            json!({ "page_id": "markdown/skills/bad.md", "content": content }),
        )
        .await;
        assert_eq!(w["ok"], json!(false), "{w}");
    }
    let good = skill("freeze_when: {a: x}\n", "{name: a, kind: string}");
    ok(&call_as(
        &p,
        Role::Admin,
        "update_page",
        json!({ "page_id": "markdown/skills/bad.md", "content": good }),
    )
    .await);
}

#[tokio::test]
async fn list_skills_exposes_the_declarations() {
    let p = start().await;
    let r = call(&p, "list_skills", json!({})).await;
    let find = |id: &str| {
        r["skills"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"] == id)
            .unwrap()
            .clone()
    };
    assert_eq!(
        find("epic")["freeze_when"],
        json!(["status", ["closed", "cancelled"]])
    );
    assert!(find("note").get("freeze_when").is_none());
    let field = find("ticket")["fields"][0].clone();
    assert_eq!(field["frozen_by_parent"], json!(true), "{field}");
}

#[tokio::test]
async fn lease_fence_comes_first_then_frozen() {
    let lease_skill = "---\nkind: skill\nid: job\ndescription: A lease.\nvisibility: public\n\
        lease:\n  holder: attempt\n  expires: lease_until\n---\n# job\n";
    let until = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 600;
    let job = format!(
        "---\nkind: instance\nskill: job\nid: j1\nattempt: h1\nlease_until: {until}\n---\n# j\n"
    );
    let fixtures = FixtureBuilder::new()
        .tenant(TENANT)
        .skill("epic", EPIC)
        .skill("job", lease_skill)
        .instance("epic", "e-closed", epic("e-closed", "closed").as_str())
        .instance("job", "j1", job.as_str())
        .done();
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides::default(),
        fixtures: Some(fixtures),
    })
    .await;
    let draft = |holder: &str| {
        json!({
            "target_page_id": pid("epic", "e-closed"),
            "content": epic_x("e-closed", "closed"),
            "base_sha256": sha(&epic("e-closed", "closed")),
            "lease": { "page_id": pid("job", "j1"), "holder": holder },
        })
    };
    let stale = call(&p, "create_draft", draft("someone-else")).await;
    assert_eq!(stale["issues"][0]["code"], json!("lease_fence"), "{stale}");
    let current = call(&p, "create_draft", draft("h1")).await;
    frozen(&current);
}

#[tokio::test]
async fn frozen_comes_before_instance_immutable_and_relation_in_use() {
    // A skill that is frozen, immutable and referenced: the freeze answers first.
    let both = "---\nkind: skill\nid: both\ndescription: x\nvisibility: public\nimmutable: true\n\
        freeze_when: {status: closed}\n---\n# both\n";
    let refers = "---\nkind: skill\nid: refers\ndescription: x\nvisibility: public\n\
        fields:\n  - {name: to, kind: link, to: both}\n---\n# refers\n";
    let both_page = "---\nkind: instance\nskill: both\nid: b1\nstatus: closed\n---\n# b1\n";
    let refers_page =
        "---\nkind: instance\nskill: refers\nid: r1\nto: \"[[both::b1]]\"\n---\n# r1\n";
    let fixtures = FixtureBuilder::new()
        .tenant(TENANT)
        .skill("both", both)
        .skill("refers", refers)
        .instance("both", "b1", both_page)
        .instance("refers", "r1", refers_page)
        .done();
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides::default(),
        fixtures: Some(fixtures),
    })
    .await;
    let page = pid("both", "b1");
    for tool_args in [
        ("delete_page", json!({ "page_id": page })),
        (
            "move_page",
            json!({ "from": page, "to": pid("both", "b2") }),
        ),
        (
            "update_page",
            json!({ "page_id": page, "content": both_page.replace("closed\n", "closed\narchived: true\n") }),
        ),
    ] {
        let r = call_as(&p, Role::Admin, tool_args.0, tool_args.1).await;
        frozen(&r);
        assert_eq!(codes(&r), vec!["instance_frozen".to_owned()], "{r}");
    }
}
