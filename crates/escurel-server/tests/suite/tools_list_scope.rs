//! Role-scoped `tools/list` over real HTTP (2026-08-14 API review, the
//! panel's single highest-consensus finding).
//!
//! Every `tools/list` entry carries a `scope: "agent" | "admin"` label,
//! declared at the definition site like `execution`. An **agent-role**
//! token receives only the agent-callable subset — before this, every
//! caller got all 69 schemas, 41 of which could only ever answer
//! `-32001`, so an LLM harness burned context on tools it could not
//! call (and the docs had drifted into claiming the filter existed).
//! Admin tokens and verifier-less dev mode still see everything.

use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "stuttgart-ai";

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides::default(),
        fixtures: Some(FixtureBuilder::new().tenant(TENANT).done()),
    })
    .await
}

async fn tools_list(p: &EscurelProcess, token: Option<&str>) -> Vec<Value> {
    let mut req = reqwest::Client::new().post(p.mcp_url());
    if let Some(t) = token {
        req = req.header("authorization", format!("Bearer {t}"));
    }
    let body: Value = req
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("decode");
    body["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("tools shape: {body}"))
        .clone()
}

#[tokio::test]
async fn every_entry_carries_a_scope_label() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);
    let tools = tools_list(&p, Some(&admin)).await;
    assert!(tools.len() > 60, "admin sees the whole surface");
    for t in &tools {
        let scope = t["scope"].as_str();
        assert!(
            matches!(scope, Some("agent") | Some("admin")),
            "tool `{}` must declare scope agent|admin, got {scope:?}",
            t["name"]
        );
    }
}

#[tokio::test]
async fn agent_role_sees_only_callable_tools() {
    let p = start().await;
    let agent = p.mint_token(TENANT, Role::Agent);
    let admin = p.mint_token(TENANT, Role::Admin);

    let agent_view = tools_list(&p, Some(&agent)).await;
    let admin_view = tools_list(&p, Some(&admin)).await;

    let names = |ts: &[Value]| -> Vec<String> {
        ts.iter()
            .map(|t| t["name"].as_str().unwrap().to_owned())
            .collect()
    };
    let agent_names = names(&agent_view);

    // The core loop is present…
    for core in ["search", "expand", "update_page", "capture_event"] {
        assert!(agent_names.contains(&core.to_owned()), "{core} missing");
    }
    // …the admin surface is not.
    for admin_only in [
        "tenant_create",
        "purge_page",
        "admin_index_query",
        "rebuild",
    ] {
        assert!(
            !agent_names.contains(&admin_only.to_owned()),
            "`{admin_only}` is dispatch-refused for agents and must not be \
             advertised to them"
        );
    }
    // The filter is exactly the scope label — nothing else dropped.
    let advertised_agent: Vec<String> = admin_view
        .iter()
        .filter(|t| t["scope"] == json!("agent"))
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        agent_names, advertised_agent,
        "agent view == the scope:agent subset, in the same order"
    );
    assert!(
        agent_view.len() < admin_view.len(),
        "the agent view is a strict subset"
    );
}
