//! `required_when` takes a list of values; `list_skills` publishes the declared
//! condition as `[other_field, [value, ...]]` and `update_page` blocks on it.
//! Real gateway, neutral skill.

use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "stuttgart-ai";
const REVIEW: &str = "---\nkind: skill\nid: review\ndescription: A review.\nvisibility: public\n\
    fields:\n  - {name: status, kind: enum, values: [open, resolved, dismissed]}\n  \
    - {name: note, kind: string, required_when: {status: [resolved, dismissed]}}\n  \
    - {name: owner, kind: string, required_when: {status: open}}\n---\n# review\n";

async fn call(p: &EscurelProcess, tool: &str, args: Value) -> Value {
    let token = p.mint_token(TENANT, Role::Admin);
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

#[tokio::test]
async fn a_value_list_is_enforced_and_published() {
    let fixtures = FixtureBuilder::new()
        .tenant(TENANT)
        .skill("review", REVIEW)
        .done();
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides::default(),
        fixtures: Some(fixtures),
    })
    .await;

    let skills = call(&p, "list_skills", json!({})).await;
    let text = skills.to_string();
    assert!(
        text.contains(r#""required_when":["status",["resolved","dismissed"]]"#),
        "{text}"
    );
    assert!(
        text.contains(r#""required_when":["status",["open"]]"#),
        "{text}"
    );

    let page =
        |extra: &str| format!("---\nkind: instance\nskill: review\nid: r1\n{extra}---\n# r1\n");
    let id = "markdown/instances/review/r1.md";
    let refused = call(
        &p,
        "update_page",
        json!({ "page_id": id, "content": page("status: dismissed\n") }),
    )
    .await;
    assert_eq!(refused["ok"], json!(false), "{refused}");
    assert_eq!(
        refused["issues"][0]["code"],
        json!("field_required_when"),
        "{refused}"
    );
    let ok = call(
        &p,
        "update_page",
        json!({ "page_id": id, "content": page("status: dismissed\nnote: duplicate\n") }),
    )
    .await;
    assert_eq!(ok["ok"], json!(true), "{ok}");
}
