//! Typed instance fields — the `fields:` skill-page key (#508), over real
//! HTTP against a running gateway with a real Indexer. No mocks, no LLM.
//!
//! `required_frontmatter` is a KEY-NAME list. It says `hotness` must be
//! present and nothing whatever about what may be in it, so an agent writing
//! frontmatter free-hand can emit `hot`, `Hot`, `"hot "`, `[hot]` and
//! `5-Cold-ish` on five consecutive runs and all five commit clean — after
//! which `list_instances(filter={hotness: hot})` does string equality against
//! a corpus that has quietly fractured into synonym classes.
//!
//! `fields:` adds the shape. What is pinned here is the half that only the
//! full stack can show: that the declaration actually BLOCKS a write, on
//! every path that writes (`update_page`, `create_draft`), and that a skill
//! which declares no `fields:` is completely unaffected.
//!
//! [`skill_params`](super::skill_params) is the sibling for `params:`, which
//! describes what one RUN takes rather than what the instances look like.

use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "stuttgart-ai";

/// The control: no `fields:` at all. Every skill page in every existing
/// tenant looks like this, and must keep behaving exactly as it did.
const NOTE_SKILL: &str = "---\ntype: skill\nid: note\ndescription: A note.\n\
    required_frontmatter: [status]\n---\n# note\n";

/// The article's showcase, declared.
const ACCOUNT_SKILL: &str = "\
---
type: skill
id: account
description: A customer account.
fields:
  - {name: hotness, kind: enum, values: [hot, warm, cold], label: Temperature}
  - {name: opened, kind: date, required: true}
  - {name: arr_eur, kind: float, min: 0}
---
# account
";

fn account(body: &str) -> String {
    format!("---\ntype: instance\nskill: account\nid: globex\n{body}---\n# Globex\n")
}

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides::default(),
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("note", NOTE_SKILL)
                .skill("account", ACCOUNT_SKILL)
                .done(),
        ),
    })
    .await
}

async fn call(p: &EscurelProcess, token: &str, tool: &str, args: Value) -> Value {
    let body: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": tool, "arguments": args },
        }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json");
    assert!(body.get("error").is_none(), "{tool} error: {body}");
    body["result"]["structuredContent"].clone()
}

/// The promise: an agent physically cannot write `5-Cold-ish` into a field
/// declared `enum(hot, warm, cold)`. Not a lint — the write does not land.
#[tokio::test]
async fn a_write_that_violates_a_declared_field_does_not_commit() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);
    let page = "markdown/instances/account/globex.md";

    let refused = call(
        &p,
        &token,
        "update_page",
        json!({
            "page_id": page,
            "content": account("hotness: 5-Cold-ish\nopened: 2026-01-05\n"),
        }),
    )
    .await;
    assert_eq!(refused["ok"], json!(false), "{refused}");
    assert!(
        refused["issues"].as_array().is_some_and(|i| i
            .iter()
            .any(|i| i["code"] == json!("frontmatter_enum_value"))),
        "the refusal must name the field rule that stopped it: {refused}"
    );

    // Nothing landed. The page does not exist — which is the strongest form
    // of "did not commit" available for a create.
    let expanded = call(&p, &token, "expand", json!({ "page_id": page })).await;
    assert!(
        expanded.get("body").and_then(Value::as_str).is_none()
            || !expanded["body"]
                .as_str()
                .unwrap_or_default()
                .contains("Globex"),
        "a refused write must leave no page: {expanded}"
    );

    // The same content with a declared value lands — so the rejection above
    // is about the VALUE and not about the field being unwritable.
    let ok = call(
        &p,
        &token,
        "update_page",
        json!({
            "page_id": page,
            "content": account("hotness: cold\nopened: 2026-01-05\narr_eur: 99000\n"),
        }),
    )
    .await;
    assert_eq!(ok["ok"], json!(true), "{ok}");
}

/// Every write path, not just the obvious one. A held write that could never
/// land costs a human a review before anyone finds out.
#[tokio::test]
async fn a_draft_that_violates_a_declared_field_is_refused_at_draft_time() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);

    let refused = call(
        &p,
        &token,
        "create_draft",
        json!({
            "target_page_id": "markdown/instances/account/globex.md",
            "content": account("hotness: lukewarm\nopened: 2026-01-05\n"),
        }),
    )
    .await;
    assert_eq!(refused["ok"], json!(false), "{refused}");
    assert!(
        refused["issues"].as_array().is_some_and(|i| i
            .iter()
            .any(|i| i["code"] == json!("frontmatter_enum_value"))),
        "{refused}"
    );
}

/// A client must be able to build an instance form from the catalogue alone,
/// exactly as it builds a run form from `params`.
#[tokio::test]
async fn list_skills_carries_the_declared_fields() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);

    let listed = call(&p, &token, "list_skills", json!({})).await;
    let skills = listed["skills"].as_array().expect("skills");

    let account = skills
        .iter()
        .find(|s| s["id"] == json!("account"))
        .expect("the typed skill");
    let fields = account["fields"].as_array().expect("fields");
    let hotness = fields
        .iter()
        .find(|f| f["name"] == json!("hotness"))
        .unwrap_or_else(|| panic!("hotness must be published: {account}"));
    assert_eq!(hotness["kind"], json!("enum"), "{hotness}");
    assert_eq!(
        hotness["values"],
        json!(["hot", "warm", "cold"]),
        "a form cannot offer a choice it was not told about: {hotness}"
    );
    assert_eq!(hotness["label"], json!("Temperature"), "{hotness}");
    let opened = fields
        .iter()
        .find(|f| f["name"] == json!("opened"))
        .expect("opened");
    assert_eq!(opened["kind"], json!("date"), "{opened}");
    assert_eq!(opened["required"], json!(true), "{opened}");

    // The control: a skill declaring no `fields:` carries no `fields` key at
    // all, so its row is byte-identical to what it was before typing existed.
    let note = skills
        .iter()
        .find(|s| s["id"] == json!("note"))
        .expect("the untyped skill");
    assert!(
        note.get("fields").is_none(),
        "an untyped skill must not grow an empty field list: {note}"
    );
}

/// The regression that matters most: typing is opt-in per skill, so a corpus
/// written untyped stays exactly as writable as it was.
#[tokio::test]
async fn a_skill_without_fields_accepts_what_it_always_accepted() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);

    let ok = call(
        &p,
        &token,
        "update_page",
        json!({
            "page_id": "markdown/instances/note/n1.md",
            "content": "---\ntype: instance\nskill: note\nid: n1\n\
                        status: 5-Cold-ish\nhotness: whatever\n---\n# n1\n",
        }),
    )
    .await;
    assert_eq!(
        ok["ok"],
        json!(true),
        "an untyped skill types nothing, and nothing new may block it: {ok}"
    );
}
