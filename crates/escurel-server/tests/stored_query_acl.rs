//! `run_stored_query` is an operator/analytics capability: it executes
//! pre-declared arbitrary SQL over the whole corpus (`pages`/`blocks`/`links`)
//! and returns arbitrary projected columns (aggregates, joins) — so there is
//! no reliable per-row owner to filter on. The sound ACL is therefore at the
//! *capability* level: only an admin (the operator) may run a stored query, a
//! non-admin member is refused. Mirrors the `admin_*` / `inspect_table` model.
//! Real gateway (TestIssuer auth) + real Indexer; no LLM.

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "stuttgart-ai";
const MEMBER: &str = "whatsapp:111";

const QUERY_SKILL: &str = "---\ntype: skill\nid: query\n\
    description: A stored query.\nvisibility: public\n---\n# query\n";
const TALK_SKILL: &str = "---\ntype: skill\nid: talk\ndescription: A program item.\n\
    visibility: public\n---\n# talk\n";
const COUNT_QUERY: &str = "---\ntype: instance\nskill: query\nid: count-by-skill\n\
    db: relational\nparams:\n  - {name: skill, type: text, required: true}\n\
    sql: \"SELECT count(*) AS n FROM pages WHERE skill = :skill AND page_type = 'instance'\"\n\
    ---\n# count-by-skill\n";
const KEYNOTE: &str =
    "---\ntype: instance\nskill: talk\nid: keynote\n---\n# Keynote\nDie Eröffnung.\n";

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("query", QUERY_SKILL)
                .skill("talk", TALK_SKILL)
                .instance("query", "count-by-skill", COUNT_QUERY)
                .instance("talk", "keynote", KEYNOTE)
                .done(),
        ),
        ..Default::default()
    })
    .await
}

/// Raw JSON-RPC call returning the whole envelope (so the test can inspect
/// the `error` member rather than panicking on it).
async fn raw(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    let resp = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": args },
        }))
        .send()
        .await
        .expect("post");
    assert_eq!(resp.status(), 200, "http status");
    resp.json().await.unwrap()
}

#[tokio::test]
async fn non_admin_member_cannot_run_stored_query() {
    let p = start().await;
    let member = p.mint_token_with_sub(TENANT, Role::Agent, MEMBER);

    let body = raw(
        &p,
        &member,
        "run_stored_query",
        json!({ "query_id": "count-by-skill", "params": { "skill": "talk" } }),
    )
    .await;

    assert!(
        body.get("error").is_some(),
        "a non-admin member must be refused run_stored_query, got: {body}"
    );
    // No result payload leaked.
    assert!(
        body.get("result").is_none(),
        "no rows leaked to a non-admin: {body}"
    );
    p.shutdown().await;
}

#[tokio::test]
async fn admin_runs_stored_query() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);

    let body = raw(
        &p,
        &admin,
        "run_stored_query",
        json!({ "query_id": "count-by-skill", "params": { "skill": "talk" } }),
    )
    .await;

    assert!(body.get("error").is_none(), "admin runs the query: {body}");
    let rows = body["result"]["structuredContent"]["rows"]
        .as_array()
        .expect("rows array");
    assert_eq!(rows.len(), 1, "count query returns one row: {body}");
    assert_eq!(rows[0]["n"], json!(1), "exactly one talk instance: {body}");
    p.shutdown().await;
}
