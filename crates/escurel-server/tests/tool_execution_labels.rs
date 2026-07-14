//! End-to-end test for the WI-8 execution labels (REQ-LABEL-01): every
//! tool advertised by `tools/list` carries an additive
//! `execution: "deterministic" | "orchestration"` label, making the
//! interlocked-loops "deterministic-first" invariant machine-visible —
//! a per-phase tool surface can hand a compute step deterministic
//! tools only ("the LLM never does critical arithmetic").
//!
//! Definition (documented in protocol.md): `deterministic` = the
//! result is a pure function of KB state + arguments (reads, queries,
//! validation); `orchestration` = the call advances loop state
//! (writes, events, sessions, admin/lifecycle operations).
//!
//! Real gateway, real `/mcp` `tools/list`. No mocks.

use escurel_test_support::{AuthMode, EscurelProcess, Opts};
use serde_json::{Value, json};

#[tokio::test]
async fn every_tool_carries_an_execution_label() {
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::Disabled,
        ..Default::default()
    })
    .await;
    let body: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json");
    let tools = body["result"]["tools"].as_array().expect("tools array");
    assert!(!tools.is_empty());

    for t in tools {
        let name = t["name"].as_str().unwrap_or("?");
        let exec = t["execution"].as_str().unwrap_or_default();
        assert!(
            exec == "deterministic" || exec == "orchestration",
            "tool `{name}` must carry an execution label, got `{exec}`"
        );
    }

    let exec_of = |name: &str| {
        tools
            .iter()
            .find(|t| t["name"] == name)
            .map(|t| t["execution"].as_str().unwrap_or_default().to_owned())
            .unwrap_or_default()
    };
    // Spot-checks of the split: reproducible compute vs loop state.
    for det in [
        "query_instance",
        "run_stored_query",
        "search",
        "expand",
        "validate",
    ] {
        assert_eq!(exec_of(det), "deterministic", "{det}");
    }
    for orch in [
        "update_page",
        "capture_event",
        "open_session",
        "import_pack",
    ] {
        assert_eq!(exec_of(orch), "orchestration", "{orch}");
    }

    p.shutdown().await;
}
