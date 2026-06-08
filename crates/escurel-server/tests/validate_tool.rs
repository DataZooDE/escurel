//! End-to-end tests for the `validate` agent tool (the 12th tool).
//!
//! Real running gateway, real Indexer (DuckDB + FsStore +
//! ZeroEmbedder), exercised over MCP-over-HTTP (raw JSON-RPC via
//! reqwest). `validate` is a dry run — it must produce the same issue
//! list `update_page` would, but commit nothing. (The gRPC arm was
//! removed when gRPC was dropped; HTTP is now the only transport.)

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "acme";

const CUSTOMER_SKILL: &str = "---\n\
type: skill\n\
id: customer\n\
description: A buying organisation.\n\
required_frontmatter: [name]\n\
optional_frontmatter: [tier]\n\
---\n\
# customer\n";

const ACME_INSTANCE: &str = "---\n\
type: instance\n\
skill: customer\n\
id: acme\n\
name: Acme Corp\n\
---\n\
# Acme Corp\n";

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("customer", CUSTOMER_SKILL)
                .instance("customer", "acme", ACME_INSTANCE)
                .done(),
        ),
        ..Default::default()
    })
    .await
}

/// Call `validate` over MCP-over-HTTP, returning the `result`
/// payload. Panics on any JSON-RPC error envelope.
async fn validate_mcp(p: &EscurelProcess, content: &str, as_page_id: Option<&str>) -> Value {
    let token = p.mint_token(TENANT, Role::Agent);
    let mut args = json!({ "content": content });
    if let Some(id) = as_page_id {
        args["as_page_id"] = json!(id);
    }
    let resp = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": "validate", "arguments": args },
        }))
        .send()
        .await
        .expect("post");
    assert_eq!(resp.status(), 200, "http status");
    let body: Value = resp.json().await.unwrap();
    if body.get("error").is_some() {
        panic!("validate returned error: {body}");
    }
    // tools/call results are MCP-shaped; the payload is under
    // `structuredContent`.
    body["result"]["structuredContent"].clone()
}

#[tokio::test]
async fn validate_clean_content_returns_no_issues() {
    let p = start().await;
    let content = "---\n\
                   type: instance\n\
                   skill: customer\n\
                   id: globex\n\
                   name: Globex\n\
                   ---\n\
                   # Globex\n\nSee [[customer::acme]].\n";
    let result = validate_mcp(&p, content, Some("markdown/instances/customer/globex.md")).await;
    let issues = result["issues"].as_array().expect("issues array");
    assert!(
        issues.is_empty(),
        "clean content should yield no issues, got: {issues:?}"
    );
    p.shutdown().await;
}

#[tokio::test]
async fn validate_malformed_frontmatter_returns_issue() {
    let p = start().await;
    // Frontmatter that is not valid YAML mapping (a tab + broken
    // indentation under a key produces a YAML scan error).
    let content = "---\n\
                   type: instance\n\
                   skill: customer\n\
                   id: [unclosed\n\
                   ---\n\
                   # broken\n";
    let result = validate_mcp(&p, content, None).await;
    let issues = result["issues"].as_array().expect("issues array");
    assert!(
        !issues.is_empty(),
        "malformed frontmatter must surface an issue"
    );
    assert_eq!(issues[0]["severity"], "error");
    p.shutdown().await;
}

#[tokio::test]
async fn validate_unknown_skill_reference_returns_issue() {
    let p = start().await;
    let content = "---\n\
                   type: instance\n\
                   skill: customer\n\
                   id: globex\n\
                   name: Globex\n\
                   ---\n\
                   # Globex\n\nRefers to [[nonexistent::whoever]].\n";
    let result = validate_mcp(&p, content, None).await;
    let issues = result["issues"].as_array().expect("issues array");
    assert!(
        issues
            .iter()
            .any(|i| i["code"] == "unknown_skill" && i["severity"] == "error"),
        "expected an unknown_skill error, got: {issues:?}"
    );
    p.shutdown().await;
}

#[tokio::test]
async fn validate_missing_required_frontmatter_returns_issue() {
    let p = start().await;
    // `customer` declares required_frontmatter [name]; omit it.
    let content = "---\n\
                   type: instance\n\
                   skill: customer\n\
                   id: globex\n\
                   ---\n\
                   # Globex\n";
    let result = validate_mcp(&p, content, None).await;
    let issues = result["issues"].as_array().expect("issues array");
    assert!(
        issues
            .iter()
            .any(|i| i["code"] == "frontmatter_required_key_missing"),
        "expected a missing-required-key issue, got: {issues:?}"
    );
    p.shutdown().await;
}

#[tokio::test]
async fn validate_does_not_commit() {
    let p = start().await;
    let new_page = "markdown/instances/customer/ephemeral.md";
    let content = "---\n\
                   type: instance\n\
                   skill: customer\n\
                   id: ephemeral\n\
                   name: Ephemeral\n\
                   ---\n\
                   # Ephemeral\n";
    let _ = validate_mcp(&p, content, Some(new_page)).await;

    // The page must NOT have been created: resolve reports it absent.
    let resp = reqwest::Client::new()
        .post(p.mcp_url())
        .header(
            "authorization",
            format!("Bearer {}", p.mint_token(TENANT, Role::Agent)),
        )
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "resolve",
                "arguments": { "wikilink": "[[customer::ephemeral]]" }
            },
        }))
        .send()
        .await
        .unwrap();
    let body: Value = resp.json().await.unwrap();
    assert_eq!(
        body["result"]["structuredContent"]["exists"], false,
        "validate must not commit the page: {body}"
    );
    p.shutdown().await;
}

#[tokio::test]
async fn validate_in_tools_list() {
    let p = start().await;
    let resp = reqwest::Client::new()
        .post(p.mcp_url())
        .header(
            "authorization",
            format!("Bearer {}", p.mint_token(TENANT, Role::Agent)),
        )
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/list",
        }))
        .send()
        .await
        .unwrap();
    let body: Value = resp.json().await.unwrap();
    let tools = body["result"]["tools"].as_array().expect("tools array");
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    assert!(
        names.contains(&"validate"),
        "tools/list must advertise validate: {names:?}"
    );
    p.shutdown().await;
}
