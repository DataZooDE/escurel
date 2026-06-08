//! Real no-mock integration test for the MCP **Streamable-HTTP**
//! lifecycle on `POST /mcp`.
//!
//! Drives the exact handshake a real MCP client (Claude Code's
//! `type:"http"` server) performs: `initialize` → the
//! `notifications/initialized` notification → `tools/list` →
//! `tools/call`. Real running gateway, real Indexer (DuckDB +
//! FsStore + ZeroEmbedder), real reqwest client — the captured
//! event flows all the way down to DuckDB and back out through the
//! inbox.

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts};
use serde_json::{Value, json};

const SKILL_CUSTOMER_BODY: &str = "---\n\
     type: skill\n\
     id: customer\n\
     description: A buying entity.\n\
     ---\n\
     # customer\n";

const INSTANCE_ACME_BODY: &str = "---\n\
     type: instance\n\
     skill: customer\n\
     id: acme-corp\n\
     ---\n\
     # Acme Corp\n";

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::Disabled,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant("acme")
                .skill("customer", SKILL_CUSTOMER_BODY)
                .instance("customer", "acme-corp", INSTANCE_ACME_BODY)
                .done(),
        ),
        ..Default::default()
    })
    .await
}

/// POST one JSON-RPC frame, return `(status, body_text)`.
async fn post_raw(p: &EscurelProcess, frame: &Value) -> (reqwest::StatusCode, String) {
    let resp = reqwest::Client::new()
        .post(p.mcp_url())
        .json(frame)
        .send()
        .await
        .expect("post");
    let status = resp.status();
    let body = resp.text().await.expect("body");
    (status, body)
}

#[tokio::test]
async fn mcp_streamable_http_lifecycle_round_trips() {
    let p = start().await;

    // (a) initialize → InitializeResult, protocolVersion echoed.
    let (status, body) = post_raw(
        &p,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "claude-code", "version": "2.1.167" }
            }
        }),
    )
    .await;
    assert_eq!(status, 200, "initialize http status; body={body}");
    let init: Value = serde_json::from_str(&body).expect("initialize json");
    assert_eq!(init["jsonrpc"], "2.0");
    assert_eq!(init["id"], 1);
    assert!(
        init.get("error").is_none(),
        "initialize must not error: {init}"
    );
    let result = &init["result"];
    assert_eq!(
        result["protocolVersion"], "2025-06-18",
        "server echoes the client's protocolVersion"
    );
    assert_eq!(result["serverInfo"]["name"], "escurel");
    assert!(
        result["serverInfo"]["version"].is_string(),
        "serverInfo.version present"
    );
    assert!(
        result["capabilities"]["tools"].is_object(),
        "capabilities.tools present: {result}"
    );

    // (b) notifications/initialized → HTTP 202, empty body, NO envelope.
    let (status, body) = post_raw(
        &p,
        &json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }),
    )
    .await;
    assert_eq!(status, 202, "initialized notification is 202 Accepted");
    assert!(
        body.trim().is_empty(),
        "a notification gets no response body, got: {body:?}"
    );

    // (c) tools/list → non-empty array including list_skills.
    let (status, body) = post_raw(
        &p,
        &json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
    )
    .await;
    assert_eq!(status, 200, "tools/list http status");
    let listed: Value = serde_json::from_str(&body).expect("tools/list json");
    let tools = listed["result"]["tools"]
        .as_array()
        .expect("result.tools array");
    assert!(!tools.is_empty(), "tools list is non-empty");
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    assert!(
        names.contains(&"list_skills"),
        "tools include list_skills; got {names:?}"
    );

    // (d) tools/call capture_event → tools/call list_inbox: the real
    //     effect lands in DuckDB and reads back out of the inbox.
    let (status, body) = post_raw(
        &p,
        &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "capture_event",
                "arguments": {
                    "at": "2026-04-01T09:00:00Z",
                    "source": "gmail",
                    "mime": "message/rfc822",
                    "label_skill": "email",
                    "title": "Contact form",
                    "body": "An enquiry.",
                    "provenance": { "extracted_by": "agt:scout-a" }
                }
            }
        }),
    )
    .await;
    assert_eq!(status, 200, "capture_event http status");
    let captured: Value = serde_json::from_str(&body).expect("capture json");
    assert!(
        captured.get("error").is_none(),
        "capture_event must not error: {captured}"
    );
    // REGRESSION GUARD for the empty-tool-output bug: a `tools/call`
    // result MUST be an MCP `CallToolResult` — a `content` array a
    // text-only client (Claude Code) can read, plus `isError:false`.
    let call_result = &captured["result"];
    assert_eq!(
        call_result["isError"], false,
        "tools/call result carries isError:false: {captured}"
    );
    let content = call_result["content"]
        .as_array()
        .expect("CallToolResult.content array");
    assert!(!content.is_empty(), "content is non-empty: {captured}");
    assert_eq!(
        content[0]["type"], "text",
        "first content block is text: {captured}"
    );
    // The text block parses back to the same payload (this is what a
    // text-only client reads).
    let text = content[0]["text"].as_str().expect("content[0].text string");
    let parsed: Value = serde_json::from_str(text).expect("content text parses as JSON");
    assert_eq!(
        parsed, call_result["structuredContent"],
        "content text == structuredContent payload"
    );
    // Programmatic clients read the raw payload from `structuredContent`.
    let structured = &call_result["structuredContent"];
    let event_id = structured["event_id"]
        .as_str()
        .expect("event_id")
        .to_owned();
    assert_eq!(structured["status"], "inbox");

    let (status, body) = post_raw(
        &p,
        &json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": { "name": "list_inbox", "arguments": {} }
        }),
    )
    .await;
    assert_eq!(status, 200, "list_inbox http status");
    let inbox: Value = serde_json::from_str(&body).expect("inbox json");
    let events = inbox["result"]["structuredContent"]["events"]
        .as_array()
        .expect("result.structuredContent.events array");
    assert_eq!(events.len(), 1, "captured event visible in inbox");
    assert_eq!(events[0]["event_id"], event_id);
    assert_eq!(events[0]["source"], "gmail");

    // (e) ping → empty-object result.
    let (status, body) =
        post_raw(&p, &json!({ "jsonrpc": "2.0", "id": 5, "method": "ping" })).await;
    assert_eq!(status, 200, "ping http status");
    let pong: Value = serde_json::from_str(&body).expect("ping json");
    assert!(pong.get("error").is_none(), "ping must not error: {pong}");
    assert!(pong["result"].is_object(), "ping result is an object");

    // (f) unknown id-bearing method → JSON-RPC -32601 method-not-found.
    let (status, body) = post_raw(
        &p,
        &json!({ "jsonrpc": "2.0", "id": 6, "method": "does/not/exist" }),
    )
    .await;
    assert_eq!(status, 200, "unknown method still 200 with JSON-RPC error");
    let err: Value = serde_json::from_str(&body).expect("error json");
    assert_eq!(err["error"]["code"], -32601, "method not found code");

    p.shutdown().await;
}
