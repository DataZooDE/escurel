//! `autonomy:` is ENFORCED at the gateway (owner decision, 2026-10-04) for MACHINE callers: a direct
//! write by a token minted for an agent run (`run_id` / `skill` / `act.sub` claims) to an instance of a
//! skill that declares `autonomy: review` or `confirm` does not land. It becomes an open draft and the
//! answer says so (`held_for_review`, the shape `create_draft` answers). A person on a plain agent-role
//! token, an admin, `autonomy: auto` skills and skills that declare nothing write directly, as before; an
//! unrecognised value fails toward holding; PROMOTING a held draft always lands.
//!
//! Real gateway over HTTP with a real verifier (test issuer), real DuckDB, real tokens.

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

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("triage", skill("triage", Some("review")).as_str())
                .skill("payment", skill("payment", Some("confirm")).as_str())
                .skill("draftbot", skill("draftbot", Some("auto")).as_str())
                .skill("note", skill("note", None).as_str())
                .skill("broken", skill("broken", Some("atuo")).as_str())
                .instance(
                    "triage",
                    "t0",
                    instance("triage", "t0", "BASELINE").as_str(),
                )
                .done(),
        ),
        ..Default::default()
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

fn page(skill: &str, id: &str) -> String {
    format!("markdown/instances/{skill}/{id}.md")
}

/// A token minted for an agent RUN: the shape the runner hands an agent.
fn machine(p: &EscurelProcess) -> String {
    p.mint_token_for_run(TENANT, Role::Agent, "agent:triage", "run-1", "root-1")
}

/// A person on an ordinary agent-role token (the extension, the CLI).
fn human(p: &EscurelProcess) -> String {
    p.mint_token(TENANT, Role::Agent)
}

#[tokio::test]
async fn a_machine_write_to_a_review_skill_is_held_as_a_draft_and_the_page_does_not_move() {
    let p = start().await;
    let agent = machine(&p);
    let r = call(
        &p,
        &agent,
        "update_page",
        json!({ "page_id": page("triage", "t0"), "content": instance("triage", "t0", "AGENT EDIT") }),
    )
    .await;
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(
        r["held_for_review"], true,
        "the response says it was held: {r}"
    );
    assert_eq!(r["draft"]["status"], "open", "{r}");
    assert_eq!(r["draft"]["target_page_id"], page("triage", "t0"), "{r}");
    let now = call(
        &p,
        &agent,
        "expand",
        json!({ "page_id": page("triage", "t0") }),
    )
    .await;
    assert!(now["body"].as_str().unwrap().contains("BASELINE"), "{now}");
    assert!(
        !now["body"].as_str().unwrap().contains("AGENT EDIT"),
        "{now}"
    );
    let drafts = call(&p, &agent, "list_drafts", json!({})).await;
    assert_eq!(drafts["drafts"].as_array().unwrap().len(), 1, "{drafts}");
    // `confirm` holds too, and creating a new page is a write like any other.
    let c = call(
        &p,
        &agent,
        "update_page",
        json!({ "page_id": page("payment", "p1"), "content": instance("payment", "p1", "new") }),
    )
    .await;
    assert_eq!(c["held_for_review"], true, "{c}");
}

#[tokio::test]
async fn a_person_an_admin_auto_unset_and_skill_pages_keep_landing_directly() {
    let p = start().await;
    let agent = machine(&p);
    let admin = p.mint_token(TENANT, Role::Admin);
    // A person on a plain agent-role token and an admin land a review-skill write.
    for (tok, note) in [(human(&p), "PERSON EDIT"), (admin.clone(), "ADMIN EDIT")] {
        let a = call(
            &p,
            &tok,
            "update_page",
            json!({ "page_id": page("triage", "t0"), "content": instance("triage", "t0", note) }),
        )
        .await;
        assert_eq!(a["ok"], true, "{note}: {a}");
        assert!(a.get("held_for_review").is_none(), "{note}: {a}");
    }
    // A machine lands where the skill is `auto` or declares nothing.
    for (sk, id) in [("draftbot", "d1"), ("note", "n1")] {
        let r = call(
            &p,
            &agent,
            "update_page",
            json!({ "page_id": page(sk, id), "content": instance(sk, id, "x") }),
        )
        .await;
        assert_eq!(r["ok"], true, "{sk}: {r}");
        assert!(r.get("held_for_review").is_none(), "{sk}: {r}");
        assert!(r["new_version"].is_string(), "{sk}: it landed: {r}");
    }
    // Writing a SKILL page is not an instance write.
    let s = call(
        &p,
        &agent,
        "update_page",
        json!({ "page_id": "markdown/skills/fresh.md", "content": skill("fresh", Some("review")) }),
    )
    .await;
    assert_eq!(s["ok"], true, "{s}");
    assert!(s.get("held_for_review").is_none(), "{s}");
}

#[tokio::test]
async fn an_unrecognised_autonomy_value_fails_toward_holding() {
    let p = start().await;
    let r = call(
        &p,
        &machine(&p),
        "update_page",
        json!({ "page_id": page("broken", "b1"), "content": instance("broken", "b1", "x") }),
    )
    .await;
    assert_eq!(
        r["held_for_review"], true,
        "a typo must never read as `auto`: {r}"
    );
}

#[tokio::test]
async fn promoting_a_held_draft_lands_it_even_when_the_approver_is_a_machine_token() {
    let p = start().await;
    let agent = machine(&p);
    let held = call(
        &p,
        &agent,
        "update_page",
        json!({ "page_id": page("triage", "t0"), "content": instance("triage", "t0", "TO APPROVE") }),
    )
    .await;
    let id = held["draft"]["draft_id"].as_str().unwrap().to_owned();
    // Promotion re-enters the write path and must LAND, not be held a second time.
    let done = call(&p, &agent, "promote_draft", json!({ "draft_id": id })).await;
    assert_eq!(done["ok"], true, "{done}");
    let now = call(
        &p,
        &agent,
        "expand",
        json!({ "page_id": page("triage", "t0") }),
    )
    .await;
    assert!(
        now["body"].as_str().unwrap().contains("TO APPROVE"),
        "{now}"
    );
    let drafts = call(&p, &agent, "list_drafts", json!({})).await;
    assert!(drafts["drafts"].as_array().unwrap().is_empty(), "{drafts}");
}

#[tokio::test]
async fn a_machine_cannot_move_or_delete_a_review_page_but_a_person_can() {
    let p = start().await;
    let agent = machine(&p);
    let del = call(
        &p,
        &agent,
        "delete_page",
        json!({ "page_id": page("triage", "t0") }),
    )
    .await;
    assert_eq!(del["ok"], false, "{del}");
    assert_eq!(del["issues"][0]["code"], "review_required", "{del}");
    let mv = call(
        &p,
        &agent,
        "move_page",
        json!({ "from": page("triage", "t0"), "to": page("triage", "t9") }),
    )
    .await;
    assert_eq!(mv["issues"][0]["code"], "review_required", "{mv}");
    // Still there.
    let still = call(
        &p,
        &agent,
        "expand",
        json!({ "page_id": page("triage", "t0") }),
    )
    .await;
    assert!(still["page"].is_object(), "{still}");
    // A person deletes it.
    let gone = call(
        &p,
        &human(&p),
        "delete_page",
        json!({ "page_id": page("triage", "t0") }),
    )
    .await;
    assert_eq!(gone["ok"], true, "{gone}");
}
