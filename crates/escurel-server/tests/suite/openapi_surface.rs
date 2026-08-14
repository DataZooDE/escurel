//! `GET /openapi.json` honesty + MCP `outputSchema` (2026-08-14 API
//! review R2/R6/B5).
//!
//! The document used to describe exactly one path (`/mcp`) with a
//! response of "JSON-RPC result or error object" — no REST routes (the
//! one place OpenAPI genuinely fits), no security schemes, no output
//! shapes anywhere. A generated client could send but not type what
//! comes back.

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

#[tokio::test]
async fn openapi_documents_the_rest_routes_and_security() {
    let p = start().await;
    let doc: Value = reqwest::get(format!("{}/openapi.json", p.base_url()))
        .await
        .expect("get")
        .json()
        .await
        .expect("decode");

    // The REST routes exist in the document that claims to serve
    // "non-MCP HTTP clients".
    for path in [
        "/mcp",
        "/ingest",
        "/ingest/upload",
        "/blob/{page_id}",
        "/healthz",
        "/version",
    ] {
        assert!(
            doc["paths"].get(path).is_some(),
            "path `{path}` missing from openapi.json: {:?}",
            doc["paths"]
                .as_object()
                .map(|m| m.keys().collect::<Vec<_>>())
        );
    }
    // Bearer auth is declared, and /mcp requires it.
    assert_eq!(
        doc["components"]["securitySchemes"]["bearerAuth"]["scheme"],
        json!("bearer"),
        "securitySchemes.bearerAuth: {doc}"
    );
    assert!(
        doc["paths"]["/mcp"]["post"]["security"].is_array(),
        "/mcp declares its bearer requirement: {}",
        doc["paths"]["/mcp"]["post"]
    );
    // The ingest request body is typed, not a mystery object.
    let ingest_props = &doc["paths"]["/ingest"]["post"]["requestBody"]["content"]["application/json"]
        ["schema"]["properties"];
    assert!(
        ingest_props.get("blob_id").is_some() && ingest_props.get("event_id").is_some(),
        "/ingest request schema names its fields: {ingest_props}"
    );
}

#[tokio::test]
async fn core_tools_carry_output_schemas() {
    let p = start().await;
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
        .expect("decode");
    let tools = body["result"]["tools"].as_array().expect("tools");
    let get = |name: &str| -> &Value {
        tools
            .iter()
            .find(|t| t["name"] == json!(name))
            .unwrap_or_else(|| panic!("tool {name} missing"))
    };

    // The write envelope is the one shape every consumer branches on.
    for write_tool in ["update_page", "delete_page", "move_page"] {
        let os = &get(write_tool)["outputSchema"];
        assert!(
            os["properties"]["ok"].is_object() && os["properties"]["issues"].is_object(),
            "`{write_tool}` must declare the {{ok, issues[]}} envelope, got {os}"
        );
    }
    // The core read declares its hits.
    let search = &get("search")["outputSchema"];
    assert!(
        search["properties"]["hits"].is_object(),
        "`search` output schema declares hits: {search}"
    );
    // The paginated listings declare the cursor contract.
    let inbox = &get("list_inbox")["outputSchema"];
    assert!(
        inbox["properties"]["events"].is_object() && inbox["properties"]["next_cursor"].is_object(),
        "`list_inbox` declares events + next_cursor: {inbox}"
    );
}
