//! End-to-end tests for the MCP-over-HTTP tool dispatcher.
//!
//! Real running gateway, real Indexer (DuckDB + FsStore +
//! ZeroEmbedder), real reqwest client. The dispatcher path goes
//! from raw JSON-RPC over HTTP all the way down to DuckDB and
//! back, exactly as a production agent would.

use std::sync::Arc;

use bytes::Bytes;
use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_server::{AlwaysReady, ServerConfig, serve};
use escurel_storage::{FsStore, Key, LaneStore};
use serde_json::{Value, json};
use tempfile::TempDir;

const TENANT: &str = "acme";

const SKILL_CUSTOMER: (&str, &str) = (
    "markdown/skills/customer.md",
    "---\n\
     type: skill\n\
     id: customer\n\
     description: A buying entity.\n\
     ---\n\
     # customer\n",
);

const SKILL_MEETING: (&str, &str) = (
    "markdown/skills/meeting.md",
    "---\n\
     type: skill\n\
     id: meeting\n\
     description: An in-person or remote meeting.\n\
     required_frontmatter:\n\
       - at\n\
     ---\n\
     # meeting\n",
);

const SKILL_QUERY: (&str, &str) = (
    "markdown/skills/query.md",
    "---\n\
     type: skill\n\
     id: query\n\
     description: SQL view over the indexed corpus.\n\
     ---\n\
     # query\n",
);

const INSTANCE_ACME: (&str, &str) = (
    "markdown/instances/customer/acme-corp.md",
    "---\n\
     type: instance\n\
     skill: customer\n\
     id: acme-corp\n\
     ---\n\
     # Acme Corp\n\
     \n\
     Comparable: [[customer::globex-llc]].\n",
);

const INSTANCE_GLOBEX: (&str, &str) = (
    "markdown/instances/customer/globex-llc.md",
    "---\n\
     type: instance\n\
     skill: customer\n\
     id: globex-llc\n\
     ---\n\
     # Globex\n",
);

const QUERY_COUNT: (&str, &str) = (
    "markdown/instances/query/count-by-skill.md",
    "---\n\
     type: instance\n\
     skill: query\n\
     id: count-by-skill\n\
     db: relational\n\
     params:\n\
       - {name: skill, type: text, required: true}\n\
     sql: \"SELECT count(*) AS n FROM pages WHERE skill = :skill AND page_type = 'instance'\"\n\
     ---\n\
     # count-by-skill\n",
);

struct Harness {
    handle: escurel_server::ServerHandle,
    client: reqwest::Client,
    base_url: String,
    _store_dir: TempDir,
    _db_dir: TempDir,
}

async fn start_with_seeded_indexer() -> Harness {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Arc::new(Indexer::new(Arc::clone(&store), embedder, conn, TENANT).unwrap());

    for (path, body) in [
        SKILL_CUSTOMER,
        SKILL_MEETING,
        SKILL_QUERY,
        INSTANCE_ACME,
        INSTANCE_GLOBEX,
        QUERY_COUNT,
    ] {
        let key = Key::new(TENANT, path.to_owned()).unwrap();
        store
            .write(&key, Bytes::from_static(body.as_bytes()))
            .await
            .unwrap();
        indexer.update_page(path, body).await.unwrap();
    }
    indexer.refresh_fts().await.unwrap();

    let cfg = ServerConfig {
        listen: "127.0.0.1:0".to_owned(),
        version: "1.0.0-test".to_owned(),
        readiness: Arc::new(AlwaysReady),
        indexer: Some(indexer),
        verifier: None,
        quota: None,
    };
    let handle = serve(cfg).await.expect("server starts");
    let base_url = format!("http://{}", handle.local_addr);
    Harness {
        handle,
        client: reqwest::Client::new(),
        base_url,
        _store_dir: store_dir,
        _db_dir: db_dir,
    }
}

async fn call_tool(h: &Harness, name: &str, args: Value) -> Value {
    let resp = h
        .client
        .post(format!("{}/mcp", h.base_url))
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": name, "arguments": args },
        }))
        .send()
        .await
        .expect("post");
    assert_eq!(resp.status(), 200, "http status");
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["jsonrpc"], "2.0");
    assert_eq!(body["id"], 1);
    if body.get("error").is_some() {
        panic!("tool {name} returned error: {body}");
    }
    body["result"].clone()
}

#[tokio::test]
async fn list_skills_returns_three_skills() {
    let h = start_with_seeded_indexer().await;
    let result = call_tool(&h, "list_skills", json!({})).await;
    let skills = result["skills"].as_array().expect("skills array");
    let ids: Vec<&str> = skills.iter().filter_map(|s| s["id"].as_str()).collect();
    assert_eq!(ids.len(), 3);
    assert!(ids.contains(&"customer"));
    assert!(ids.contains(&"meeting"));
    assert!(ids.contains(&"query"));
    h.handle.shutdown().await;
}

#[tokio::test]
async fn list_instances_returns_filtered_by_skill() {
    let h = start_with_seeded_indexer().await;
    let result = call_tool(&h, "list_instances", json!({ "skill_id": "customer" })).await;
    let inst = result["instances"].as_array().unwrap();
    assert_eq!(inst.len(), 2);
    assert!(inst.iter().all(|i| i["skill"] == "customer"));
    h.handle.shutdown().await;
}

#[tokio::test]
async fn resolve_round_trips_through_http() {
    let h = start_with_seeded_indexer().await;
    let result = call_tool(
        &h,
        "resolve",
        json!({ "wikilink": "[[customer::acme-corp]]" }),
    )
    .await;
    assert_eq!(result["exists"], true);
    assert_eq!(result["page"]["skill"], "customer");
    assert_eq!(result["page"]["slug"], "acme-corp");
    h.handle.shutdown().await;
}

#[tokio::test]
async fn expand_returns_page_body_and_wikilinks() {
    let h = start_with_seeded_indexer().await;
    let result = call_tool(&h, "expand", json!({ "page_id": INSTANCE_ACME.0 })).await;
    assert!(result["body"].as_str().unwrap().contains("Acme Corp"));
    let wls = result["wikilinks_out"].as_array().unwrap();
    assert!(wls.iter().any(|w| w["id"] == "globex-llc"));
    h.handle.shutdown().await;
}

#[tokio::test]
async fn neighbours_returns_outbound_links() {
    let h = start_with_seeded_indexer().await;
    let result = call_tool(
        &h,
        "neighbours",
        json!({ "page_id": INSTANCE_ACME.0, "direction": "out" }),
    )
    .await;
    let edges = result["edges"].as_array().unwrap();
    let dst_pages: Vec<&str> = edges
        .iter()
        .filter_map(|e| e["dst_page"].as_str())
        .collect();
    assert!(dst_pages.contains(&"globex-llc"));
    h.handle.shutdown().await;
}

#[tokio::test]
async fn search_returns_hits_for_query() {
    let h = start_with_seeded_indexer().await;
    let result = call_tool(&h, "search", json!({ "q": "Acme", "k": 5 })).await;
    let hits = result["hits"].as_array().unwrap();
    assert!(!hits.is_empty());
    assert_eq!(result["granularity"], "block");
    h.handle.shutdown().await;
}

#[tokio::test]
async fn run_stored_query_routes_through_http() {
    let h = start_with_seeded_indexer().await;
    let result = call_tool(
        &h,
        "run_stored_query",
        json!({
            "query_id": "count-by-skill",
            "params": { "skill": "customer" }
        }),
    )
    .await;
    let rows = result["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["n"], 2);
    h.handle.shutdown().await;
}

#[tokio::test]
async fn unknown_tool_returns_jsonrpc_method_not_found() {
    let h = start_with_seeded_indexer().await;
    let resp = h
        .client
        .post(format!("{}/mcp", h.base_url))
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/call",
            "params": { "name": "nope", "arguments": {} }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], -32601);
    assert_eq!(body["id"], 7);
    h.handle.shutdown().await;
}

#[tokio::test]
async fn unknown_method_returns_jsonrpc_method_not_found() {
    let h = start_with_seeded_indexer().await;
    let resp = h
        .client
        .post(format!("{}/mcp", h.base_url))
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 8,
            "method": "foo/bar",
            "params": {}
        }))
        .send()
        .await
        .unwrap();
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], -32601);
    h.handle.shutdown().await;
}

#[tokio::test]
async fn update_page_round_trips_through_http() {
    let h = start_with_seeded_indexer().await;
    let body = "---\n\
                type: instance\n\
                skill: customer\n\
                id: brand-new\n\
                ---\n\
                # Brand New Customer\n\
                \n\
                Created via update_page over MCP.\n";

    let result = call_tool(
        &h,
        "update_page",
        json!({
            "page_id": "markdown/instances/customer/brand-new.md",
            "content": body
        }),
    )
    .await;
    assert_eq!(result["ok"], true);
    assert_eq!(result["issues"].as_array().unwrap().len(), 0);

    // The new page must now appear in list_instances.
    let inst = call_tool(&h, "list_instances", json!({ "skill_id": "customer" })).await;
    let count = inst["instances"].as_array().unwrap().len();
    assert_eq!(count, 3, "expected the 2 seeded + brand-new = 3");

    h.handle.shutdown().await;
}

#[tokio::test]
async fn update_page_propagates_parse_error_as_jsonrpc_internal() {
    let h = start_with_seeded_indexer().await;
    let resp = h
        .client
        .post(format!("{}/mcp", h.base_url))
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 99,
            "method": "tools/call",
            "params": {
                "name": "update_page",
                "arguments": {
                    "page_id": "x.md",
                    "content": "no frontmatter at all"
                }
            }
        }))
        .send()
        .await
        .unwrap();
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], -32603, "internal error: {body}");
    h.handle.shutdown().await;
}

#[tokio::test]
async fn tools_list_returns_all_eight_tools_with_schemas() {
    let h = start_with_seeded_indexer().await;
    let resp = h
        .client
        .post(format!("{}/mcp", h.base_url))
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 100,
            "method": "tools/list",
        }))
        .send()
        .await
        .unwrap();
    let body: Value = resp.json().await.unwrap();
    let tools = body["result"]["tools"].as_array().expect("tools array");
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    let expected = [
        "list_skills",
        "list_instances",
        "resolve",
        "expand",
        "neighbours",
        "search",
        "run_stored_query",
        "update_page",
    ];
    for name in expected {
        assert!(
            names.contains(&name),
            "tools/list missing {name}: {names:?}"
        );
    }
    // Every entry must carry the MCP-shape fields.
    for t in tools {
        assert!(t["description"].is_string());
        assert_eq!(t["inputSchema"]["type"], "object");
    }
    h.handle.shutdown().await;
}

#[tokio::test]
async fn malformed_jsonrpc_version_returns_invalid_request() {
    let h = start_with_seeded_indexer().await;
    let resp = h
        .client
        .post(format!("{}/mcp", h.base_url))
        .json(&json!({
            "jsonrpc": "1.0",
            "id": 9,
            "method": "tools/call",
            "params": { "name": "list_skills", "arguments": {} }
        }))
        .send()
        .await
        .unwrap();
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], -32600);
    h.handle.shutdown().await;
}
