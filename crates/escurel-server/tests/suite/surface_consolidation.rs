//! Tool-surface consolidation (2026-08-14 API review, minimalism
//! findings 3-4): the deprecated legacy tool is GONE and the provenance
//! quartet collapsed to two tools.
//!
//! - `run_stored_query` (self-documented "legacy... use query_instance"
//!   since it shipped, DEPRECATED on the wire since #395) is removed.
//! - `provenance_path` folds into `provenance_ancestry` as an optional
//!   `to_page` (same schema family, same ACL fail-closed rule).
//! - `expectation_drift` / `abandoned_paths` — zero consumers anywhere —
//!   fold into `provenance_report(kind: "drift" | "abandoned")`.

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "lab";
const A1: &str = "markdown/instances/pub_node/a1.md";
const C1: &str = "markdown/instances/pub_node/c1.md";

fn skill(id: &str, extra: &str) -> String {
    format!("---\ntype: skill\nid: {id}\ndescription: {id}.\n{extra}---\n# {id}\n")
}

/// The provenance_path chain fixture: a1 → b1 → c1 over `derived_from`.
fn fixtures() -> FixtureBuilder {
    FixtureBuilder::new()
        .tenant(TENANT)
        .skill("pub_node", skill("pub_node", "visibility: public\n"))
        .skill("mid", skill("mid", "visibility: public\n"))
        .instance(
            "pub_node",
            "a1",
            "---\ntype: instance\nskill: pub_node\nid: a1\n\
             derived_from: \"[[mid::b1]]\"\n---\n# a1\n",
        )
        .instance(
            "mid",
            "b1",
            "---\ntype: instance\nskill: mid\nid: b1\n\
             derived_from: \"[[pub_node::c1]]\"\n---\n# b1\n",
        )
        .instance(
            "pub_node",
            "c1",
            "---\ntype: instance\nskill: pub_node\nid: c1\n---\n# c1\n",
        )
        .done()
}

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(fixtures()),
        ..Default::default()
    })
    .await
}

async fn call(p: &EscurelProcess, role: Role, name: &str, args: Value) -> Value {
    let token = p.mint_token(TENANT, role);
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
        .expect("json")
}

async fn tool_names(p: &EscurelProcess) -> Vec<String> {
    let token = p.mint_token(TENANT, Role::Admin);
    let body: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json");
    body["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn the_retired_tools_are_gone_from_the_surface() {
    let p = start().await;
    let names = tool_names(&p).await;
    for gone in [
        "run_stored_query",
        "provenance_path",
        "expectation_drift",
        "abandoned_paths",
    ] {
        assert!(
            !names.contains(&gone.to_owned()),
            "`{gone}` must no longer be advertised"
        );
    }
    assert!(
        names.contains(&"provenance_report".to_owned()),
        "the consolidated report tool is advertised: {names:?}"
    );

    // And a call is a refusal, not a silent success.
    let out = call(
        &p,
        Role::Admin,
        "run_stored_query",
        json!({ "query_id": "x" }),
    )
    .await;
    assert!(
        out.get("error").is_some(),
        "calling the retired tool must refuse: {out}"
    );
}

#[tokio::test]
async fn ancestry_with_to_page_answers_the_path_question() {
    let p = start().await;
    // a1 reaches c1 in 2 hops (the old provenance_path contract).
    let out = call(
        &p,
        Role::Agent,
        "provenance_ancestry",
        json!({ "page_id": A1, "to_page": C1 }),
    )
    .await;
    assert!(out.get("error").is_none(), "{out}");
    let s = &out["result"]["structuredContent"];
    assert_eq!(s["reachable"], json!(true), "{out}");
    assert_eq!(s["depth"], json!(2), "{out}");
    assert_eq!(
        s["path"].as_array().map(Vec::len),
        Some(3),
        "a1→b1→c1: {out}"
    );

    // Without `to_page` the classic ancestry-walk shape is unchanged.
    let walk = call(
        &p,
        Role::Agent,
        "provenance_ancestry",
        json!({ "page_id": A1 }),
    )
    .await;
    assert!(walk.get("error").is_none(), "{walk}");
    assert!(
        walk["result"]["structuredContent"]["hops"].is_array(),
        "ancestry walk still returns hops: {walk}"
    );
}

#[tokio::test]
async fn provenance_report_serves_both_kinds() {
    let p = start().await;
    for kind in ["drift", "abandoned"] {
        let out = call(
            &p,
            Role::Agent,
            "provenance_report",
            json!({ "kind": kind }),
        )
        .await;
        assert!(out.get("error").is_none(), "kind {kind}: {out}");
        let s = &out["result"]["structuredContent"];
        assert!(
            s["rows"].is_array(),
            "report kind `{kind}` returns rows: {out}"
        );
    }
    // An unknown kind is a typed refusal.
    let bad = call(
        &p,
        Role::Agent,
        "provenance_report",
        json!({ "kind": "nonsense" }),
    )
    .await;
    assert_eq!(bad["error"]["code"], json!(-32602), "{bad}");
}
