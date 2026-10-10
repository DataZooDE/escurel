//! Round-2 review (2026-10-04): ways a MACHINE caller got around the autonomy gate, each reproduced
//! through the real gateway with real tokens before it was closed.
//!
//! * a machine could edit the SKILL page and switch `autonomy: auto` on for itself;
//! * a machine could promote (or discard) the draft it had just been held in;
//! * a run token minted by the runner is ADMIN, and the gate waved admins through;
//! * `move_page` gated only the source, `merge_branch` landed unreviewed bytes (an unparsable skill
//!   page failing OPEN is pinned by a unit test next to the gate).

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "acme";

fn skill(id: &str, autonomy: Option<&str>) -> String {
    let a = autonomy
        .map(|a| format!("autonomy: {a}\n"))
        .unwrap_or_default();
    format!("---\nkind: skill\nid: {id}\ndescription: {id}.\n{a}---\n# {id}\n")
}

fn instance(skill: &str, id: &str, note: &str) -> String {
    format!("---\nkind: instance\nskill: {skill}\nid: {id}\n---\n# {id}\n\n{note}\n")
}

fn page(skill: &str, id: &str) -> String {
    format!("markdown/instances/{skill}/{id}.md")
}

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("triage", skill("triage", Some("review")).as_str())
                .skill("open", skill("open", None).as_str())
                .instance(
                    "triage",
                    "t0",
                    instance("triage", "t0", "BASELINE").as_str(),
                )
                .instance("open", "o0", instance("open", "o0", "BASELINE").as_str())
                .done(),
        ),
        ..Default::default()
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

async fn call(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    let body = rpc(p, token, name, args).await;
    assert!(body.get("error").is_none(), "{name}: {body}");
    body["result"]["structuredContent"].clone()
}

fn machine(p: &EscurelProcess) -> String {
    p.mint_token_for_run(TENANT, Role::Agent, "agent:triage", "run-1", "root-1")
}

/// What the runner mints today: an agent run token that is ADMIN.
fn machine_admin(p: &EscurelProcess) -> String {
    p.mint_token_for_run(TENANT, Role::Admin, "agent:triage", "run-2", "root-2")
}

fn human(p: &EscurelProcess) -> String {
    p.mint_token(TENANT, Role::Agent)
}

async fn body_of(p: &EscurelProcess, token: &str, page_id: &str) -> String {
    let r = call(p, token, "expand", json!({ "page_id": page_id })).await;
    r["body"].as_str().unwrap_or_default().to_owned()
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

#[tokio::test]
async fn a_machine_cannot_switch_the_gate_off_by_editing_the_skill_page() {
    let p = start().await;
    let agent = machine(&p);
    let edit = call(
        &p,
        &agent,
        "update_page",
        json!({ "page_id": "markdown/skills/triage.md", "content": skill("triage", Some("auto")) }),
    )
    .await;
    // Held or refused — either way the skill page did not move.
    assert!(
        edit["held_for_review"] == json!(true)
            || codes(&edit).contains(&"skill_edit_requires_human".to_owned()),
        "a machine's skill-page edit must not land: {edit}"
    );
    let page_now = body_of(&p, &agent, "markdown/skills/triage.md").await;
    assert!(!page_now.contains("autonomy: auto"), "{page_now}");
    // ... and so the instance gate still holds.
    let w = call(
        &p,
        &agent,
        "update_page",
        json!({ "page_id": page("triage", "t0"), "content": instance("triage", "t0", "SNEAKY") }),
    )
    .await;
    assert_eq!(w["held_for_review"], true, "{w}");
}

#[tokio::test]
async fn a_machine_cannot_promote_the_draft_it_was_held_in_nor_discard_anothers() {
    let p = start().await;
    let agent = machine(&p);
    let held = call(
        &p,
        &agent,
        "update_page",
        json!({ "page_id": page("triage", "t0"), "content": instance("triage", "t0", "SELF-APPROVED") }),
    )
    .await;
    let id = held["draft"]["draft_id"].as_str().unwrap().to_owned();
    let promoted = call(&p, &agent, "promote_draft", json!({ "draft_id": id })).await;
    assert_eq!(promoted["ok"], false, "{promoted}");
    assert!(
        codes(&promoted).contains(&"promote_requires_human".to_owned()),
        "{promoted}"
    );
    // Another run's token cannot discard it either (a decision on someone else's work) ...
    let other = p.mint_token_for_run(TENANT, Role::Agent, "agent:other", "run-9", "root-9");
    let discarded = call(&p, &other, "discard_draft", json!({ "draft_id": id })).await;
    assert_eq!(discarded["ok"], false, "{discarded}");
    assert!(
        !body_of(&p, &agent, &page("triage", "t0"))
            .await
            .contains("SELF-APPROVED")
    );
    // ... the draft is still open, and a person lands it.
    let done = call(&p, &human(&p), "promote_draft", json!({ "draft_id": id })).await;
    assert_eq!(done["ok"], true, "{done}");
    assert!(
        body_of(&p, &human(&p), &page("triage", "t0"))
            .await
            .contains("SELF-APPROVED")
    );
}

#[tokio::test]
async fn a_machine_may_withdraw_its_own_draft() {
    let p = start().await;
    let agent = machine(&p);
    let held = call(
        &p,
        &agent,
        "update_page",
        json!({ "page_id": page("triage", "t0"), "content": instance("triage", "t0", "OOPS") }),
    )
    .await;
    let id = held["draft"]["draft_id"].as_str().unwrap().to_owned();
    let out = call(&p, &agent, "discard_draft", json!({ "draft_id": id })).await;
    assert_eq!(out["ok"], true, "{out}");
}

#[tokio::test]
async fn a_machine_cannot_promote_a_changeset_it_proposed() {
    let p = start().await;
    let agent = machine(&p);
    let first = call(
        &p,
        &agent,
        "create_draft",
        json!({ "target_page_id": page("open", "o9"), "new_changeset": true,
                "content": instance("open", "o9", "BATCH-1") }),
    )
    .await;
    let cs = first["draft"]["changeset_id"]
        .as_str()
        .expect("changeset id")
        .to_owned();
    let out = call(
        &p,
        &agent,
        "promote_changeset",
        json!({ "changeset_id": cs }),
    )
    .await;
    assert_eq!(out["ok"], false, "{out}");
    assert!(
        codes(&out).contains(&"promote_requires_human".to_owned()),
        "{out}"
    );
    let landed = call(
        &p,
        &human(&p),
        "promote_changeset",
        json!({ "changeset_id": cs }),
    )
    .await;
    assert_eq!(landed["ok"], true, "{landed}");
}

#[tokio::test]
async fn a_runner_minted_admin_run_token_is_gated_like_any_other_machine() {
    let p = start().await;
    let agent = machine_admin(&p);
    let w = call(
        &p,
        &agent,
        "update_page",
        json!({ "page_id": page("triage", "t0"), "content": instance("triage", "t0", "ADMIN-RUN") }),
    )
    .await;
    assert_eq!(
        w["held_for_review"], true,
        "an admin RUN token is still a machine: {w}"
    );
    let del = call(
        &p,
        &agent,
        "delete_page",
        json!({ "page_id": page("triage", "t0") }),
    )
    .await;
    assert_eq!(del["ok"], false, "{del}");
    // A person who is admin still writes straight through.
    let person = p.mint_token(TENANT, Role::Admin);
    let ok = call(
        &p,
        &person,
        "update_page",
        json!({ "page_id": page("triage", "t0"), "content": instance("triage", "t0", "ADMIN-PERSON") }),
    )
    .await;
    assert_eq!(ok["ok"], true, "{ok}");
    assert!(ok.get("held_for_review").is_none(), "{ok}");
}

#[tokio::test]
async fn a_machine_cannot_move_a_page_into_a_review_skill() {
    let p = start().await;
    let agent = machine(&p);
    let mv = call(
        &p,
        &agent,
        "move_page",
        json!({ "from": page("open", "o0"), "to": page("triage", "smuggled") }),
    )
    .await;
    assert_eq!(mv["ok"], false, "{mv}");
    assert!(codes(&mv).contains(&"review_required".to_owned()), "{mv}");
    let r = call(
        &p,
        &agent,
        "expand",
        json!({ "page_id": page("triage", "smuggled") }),
    )
    .await;
    assert!(r["page"].is_null(), "nothing landed: {r}");
}

#[tokio::test]
async fn a_machine_cannot_land_a_branch_onto_a_review_skill() {
    let p = start().await;
    let agent = machine(&p);
    call(&p, &agent, "create_branch", json!({ "name": "wip" })).await;
    let wrote = call(
        &p,
        &agent,
        "update_page",
        json!({ "page_id": page("triage", "t0"), "branch": "wip",
                "content": instance("triage", "t0", "BRANCH-BYPASS") }),
    )
    .await;
    let _ = wrote;
    let merged = call(&p, &agent, "merge_branch", json!({ "name": "wip" })).await;
    assert_eq!(merged["ok"], false, "{merged}");
    assert!(
        !body_of(&p, &human(&p), &page("triage", "t0"))
            .await
            .contains("BRANCH-BYPASS"),
        "the merge must not land a machine's bytes on a review skill"
    );
}

/// The probe `merge_branch` runs for a machine used to be an INSTANCE id (`markdown/instances/<skill>/<slug>.md`),
/// so a skill page on a branch was judged by whatever skill happened to share its name: refused by luck for
/// a review skill, landed for an `auto` one (and for a brand-new skill page).
#[tokio::test]
async fn a_machine_cannot_land_a_skill_page_through_a_branch() {
    let p = start().await;
    let agent = machine(&p);
    call(&p, &agent, "create_branch", json!({ "name": "wip" })).await;
    // An EDIT of the `auto` skill's page and a brand-new skill page, both only a view on the branch.
    for (id, body) in [
        ("open", skill("open", Some("auto"))),
        ("evil", skill("evil", Some("auto"))),
    ] {
        let wrote = call(
            &p,
            &agent,
            "update_page",
            json!({ "page_id": format!("markdown/skills/{id}.md"), "branch": "wip", "content": body }),
        )
        .await;
        assert_ne!(wrote["ok"], false, "{wrote}");
    }
    // Merging is the direct write: a machine's merge must be refused as a whole.
    let merged = call(&p, &agent, "merge_branch", json!({ "name": "wip" })).await;
    assert_eq!(
        merged["ok"], false,
        "a machine merged skill pages: {merged}"
    );
    assert!(
        codes(&merged).contains(&"review_required".to_owned()),
        "{merged}"
    );
    let landed = rpc(
        &p,
        &human(&p),
        "expand",
        json!({ "page_id": "markdown/skills/evil.md" }),
    )
    .await;
    assert!(
        landed["result"]["structuredContent"]["page"].is_null(),
        "a brand-new skill page landed from a machine's branch: {landed}"
    );
    // A person may merge the same branch: that is the review.
    let by_person = call(&p, &human(&p), "merge_branch", json!({ "name": "wip" })).await;
    assert_ne!(by_person["ok"], false, "{by_person}");
}

#[tokio::test]
async fn a_held_write_is_typed_in_the_update_page_answer() {
    let p = start().await;
    let r = call(
        &p,
        &machine(&p),
        "update_page",
        json!({ "page_id": page("triage", "t0"), "content": instance("triage", "t0", "H") }),
    )
    .await;
    assert_eq!(r["held_for_review"], true, "{r}");
}

#[tokio::test]
async fn a_machine_cannot_write_through_to_a_review_skill_s_source() {
    let p = start().await;
    let body = rpc(
        &p,
        &machine_admin(&p),
        "write_instance",
        json!({ "ref": format!("triage::t0"), "payload": { "x": 1 } }),
    )
    .await;
    let text = body.to_string();
    assert!(text.contains("review_required"), "{text}");
}
