//! Phase A — `/metrics` returns the real `escurel-obs` registry.
//!
//! Spins up the real gateway through `EscurelProcess`, fires
//! `tools/list` requests through `POST /mcp`, then scrapes
//! `/metrics` and asserts that the request counter family
//! (`escurel_requests_total`) shows at least the count we drove.
//! No mocks: real Prometheus registry rendered through the real
//! `axum` route.

use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, Opts};
use serde_json::json;

#[tokio::test]
async fn metrics_counts_real_mcp_requests() {
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::Disabled,
        fixtures: None,
        config_overrides: ConfigOverrides {
            disable_grpc: true,
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

    // Scrape /metrics and parse the request-counter sample for the
    // /mcp route.
    let body = http
        .get(format!("{}/metrics", p.base_url()))
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
