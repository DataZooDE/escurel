//! `max_items: <n>` on a field: a page whose field holds more than `n` values
//! is refused by `update_page`, `create_draft` and `promote_draft`; at the limit
//! it lands. A malformed declaration refuses the skill page itself, and
//! `list_skills` publishes the cap. Real gateway, real DuckDB, neutral skills.

use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const TENANT: &str = "stuttgart-ai";

fn task_skill(max_items: &str) -> String {
    format!(
        "---\nkind: skill\nid: task\ndescription: A unit of work.\nvisibility: public\n\
         fields:\n  - {{name: after, kind: link, to: task, max_items: {max_items}}}\n---\n# task\n"
    )
}

fn task(id: &str, after: &[&str]) -> String {
    let links = match after {
        [] => String::new(),
        [one] => format!("after: \"[[task::{one}]]\"\n"),
        many => format!(
            "after: [{}]\n",
            many.iter()
                .map(|a| format!("\"[[task::{a}]]\""))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    };
    format!("---\nkind: instance\nskill: task\nid: {id}\n{links}---\n# {id}\n")
}

fn pid(id: &str) -> String {
    format!("markdown/instances/task/{id}.md")
}

fn sha(s: &str) -> String {
    format!("{:x}", Sha256::digest(s.as_bytes()))
}

async fn start() -> EscurelProcess {
    let fixtures = FixtureBuilder::new()
        .tenant(TENANT)
        .skill("task", task_skill("2").as_str())
        .instance("task", "a", task("a", &[]).as_str())
        .instance("task", "b", task("b", &[]).as_str())
        .instance("task", "c", task("c", &[]).as_str())
        .done();
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides::default(),
        fixtures: Some(fixtures),
    })
    .await
}

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

fn refused_over_the_cap(v: &Value) {
    assert_eq!(v["ok"], json!(false), "{v}");
    let issue = v["issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["code"] == "field_max_items")
        .unwrap_or_else(|| panic!("no field_max_items: {v}"));
    assert_eq!(issue["family"], json!("field_max_items"), "{v}");
    assert_eq!(issue["location"], json!("frontmatter.after"), "{v}");
}

#[tokio::test]
async fn update_page_refuses_more_links_than_declared_and_accepts_the_limit() {
    let p = start().await;
    let over = call(
        &p,
        "update_page",
        json!({ "page_id": pid("x"), "content": task("x", &["a", "b", "c"]) }),
    )
    .await;
    refused_over_the_cap(&over);

    for (id, after) in [("x", &["a", "b"][..]), ("y", &["a"][..])] {
        let v = call(
            &p,
            "update_page",
            json!({ "page_id": pid(id), "content": task(id, after) }),
        )
        .await;
        assert_eq!(v["ok"], json!(true), "{id}: {v}");
    }
}

#[tokio::test]
async fn create_draft_and_validate_refuse_more_links_than_declared() {
    let p = start().await;
    let over = task("x", &["a", "b", "c"]);
    let v = call(
        &p,
        "validate",
        json!({ "as_page_id": pid("x"), "content": over }),
    )
    .await;
    assert!(
        v["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["code"] == "field_max_items"),
        "{v}"
    );
    let d = call(
        &p,
        "create_draft",
        json!({ "target_page_id": pid("x"), "content": over, "base_sha256": "" }),
    )
    .await;
    refused_over_the_cap(&d);

    let at = call(
        &p,
        "create_draft",
        json!({ "target_page_id": pid("x"), "content": task("x", &["a", "b"]), "base_sha256": "" }),
    )
    .await;
    assert_eq!(at["ok"], json!(true), "{at}");
}

#[tokio::test]
async fn promote_draft_refuses_a_draft_the_tightened_cap_no_longer_admits() {
    let p = start().await;
    let content = task("x", &["a", "b"]);
    let d = call(
        &p,
        "create_draft",
        json!({ "target_page_id": pid("x"), "content": content, "base_sha256": "" }),
    )
    .await;
    assert_eq!(d["ok"], json!(true), "{d}");
    let id = d["draft"]["draft_id"].as_str().unwrap().to_owned();

    let skill = "markdown/skills/task.md";
    let current = call(&p, "expand", json!({ "page_id": skill, "raw": true })).await;
    let tightened = call(
        &p,
        "update_page",
        json!({
            "page_id": skill,
            "content": task_skill("1"),
            "base_sha256": sha(current["content"].as_str().unwrap_or_default()),
        }),
    )
    .await;
    assert_eq!(tightened["ok"], json!(true), "{tightened}");

    let r = call(&p, "promote_draft", json!({ "draft_id": id })).await;
    refused_over_the_cap(&r);
    let e = call(&p, "expand", json!({ "page_id": pid("x"), "raw": true })).await;
    assert!(
        e["content"].as_str().unwrap_or_default().is_empty() || e.get("content").is_none(),
        "nothing landed: {e}"
    );
}

#[tokio::test]
async fn a_malformed_cap_refuses_the_skill_page() {
    let p = start().await;
    for bad in ["0", "-2", "1.5", "\"3\"", "many"] {
        let content = task_skill(bad).replace("id: task", "id: bad");
        let page = "markdown/skills/bad.md";
        let v = call(
            &p,
            "validate",
            json!({ "as_page_id": page, "content": content }),
        )
        .await;
        assert!(
            v["issues"].as_array().unwrap().iter().any(|i| {
                i["code"] == "fields_malformed"
                    && i["location"] == "frontmatter.fields.after.max_items"
            }),
            "{bad}: {v}"
        );
        let w = call(
            &p,
            "update_page",
            json!({ "page_id": page, "content": content }),
        )
        .await;
        assert_eq!(w["ok"], json!(false), "{bad}: {w}");
    }
}

#[tokio::test]
async fn list_skills_publishes_the_cap() {
    let p = start().await;
    let r = call(&p, "list_skills", json!({})).await;
    let task = r["skills"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "task")
        .unwrap()
        .clone();
    assert_eq!(task["fields"][0]["max_items"], json!(2), "{task}");
}
