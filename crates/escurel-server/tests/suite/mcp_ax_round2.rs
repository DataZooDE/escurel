//! Round-2 agent-usability contract (second crew review of the OKF branch), over real HTTP:
//! unknown arguments are refused, drafts carry `write_back` in their schema, empty successes are
//! errors, cursors are signed. One real gateway, real bearers; no mocks.

use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "stuttgart-ai";
const NOTE_SKILL: &str = "---\nkind: skill\nid: note\ndescription: A note.\n\
    visibility: public\n---\n# note\n";
const NOTE_A: &str = "---\nkind: instance\nskill: note\nid: a\n---\n# A\n";

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides::default(),
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("note", NOTE_SKILL)
                .instance("note", "a", NOTE_A)
                .done(),
        ),
    })
    .await
}

async fn call(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": args },
        }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("decode")
}

fn first_issue(v: &Value) -> Value {
    v["result"]["structuredContent"]["issues"][0].clone()
}

/// `filter`/`limt` used to be dropped silently, so the call "succeeded" with default behaviour.
#[tokio::test]
async fn unknown_arguments_are_refused_with_a_did_you_mean() {
    let p = start().await;
    let t = p.mint_token(TENANT, Role::Agent);

    let r = call(
        &p,
        &t,
        "list_instances",
        json!({ "skill_id": "note", "limt": 5 }),
    )
    .await;
    assert_eq!(r["result"]["isError"], json!(true), "{r}");
    let i = first_issue(&r);
    assert_eq!(i["code"], "invalid_argument", "{r}");
    let msg = i["message"].as_str().unwrap();
    assert!(
        msg.contains("limt") && msg.contains("limit"),
        "did-you-mean: {msg}"
    );
    assert!(
        msg.contains("skill_id"),
        "lists the valid parameters: {msg}"
    );

    // The documented aliases and the plain call keep working.
    let ok = call(&p, &t, "list_instances", json!({ "skill": "note" })).await;
    assert_eq!(ok["result"]["isError"], json!(false), "{ok}");
    let ok = call(&p, &t, "list_skills", json!({})).await;
    assert_eq!(ok["result"]["isError"], json!(false), "{ok}");
    let bad = call(&p, &t, "list_skills", json!({ "filter": "x" })).await;
    assert_eq!(first_issue(&bad)["code"], "invalid_argument", "{bad}");
}
