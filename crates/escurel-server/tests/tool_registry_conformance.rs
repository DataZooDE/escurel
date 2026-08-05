//! The three tool registries must agree.
//!
//! A tool name is currently declared in three places that nothing links:
//!
//!   1. the dispatch arms          (`mcp.rs`, `match params.name.as_str()`)
//!   2. the discovery payload      (`mcp/schema.rs`, `tools_list_payload`)
//!   3. the execution-label list   (`mcp/schema.rs`, `DETERMINISTIC_TOOLS`)
//!
//! Nothing forces them to agree, so a tool can be dispatchable but
//! undiscoverable, advertised but unroutable, or labelled `deterministic`
//! while it orchestrates — and the crate still compiles. That is the same
//! drift failure mode this repository already documents for the consumer
//! skill, reproduced inside one file.
//!
//! Unifying the three into one declarative registry is R2 of
//! `docs/notes/complexity-reduction-plan.md`. This test is what makes that
//! change safe to attempt, and it guards the invariant in the meantime.
//!
//! Real gateway, real `/mcp`. No mocks.

use std::collections::BTreeSet;

use escurel_test_support::{AuthMode, EscurelProcess, Opts};
use serde_json::{Value, json};

/// JSON-RPC "method not found" — what the dispatcher returns for a name it
/// has no arm for.
const METHOD_NOT_FOUND: i64 = -32601;

async fn advertised(p: &EscurelProcess) -> Vec<Value> {
    let body: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
        .send()
        .await
        .expect("post tools/list")
        .json()
        .await
        .expect("json");
    body["result"]["tools"]
        .as_array()
        .expect("tools/list returns an array")
        .clone()
}

async fn call_tool(p: &EscurelProcess, name: &str) -> Value {
    reqwest::Client::new()
        .post(p.mcp_url())
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": {} },
        }))
        .send()
        .await
        .expect("post tools/call")
        .json()
        .await
        .expect("json")
}

/// Every advertised tool must be routable.
///
/// Calling with empty arguments is expected to fail for most tools — missing
/// parameters, missing role, missing page. What must *not* happen is
/// `method not found`, which means the name is advertised by the discovery
/// payload and has no dispatch arm behind it.
#[tokio::test]
async fn every_advertised_tool_is_dispatchable() {
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::Disabled,
        ..Default::default()
    })
    .await;

    let tools = advertised(&p).await;
    assert!(!tools.is_empty(), "tools/list returned nothing");

    let mut unroutable = Vec::new();
    for t in &tools {
        let name = t["name"].as_str().expect("tool has a name").to_owned();
        let resp = call_tool(&p, &name).await;
        if resp["error"]["code"].as_i64() == Some(METHOD_NOT_FOUND) {
            unroutable.push(name);
        }
    }

    assert!(
        unroutable.is_empty(),
        "advertised by tools/list but no dispatch arm: {unroutable:?}"
    );
    p.shutdown().await;
}

/// Every advertised tool must carry an execution label, and the label must be
/// one of the two documented values.
///
/// `tool_execution_labels.rs` already asserts presence; this adds the
/// vocabulary check, so a typo produces a failure rather than a silently
/// unrecognised label.
#[tokio::test]
async fn every_execution_label_is_a_known_value() {
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::Disabled,
        ..Default::default()
    })
    .await;

    let mut bad = Vec::new();
    for t in advertised(&p).await {
        let name = t["name"].as_str().unwrap_or_default().to_owned();
        match t["execution"].as_str() {
            Some("deterministic") | Some("orchestration") => {}
            other => bad.push(format!("{name}: {other:?}")),
        }
    }

    assert!(bad.is_empty(), "unrecognised execution labels: {bad:?}");
    p.shutdown().await;
}

/// Tool names must be unique in the discovery payload.
///
/// A duplicated entry would let two schemas describe one dispatch arm, and
/// whichever a client read last would win.
#[tokio::test]
async fn advertised_tool_names_are_unique() {
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::Disabled,
        ..Default::default()
    })
    .await;

    let tools = advertised(&p).await;
    let names: Vec<String> = tools
        .iter()
        .map(|t| t["name"].as_str().unwrap_or_default().to_owned())
        .collect();
    let unique: BTreeSet<&String> = names.iter().collect();

    assert_eq!(
        unique.len(),
        names.len(),
        "duplicate tool names in tools/list: {:?}",
        {
            let mut seen = BTreeSet::new();
            names
                .iter()
                .filter(|n| !seen.insert((*n).clone()))
                .collect::<Vec<_>>()
        }
    );
    p.shutdown().await;
}

/// Every advertised tool must describe its arguments.
///
/// An advertised tool with no input schema cannot be called correctly by a
/// client that has not read the source, which defeats the point of
/// discovery.
#[tokio::test]
async fn every_advertised_tool_has_an_input_schema() {
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::Disabled,
        ..Default::default()
    })
    .await;

    let mut missing = Vec::new();
    for t in advertised(&p).await {
        let name = t["name"].as_str().unwrap_or_default().to_owned();
        let schema = &t["inputSchema"];
        if !schema.is_object() || schema.get("type").is_none() {
            missing.push(name);
        }
    }

    assert!(missing.is_empty(), "no usable inputSchema: {missing:?}");
    p.shutdown().await;
}
