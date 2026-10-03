//! The skill contract keys the workbench reads (knowledge-workbench backend
//! P2-7 — BRD FR-S-1..5): `summary`, `harness`, `actions[]`, `cascade{}`.
//! `list_skills` reports what a skill page declares; `validate` lints it —
//! a skill without a `summary` (`summary_missing`, warning), one over 200
//! characters (`summary_too_long`), a `harness` no adapter answers to
//! (`harness_unknown`), an `actions` entry naming a skill the corpus does
//! not have (`action_skill_unknown`). Real gateway, real DuckDB.

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "carl";
const RENEWAL: &str = "---\ntype: skill\nid: renewal\ndescription: d.\nautonomy: review\n\
summary: Keeps each contract's renewal date and terms current.\nharness: claude\n\
actions:\n  - {name: record-decision, kind: event, label: Record the decision, event: decision-record}\n  - {name: ask-why, kind: prompt, label: Ask why, prompt: \"why was {id} renewed?\"}\ncascade:\n  target: produced\n  max_depth: 2\n---\n# renewal\n";
const DECISION: &str = "---\ntype: skill\nid: decision-record\ndescription: d.\nautonomy: auto\n---\n# decision-record\n";

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("renewal", RENEWAL)
                .skill("decision-record", DECISION)
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

fn skill(extra: &str) -> String {
    format!("---\ntype: skill\nid: note\ndescription: d.\nautonomy: review\n{extra}---\n# note\n")
}

fn issue<'a>(out: &'a Value, code: &str) -> Option<&'a Value> {
    out["issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["code"] == code)
}

#[tokio::test]
async fn list_skills_reports_the_contract_keys_a_skill_declares() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);
    let out = call(&p, &token, "list_skills", json!({})).await;
    let by_id: std::collections::HashMap<&str, &Value> = out["skills"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["id"].as_str().unwrap(), s))
        .collect();
    let r = by_id["renewal"];
    assert_eq!(
        r["summary"], "Keeps each contract's renewal date and terms current.",
        "{r}"
    );
    assert_eq!(r["harness"], "claude", "{r}");
    // Objects on the wire: name, kind, label and the event/prompt; the title/body templates a
    // skill may carry are not part of the row.
    assert_eq!(
        r["actions"],
        json!([
            {"name": "record-decision", "kind": "event", "label": "Record the decision", "event": "decision-record"},
            {"name": "ask-why", "kind": "prompt", "label": "Ask why", "prompt": "why was {id} renewed?"},
        ]),
        "{r}"
    );
    assert_eq!(r["cascade"]["target"], "produced", "{r}");
    assert_eq!(r["cascade"]["max_depth"], 2, "{r}");
    let d = by_id["decision-record"];
    assert!(d.get("summary").is_none(), "absent stays absent: {d}");
    assert!(d.get("harness").is_none(), "{d}");
    assert!(d.get("actions").is_none(), "{d}");
    assert!(d.get("cascade").is_none(), "{d}");
}

#[tokio::test]
async fn validate_lints_the_contract_keys() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);

    // A well-formed skill carries none of the new findings.
    let good = call(&p, &token, "validate", json!({ "content": skill(
        "summary: Short and sweet.\nharness: gemini\nactions:\n  - {name: renew, kind: event, label: Renew, event: renewal}\ncascade:\n  target: produced\n") })).await;
    for code in [
        "summary_missing",
        "summary_too_long",
        "harness_unknown",
        "action_skill_unknown",
    ] {
        assert!(
            issue(&good, code).is_none(),
            "{code} on a good skill: {good}"
        );
    }
    assert_eq!(good["ok"], true, "{good}");

    // No summary: a warning, not a refusal — the workbench shows the
    // description instead, but the author is told.
    let out = call(&p, &token, "validate", json!({ "content": skill("") })).await;
    let i = issue(&out, "summary_missing").unwrap_or_else(|| panic!("{out}"));
    assert_eq!(i["severity"], "warning", "{out}");
    assert_eq!(i["location"], "frontmatter.summary");
    assert_eq!(out["ok"], true, "a warning does not fail validation: {out}");

    // Too long: an error, with the limit in the message.
    let long = "x".repeat(201);
    let out = call(
        &p,
        &token,
        "validate",
        json!({ "content": skill(&format!("summary: {long}\n")) }),
    )
    .await;
    let i = issue(&out, "summary_too_long").unwrap_or_else(|| panic!("{out}"));
    assert_eq!(i["severity"], "error");
    assert!(i["message"].as_str().unwrap().contains("200"), "{out}");
    assert_eq!(out["ok"], false);

    // An unknown harness: an error naming the known ones.
    let out = call(
        &p,
        &token,
        "validate",
        json!({ "content": skill("summary: s.\nharness: muse2\n") }),
    )
    .await;
    let i = issue(&out, "harness_unknown").unwrap_or_else(|| panic!("{out}"));
    assert_eq!(i["location"], "frontmatter.harness");
    assert!(
        i["suggestion"].as_str().unwrap_or("").contains("claude"),
        "{out}"
    );
    assert_eq!(out["ok"], false);

    // An action naming a skill the corpus does not have is an error at the entry's `event`.
    let out = call(
        &p,
        &token,
        "validate",
        json!({ "content": skill("summary: s.\nactions:\n  - {name: a, kind: event, label: A, event: renewal}\n  - {name: b, kind: event, label: B, event: ghost}\n") }),
    )
    .await;
    let i = issue(&out, "action_skill_unknown").unwrap_or_else(|| panic!("{out}"));
    assert_eq!(i["location"], "frontmatter.actions[1].event", "{out}");
    assert!(i["message"].as_str().unwrap().contains("ghost"), "{out}");
    assert_eq!(out["ok"], false);

    // The old shorthand — a bare skill id — and a non-list are `action_invalid`, and the message
    // points at the object form.
    for (content, location) in [
        (
            "summary: s.\nactions:\n  - renewal\n",
            "frontmatter.actions[0]",
        ),
        ("summary: s.\nactions: renewal\n", "frontmatter.actions"),
    ] {
        let out = call(&p, &token, "validate", json!({ "content": skill(content) })).await;
        let i = issue(&out, "action_invalid").unwrap_or_else(|| panic!("{out}"));
        assert_eq!(i["location"], location, "{out}");
        assert!(i["message"].as_str().unwrap().contains("name"), "{out}");
        assert_eq!(out["ok"], false);
    }

    // Each way an object can be wrong has its own code.
    for (entry, code, location) in [
        (
            "{name: Bad Name, kind: event, label: L, event: renewal}",
            "action_name_invalid",
            "frontmatter.actions[0].name",
        ),
        (
            "{name: a, kind: shout, label: L}",
            "action_kind_unknown",
            "frontmatter.actions[0].kind",
        ),
        (
            "{name: a, kind: event, event: renewal}",
            "action_label_missing",
            "frontmatter.actions[0].label",
        ),
        (
            "{name: a, kind: event, label: L}",
            "action_event_missing",
            "frontmatter.actions[0].event",
        ),
        (
            "{name: a, kind: prompt, label: L}",
            "action_prompt_missing",
            "frontmatter.actions[0].prompt",
        ),
    ] {
        let out = call(
            &p,
            &token,
            "validate",
            json!({ "content": skill(&format!("summary: s.\nactions:\n  - {entry}\n")) }),
        )
        .await;
        let i = issue(&out, code).unwrap_or_else(|| panic!("{code}: {out}"));
        assert_eq!(i["location"], location, "{out}");
    }
    let out = call(
        &p,
        &token,
        "validate",
        json!({ "content": skill("summary: s.\nactions:\n  - {name: a, kind: event, label: A, event: renewal}\n  - {name: a, kind: prompt, label: B, prompt: p}\n") }),
    )
    .await;
    let i = issue(&out, "action_name_duplicate").unwrap_or_else(|| panic!("{out}"));
    assert_eq!(i["location"], "frontmatter.actions[1].name", "{out}");
}
