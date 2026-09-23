//! `fields[].render` and `blocks[]` on a skill page (knowledge-workbench
//! backend P3-5 — BRD FR-S-6/7). Both are declarations the workbench renders
//! from: `render` is a per-field hint (`text | markdown | date | datetime |
//! money | link | badge`), `blocks` the declared layout of an instance body
//! (`[{anchor, title, kind}]`). `list_skills` passes both through; `validate`
//! lints them (`field_render_unknown`, a warning; `blocks_malformed`, an
//! error). No gateway behaviour keys off either. Real gateway, real DuckDB.

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "carl";
const ACCOUNT: &str = "\
---
type: skill
id: account
description: A customer account.
summary: One account per customer.
fields:
  - {name: arr_eur, kind: float, min: 0, render: money}
  - {name: notes, kind: string, render: markdown}
  - {name: opened, kind: date}
blocks:
  - {anchor: summary, title: Summary, kind: markdown}
  - {anchor: timeline, title: Timeline, kind: events}
---
# account
";
const NOTE: &str = "---\ntype: skill\nid: note\ndescription: d.\nsummary: s.\n\
fields:\n  - {name: status, kind: string}\n---\n# note\n";

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("account", ACCOUNT)
                .skill("note", NOTE)
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
    format!("---\ntype: skill\nid: x\ndescription: d.\nsummary: s.\n{extra}---\n# x\n")
}

fn issue<'a>(out: &'a Value, code: &str) -> Option<&'a Value> {
    out["issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["code"] == code)
}

#[tokio::test]
async fn list_skills_passes_through_field_render_hints_and_the_declared_blocks() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);
    let out = call(&p, &token, "list_skills", json!({})).await;
    let by_id: std::collections::HashMap<&str, &Value> = out["skills"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["id"].as_str().unwrap(), s))
        .collect();

    let a = by_id["account"];
    let fields = a["fields"].as_array().unwrap();
    assert_eq!(fields[0]["name"], "arr_eur");
    assert_eq!(fields[0]["render"], "money", "{a}");
    assert_eq!(fields[1]["render"], "markdown", "{a}");
    assert!(
        fields[2].get("render").is_none(),
        "no hint declared, none reported: {a}"
    );
    assert_eq!(
        a["blocks"],
        json!([
            { "anchor": "summary", "title": "Summary", "kind": "markdown" },
            { "anchor": "timeline", "title": "Timeline", "kind": "events" },
        ]),
        "verbatim, in the author's order: {a}"
    );

    let n = by_id["note"];
    assert!(n["fields"][0].get("render").is_none(), "{n}");
    assert!(n.get("blocks").is_none(), "absent stays absent: {n}");
}

#[tokio::test]
async fn validate_lints_render_hints_and_blocks() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);

    // Well-formed: none of the new findings.
    let good = call(
        &p,
        &token,
        "validate",
        json!({ "content": skill(
        "fields:\n  - {name: due, kind: date, render: date}\n  - {name: url, render: link}\n\
         blocks:\n  - {anchor: body, title: Body, kind: markdown}\n  - {anchor: refs}\n") }),
    )
    .await;
    for code in ["field_render_unknown", "blocks_malformed"] {
        assert!(
            issue(&good, code).is_none(),
            "{code} on a good skill: {good}"
        );
    }
    assert_eq!(good["ok"], true, "{good}");

    // An unknown render hint: a warning at the field, naming the known
    // ones; the value is still passed through (a client ignores what it
    // does not know).
    let out = call(
        &p,
        &token,
        "validate",
        json!({ "content": skill("fields:\n  - {name: arr_eur, kind: float, render: sparkle}\n") }),
    )
    .await;
    let i = issue(&out, "field_render_unknown").unwrap_or_else(|| panic!("{out}"));
    assert_eq!(i["severity"], "warning", "{out}");
    assert_eq!(i["location"], "frontmatter.fields.arr_eur.render");
    assert!(
        i["suggestion"].as_str().unwrap_or("").contains("money"),
        "{out}"
    );
    assert_eq!(out["ok"], true, "a warning does not fail validation: {out}");

    // `blocks:` that is not a sequence: an error at the key.
    let out = call(
        &p,
        &token,
        "validate",
        json!({ "content": skill("blocks: summary\n") }),
    )
    .await;
    let i = issue(&out, "blocks_malformed").unwrap_or_else(|| panic!("{out}"));
    assert_eq!(i["severity"], "error", "{out}");
    assert_eq!(i["location"], "frontmatter.blocks");
    assert_eq!(out["ok"], false);

    // An entry without an `anchor`: an error at that entry.
    let out = call(
        &p,
        &token,
        "validate",
        json!({ "content": skill("blocks:\n  - {anchor: a, title: A}\n  - {title: Orphan}\n") }),
    )
    .await;
    let i = issue(&out, "blocks_malformed").unwrap_or_else(|| panic!("{out}"));
    assert_eq!(i["location"], "frontmatter.blocks[1]", "{out}");
    assert_eq!(out["ok"], false);
}
