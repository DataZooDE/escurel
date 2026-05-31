//! Phase A — `/metrics` returns the real `escurel-obs` registry.
//!
//! Spins up the real gateway through `EscurelProcess`, fires
//! `tools/list` requests through `POST /mcp`, then scrapes
//! `/metrics` and asserts that the request counter family
//! (`escurel_requests_total`) shows at least the count we drove.
//! No mocks: real Prometheus registry rendered through the real
//! `axum` route.

use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts};
use serde_json::json;

const CUSTOMER_SKILL: &str = "---\ntype: skill\nid: customer\n\
description: A buyer.\nrequired_frontmatter: [id]\n---\n# customer\n";

#[tokio::test]
async fn metrics_counts_real_mcp_requests() {
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::Disabled,
        fixtures: None,
        config_overrides: ConfigOverrides {
            disable_indexer: true,
            ..Default::default()
        },
    })
    .await;

    // Drive three real MCP `tools/list` calls — the dispatcher
    // increments the request counter once per call.
    let http = reqwest::Client::new();
    for id in 1..=3 {
        let body = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/list",
            "params": {}
        });
        let resp = http
            .post(p.mcp_url())
            .json(&body)
            .send()
            .await
            .expect("mcp request");
        assert_eq!(resp.status(), 200, "tools/list must succeed");
    }

    // Metrics live on their own dedicated listener now — NOT on the
    // main HTTP app. The main port must 404 for `/metrics`.
    let on_main = http
        .get(format!("{}/metrics", p.base_url()))
        .send()
        .await
        .expect("main /metrics request");
    assert_eq!(
        on_main.status(),
        404,
        "/metrics must not be served on the main HTTP port"
    );

    // Scrape the dedicated metrics listener and parse the
    // request-counter sample for the /mcp route.
    let metrics_url = p.metrics_url().expect("metrics listener bound");
    let body = http
        .get(metrics_url)
        .send()
        .await
        .expect("metrics request")
        .text()
        .await
        .expect("metrics body");

    // Must be the real registry — HELP/TYPE lines for the families
    // escurel-obs registers.
    assert!(
        body.contains("# HELP escurel_requests_total"),
        "missing HELP escurel_requests_total in body:\n{body}"
    );
    assert!(
        body.contains("# TYPE escurel_requests_total counter"),
        "missing TYPE escurel_requests_total counter in body:\n{body}"
    );

    // The /mcp route must show a request count of at least 3.
    let mut count_for_mcp = 0u64;
    for line in body.lines() {
        if line.starts_with("escurel_requests_total{")
            && line.contains(r#"route="/mcp""#)
            && line.contains(r#"status="200""#)
        {
            let value = line
                .rsplit_once(' ')
                .map(|(_, v)| v)
                .expect("sample has a value");
            count_for_mcp = value.parse::<u64>().expect("counter is u64");
        }
    }
    assert!(
        count_for_mcp >= 3,
        "expected /mcp request count >= 3, got {count_for_mcp}; body:\n{body}"
    );

    // The gateway's liveness gauge must be set to 1.
    assert!(
        body.contains("escurel_up 1"),
        "expected escurel_up 1 in metrics body:\n{body}"
    );

    p.shutdown().await;
}

#[tokio::test]
async fn per_tool_metrics_record_calls_and_latency() {
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::Disabled,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant("acme")
                .skill("customer", CUSTOMER_SKILL)
                .done(),
        ),
        config_overrides: ConfigOverrides {
            ..Default::default()
        },
    })
    .await;

    // Drive a real tool call (list_skills) over MCP.
    let http = reqwest::Client::new();
    let resp = http
        .post(p.mcp_url())
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "list_skills", "arguments": {} }
        }))
        .send()
        .await
        .expect("tools/call");
    assert_eq!(resp.status(), 200);

    let body = http
        .get(p.metrics_url().expect("metrics listener"))
        .send()
        .await
        .expect("metrics")
        .text()
        .await
        .expect("metrics body");

    // escurel_tool_calls is labelled by tenant/tool/transport/status.
    assert!(
        body.lines().any(|l| l.starts_with("escurel_tool_calls{")
            && l.contains(r#"tool="list_skills""#)
            && l.contains(r#"transport="mcp_http""#)
            && l.contains(r#"status="ok""#)),
        "missing escurel_tool_calls sample for list_skills:\n{body}"
    );
    // The latency histogram family is present and observed.
    assert!(
        body.contains("# TYPE escurel_tool_latency_ms histogram"),
        "missing escurel_tool_latency_ms histogram:\n{body}"
    );
    assert!(
        body.lines()
            .any(|l| l.starts_with("escurel_tool_latency_ms_count{")
                && l.contains(r#"tool="list_skills""#)),
        "missing escurel_tool_latency_ms count for list_skills:\n{body}"
    );
    // The live-sessions gauge is exported (zero here).
    assert!(
        body.contains("escurel_live_sessions_open 0"),
        "missing escurel_live_sessions_open gauge:\n{body}"
    );

    p.shutdown().await;
}
