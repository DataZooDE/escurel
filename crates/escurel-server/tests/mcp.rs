//! End-to-end tests for the MCP-over-HTTP tool dispatcher.
//!
//! Real running gateway, real Indexer (DuckDB + FsStore +
//! ZeroEmbedder), real reqwest client. The dispatcher path goes
//! from raw JSON-RPC over HTTP all the way down to DuckDB and
//! back, exactly as a production agent would.

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts};
use serde_json::{Value, json};

const SKILL_CUSTOMER_ID: &str = "customer";
const SKILL_CUSTOMER_BODY: &str = "---\n\
     type: skill\n\
     id: customer\n\
     description: A buying entity.\n\
     ---\n\
     # customer\n";

const SKILL_MEETING_BODY: &str = "---\n\
     type: skill\n\
     id: meeting\n\
     description: An in-person or remote meeting.\n\
     required_frontmatter:\n\
       - at\n\
     ---\n\
     # meeting\n";

const SKILL_QUERY_BODY: &str = "---\n\
     type: skill\n\
     id: query\n\
     description: SQL view over the indexed corpus.\n\
     ---\n\
     # query\n";

// A dynamic-workflow plan skill: markdown-file-backed, but declares
// `backend: { kind: workflow }` and carries a `phases:` orchestration block.
// Inline-flow YAML avoids the indented-block whitespace pitfall of the shared
// 5-space test-literal prefix. The `phases:` block is parsed by the
// `escurel-runner-workflow` reducer (PR-2), not the index; PR-1 only needs the
// kind to surface through `list_skills`.
const SKILL_DEEP_RESEARCH_BODY: &str = "---\n\
     type: skill\n\
     id: deep-research\n\
     description: Fan-out web search, adversarially verify, synthesize a report.\n\
     backend: {kind: workflow}\n\
     phases: [{id: scope, produces: research-angle, fan_out: 1}, {id: synthesize, produces: research-report, fan_out: 1}]\n\
     ---\n\
     # deep-research\n";

const MEETING_OLD_BODY: &str = "---\n\
     type: instance\n\
     skill: meeting\n\
     id: kickoff\n\
     at: 2026-01-10T10:00:00Z\n\
     ---\n\
     # Kickoff\n";
const MEETING_NEW_BODY: &str = "---\n\
     type: instance\n\
     skill: meeting\n\
     id: qbr\n\
     at: 2026-04-12T10:00:00Z\n\
     ---\n\
     # QBR\n";

// A scenario-B-only customer: hidden in the base view, visible under B.
const INSTANCE_FUTURE_B_BODY: &str = "---\n\
     type: instance\n\
     skill: customer\n\
     id: future-corp\n\
     scenario: B\n\
     ---\n\
     # Future Corp (scenario B)\n";

const INSTANCE_ACME_PATH: &str = "markdown/instances/customer/acme-corp.md";
const INSTANCE_ACME_BODY: &str = "---\n\
     type: instance\n\
     skill: customer\n\
     id: acme-corp\n\
     ---\n\
     # Acme Corp\n\
     \n\
     Comparable: [[customer::globex-llc]].\n";

const INSTANCE_GLOBEX_BODY: &str = "---\n\
     type: instance\n\
     skill: customer\n\
     id: globex-llc\n\
     ---\n\
     # Globex\n";

const QUERY_COUNT_BODY: &str = "---\n\
     type: instance\n\
     skill: query\n\
     id: count-by-skill\n\
     db: relational\n\
     params:\n\
       - {name: skill, type: text, required: true}\n\
     sql: \"SELECT count(*) AS n FROM pages WHERE skill = :skill AND page_type = 'instance'\"\n\
     ---\n\
     # count-by-skill\n";

async fn start_with_seeded_indexer() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::Disabled,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant("acme")
                .skill(SKILL_CUSTOMER_ID, SKILL_CUSTOMER_BODY)
                .skill("meeting", SKILL_MEETING_BODY)
                .skill("query", SKILL_QUERY_BODY)
                .instance("customer", "acme-corp", INSTANCE_ACME_BODY)
                .instance("customer", "globex-llc", INSTANCE_GLOBEX_BODY)
                .instance("query", "count-by-skill", QUERY_COUNT_BODY)
                .instance("meeting", "kickoff", MEETING_OLD_BODY)
                .instance("meeting", "qbr", MEETING_NEW_BODY)
                .instance("customer", "future-corp", INSTANCE_FUTURE_B_BODY)
                .done(),
        ),
        ..Default::default()
    })
    .await
}

async fn call_tool(p: &EscurelProcess, name: &str, args: Value) -> Value {
    let resp = reqwest::Client::new()
        .post(p.mcp_url())
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
    // tools/call results are MCP-shaped (`CallToolResult`); the payload
    // lives under `structuredContent`. Return it so callers read
    // `result["skills"]` etc. unchanged.
    body["result"]["structuredContent"].clone()
}

#[tokio::test]
async fn list_skills_returns_seeded_skills_plus_meta_skill() {
    let p = start_with_seeded_indexer().await;
    let result = call_tool(&p, "list_skills", json!({})).await;
    let skills = result["skills"].as_array().expect("skills array");
    let ids: Vec<&str> = skills.iter().filter_map(|s| s["id"].as_str()).collect();
    // Three seeded skills plus the mandatory `escurel` meta-skill that
    // every tenant ships (locked decision 3).
    assert!(ids.contains(&"customer"));
    assert!(ids.contains(&"meeting"));
    assert!(ids.contains(&"query"));
    assert!(ids.contains(&"escurel"), "meta-skill present; got {ids:?}");
    assert_eq!(ids.len(), 4);
    p.shutdown().await;
}

#[tokio::test]
async fn list_skills_reports_markdown_backend_kind() {
    let p = start_with_seeded_indexer().await;
    let result = call_tool(&p, "list_skills", json!({})).await;
    let skills = result["skills"].as_array().expect("skills array");
    assert!(!skills.is_empty());
    // Every skill is markdown-backed today: it carries an additive
    // `backend` block and a `capabilities` descriptor (REQ-BK-02).
    for s in skills {
        assert_eq!(
            s["backend"]["kind"], "markdown",
            "skill {:?} should report markdown backend",
            s["id"]
        );
        let caps = &s["capabilities"];
        assert_eq!(caps["writable"], true);
        assert_eq!(caps["supports_crdt"], true);
        assert_eq!(caps["granularity"], "block");
        assert_eq!(caps["search"], "hybrid");
    }
    p.shutdown().await;
}

#[tokio::test]
async fn list_skills_reports_workflow_backend_kind() {
    // A `kind: workflow` plan skill surfaces through the real gateway with
    // its distinct backend kind and markdown-like capabilities (it is edited
    // to steer the workflow). This is the corpus foundation the reducer
    // (PR-2+) reads its plan from.
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::Disabled,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant("acme")
                .skill("deep-research", SKILL_DEEP_RESEARCH_BODY)
                .done(),
        ),
        ..Default::default()
    })
    .await;
    let result = call_tool(&p, "list_skills", json!({})).await;
    let skills = result["skills"].as_array().expect("skills array");
    let wf = skills
        .iter()
        .find(|s| s["id"] == "deep-research")
        .expect("deep-research workflow skill present");
    assert_eq!(wf["backend"]["kind"], "workflow");
    let caps = &wf["capabilities"];
    assert_eq!(caps["writable"], true, "the plan page is edited to steer");
    assert_eq!(caps["granularity"], "block");
    assert_eq!(caps["search"], "hybrid");
    assert_eq!(caps["supports_crdt"], true);
    p.shutdown().await;
}

#[tokio::test]
async fn list_instances_returns_filtered_by_skill() {
    let p = start_with_seeded_indexer().await;
    let result = call_tool(&p, "list_instances", json!({ "skill_id": "customer" })).await;
    let inst = result["instances"].as_array().unwrap();
    assert_eq!(inst.len(), 2);
    assert!(inst.iter().all(|i| i["skill"] == "customer"));
    p.shutdown().await;
}

#[tokio::test]
async fn list_instances_frontmatter_filter_selects_subset() {
    let p = start_with_seeded_indexer().await;
    // Both customers without a filter…
    let all = call_tool(&p, "list_instances", json!({ "skill_id": "customer" })).await;
    assert_eq!(all["instances"].as_array().unwrap().len(), 2);

    // …narrowed to one by a frontmatter equality filter.
    let filtered = call_tool(
        &p,
        "list_instances",
        json!({
            "skill_id": "customer",
            "frontmatter_key": "id",
            "frontmatter_value": "acme-corp",
        }),
    )
    .await;
    let inst = filtered["instances"].as_array().unwrap();
    assert_eq!(inst.len(), 1);
    assert_eq!(inst[0]["frontmatter"]["id"], "acme-corp");
    p.shutdown().await;
}

#[tokio::test]
async fn list_instances_as_of_time_travels_through_http() {
    let p = start_with_seeded_indexer().await;
    // Both meetings without a cut…
    let all = call_tool(&p, "list_instances", json!({ "skill_id": "meeting" })).await;
    assert_eq!(all["instances"].as_array().unwrap().len(), 2);

    // …a cut between them hides the April QBR.
    let cut = call_tool(
        &p,
        "list_instances",
        json!({ "skill_id": "meeting", "as_of": "2026-02-01T00:00:00Z" }),
    )
    .await;
    let inst = cut["instances"].as_array().unwrap();
    assert_eq!(inst.len(), 1);
    assert_eq!(inst[0]["frontmatter"]["id"], "kickoff");
    p.shutdown().await;
}

#[tokio::test]
async fn list_instances_scenario_overlay_through_http() {
    let p = start_with_seeded_indexer().await;
    // Base view: the scenario-B customer is hidden.
    let base = call_tool(&p, "list_instances", json!({ "skill_id": "customer" })).await;
    assert_eq!(base["instances"].as_array().unwrap().len(), 2);

    // Scenario B: the B-only customer appears (base ∪ overlay).
    let b = call_tool(
        &p,
        "list_instances",
        json!({ "skill_id": "customer", "scenario": "B" }),
    )
    .await;
    let ids: Vec<&str> = b["instances"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|i| i["frontmatter"]["id"].as_str())
        .collect();
    assert_eq!(ids.len(), 3);
    assert!(
        ids.contains(&"future-corp"),
        "B-only instance visible under scenario B"
    );
    p.shutdown().await;
}

#[tokio::test]
async fn events_capture_inbox_assign_history_round_trips_through_http() {
    let p = start_with_seeded_indexer().await;
    let instance = "markdown/instances/customer/acme-corp.md";

    // capture → lands in the inbox.
    let captured = call_tool(
        &p,
        "capture_event",
        json!({
            "at": "2026-04-01T09:00:00Z",
            "source": "gmail",
            "mime": "message/rfc822",
            "label_skill": "email",
            "title": "Contact form",
            "body": "An enquiry.",
            "provenance": { "extracted_by": "agt:scout-a" }
        }),
    )
    .await;
    let event_id = captured["event_id"].as_str().expect("event id").to_owned();
    assert_eq!(captured["status"], "inbox");

    let inbox = call_tool(&p, "list_inbox", json!({})).await;
    assert_eq!(inbox["events"].as_array().unwrap().len(), 1);
    assert_eq!(inbox["events"][0]["source"], "gmail");

    // history empty until assigned.
    let before = call_tool(&p, "list_events", json!({ "instance_page_id": instance })).await;
    assert!(before["events"].as_array().unwrap().is_empty());

    // assign → leaves inbox, enters the instance's event history.
    call_tool(
        &p,
        "assign_event",
        json!({ "event_id": event_id, "instance_page_id": instance }),
    )
    .await;
    let after_inbox = call_tool(&p, "list_inbox", json!({})).await;
    assert!(
        after_inbox["events"].as_array().unwrap().is_empty(),
        "assigned event leaves the inbox",
    );
    let history = call_tool(&p, "list_events", json!({ "instance_page_id": instance })).await;
    let evs = history["events"].as_array().unwrap();
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0]["event_id"], event_id);
    assert_eq!(evs[0]["status"], "processed");
    assert_eq!(evs[0]["provenance"]["extracted_by"], "agt:scout-a");
    p.shutdown().await;
}

#[tokio::test]
async fn resolve_round_trips_through_http() {
    let p = start_with_seeded_indexer().await;
    let result = call_tool(
        &p,
        "resolve",
        json!({ "wikilink": "[[customer::acme-corp]]" }),
    )
    .await;
    assert_eq!(result["exists"], true);
    assert_eq!(result["page"]["skill"], "customer");
    assert_eq!(result["page"]["slug"], "acme-corp");
    p.shutdown().await;
}

#[tokio::test]
async fn expand_returns_page_body_and_wikilinks() {
    let p = start_with_seeded_indexer().await;
    let result = call_tool(&p, "expand", json!({ "page_id": INSTANCE_ACME_PATH })).await;
    assert!(result["body"].as_str().unwrap().contains("Acme Corp"));
    let wls = result["wikilinks_out"].as_array().unwrap();
    assert!(wls.iter().any(|w| w["id"] == "globex-llc"));
    p.shutdown().await;
}

#[tokio::test]
async fn neighbours_returns_outbound_links() {
    let p = start_with_seeded_indexer().await;
    let result = call_tool(
        &p,
        "neighbours",
        json!({ "page_id": INSTANCE_ACME_PATH, "direction": "out" }),
    )
    .await;
    let edges = result["edges"].as_array().unwrap();
    let dst_pages: Vec<&str> = edges
        .iter()
        .filter_map(|e| e["dst_page"].as_str())
        .collect();
    assert!(dst_pages.contains(&"globex-llc"));
    p.shutdown().await;
}

#[tokio::test]
async fn search_returns_hits_for_query() {
    let p = start_with_seeded_indexer().await;
    let result = call_tool(&p, "search", json!({ "q": "Acme", "k": 5 })).await;
    let hits = result["hits"].as_array().unwrap();
    assert!(!hits.is_empty());
    assert_eq!(result["granularity"], "block");
    p.shutdown().await;
}

#[tokio::test]
async fn search_page_granularity_and_filter_over_mcp() {
    let p = start_with_seeded_indexer().await;
    // Page granularity: response echoes "page", hits drop the anchor
    // and collapse to one per page.
    let page = call_tool(
        &p,
        "search",
        json!({ "q": "customer", "k": 10, "granularity": "page", "skill": "customer" }),
    )
    .await;
    assert_eq!(page["granularity"], "page");
    let hits = page["hits"].as_array().unwrap();
    assert!(!hits.is_empty());
    assert!(hits.iter().all(|h| h["anchor"].is_null()));

    // Frontmatter filter narrows by a frontmatter field (an object
    // `filter`, the MCP-native shape). `id` is present on every
    // instance, so this isolates acme-corp and excludes globex.
    let filtered = call_tool(
        &p,
        "search",
        json!({ "q": "customer", "k": 10, "skill": "customer", "filter": { "id": "acme-corp" } }),
    )
    .await;
    let fhits = filtered["hits"].as_array().unwrap();
    assert!(!fhits.is_empty(), "acme-corp matches the filter");
    assert!(
        fhits
            .iter()
            .all(|h| h["frontmatter_excerpt"]["id"] == "acme-corp"),
        "every hit has id acme-corp: {fhits:?}"
    );
    p.shutdown().await;
}

#[tokio::test]
async fn run_stored_query_routes_through_http() {
    let p = start_with_seeded_indexer().await;
    let result = call_tool(
        &p,
        "run_stored_query",
        json!({
            "query_id": "count-by-skill",
            "params": { "skill": "customer" }
        }),
    )
    .await;
    let rows = result["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    // Raw stored queries are scenario-agnostic: they see every page,
    // including the scenario-B future-corp customer (3 total).
    assert_eq!(rows[0]["n"], 3);
    p.shutdown().await;
}

#[tokio::test]
async fn unknown_tool_returns_jsonrpc_method_not_found() {
    let p = start_with_seeded_indexer().await;
    let resp = reqwest::Client::new()
        .post(p.mcp_url())
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
    p.shutdown().await;
}

#[tokio::test]
async fn unknown_method_returns_jsonrpc_method_not_found() {
    let p = start_with_seeded_indexer().await;
    let resp = reqwest::Client::new()
        .post(p.mcp_url())
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
    p.shutdown().await;
}

#[tokio::test]
async fn update_page_round_trips_through_http() {
    let p = start_with_seeded_indexer().await;
    let body = "---\n\
                type: instance\n\
                skill: customer\n\
                id: brand-new\n\
                ---\n\
                # Brand New Customer\n\
                \n\
                Created via update_page over MCP.\n";

    let result = call_tool(
        &p,
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
    let inst = call_tool(&p, "list_instances", json!({ "skill_id": "customer" })).await;
    let count = inst["instances"].as_array().unwrap().len();
    assert_eq!(count, 3, "expected the 2 seeded + brand-new = 3");

    p.shutdown().await;
}

#[tokio::test]
async fn update_page_propagates_parse_error_as_jsonrpc_internal() {
    let p = start_with_seeded_indexer().await;
    let resp = reqwest::Client::new()
        .post(p.mcp_url())
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
    p.shutdown().await;
}

#[tokio::test]
async fn tools_list_returns_the_agent_tools_with_schemas() {
    let p = start_with_seeded_indexer().await;
    let resp = reqwest::Client::new()
        .post(p.mcp_url())
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
        "capture_event",
        "list_inbox",
        "list_events",
        "list_snapshots",
        "assign_event",
        // Admin tenant-lifecycle + operator tools must be advertised
        // too, so a discovery client finds them.
        "tenant_create",
        "tenant_list",
        "tenant_get",
        "tenant_update",
        "tenant_delete",
        "tenant_export",
        "tenant_import",
        "rebuild",
        "compact_lanes",
        "attach_external",
        "embedding_reload",
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
    p.shutdown().await;
}

#[tokio::test]
async fn malformed_jsonrpc_version_returns_invalid_request() {
    let p = start_with_seeded_indexer().await;
    let resp = reqwest::Client::new()
        .post(p.mcp_url())
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
    p.shutdown().await;
}
