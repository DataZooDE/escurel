//! End-to-end test for the WI-6 absorption instrumentation
//! (REQ-INSTR-01): every confirmed `update_page` counts toward
//! `escurel_writes_total{tenant, origin}` where `origin` is `runner`
//! (the write carried #246 runner/workflow provenance) or `human`
//! (out-of-band). Scraped from the real Prometheus listener — the time
//! series behind the interlocked-loops paper's Fig-2 convergence curve
//! (consultant→agent write-ratio over time), its stated falsification
//! test.
//!
//! Real gateway, real DuckDB, real `/metrics` scrape. No mocks.

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const SKILL: &str = "---\ntype: skill\nid: note\ndescription: x\n---\n# note\n";

async fn call(p: &EscurelProcess, name: &str, args: Value) -> Value {
    let token = p.mint_token(TENANT, Role::Agent);
    reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": args },
        }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json")
}

fn counter(body: &str, origin: &str) -> u64 {
    body.lines()
        .find(|l| {
            l.starts_with("escurel_writes_total")
                && l.contains(&format!("origin=\"{origin}\""))
                && l.contains(&format!("tenant=\"{TENANT}\""))
        })
        .and_then(|l| l.split_whitespace().last())
        .and_then(|v| v.parse::<f64>().ok())
        .map(|v| v as u64)
        .unwrap_or(0)
}

#[tokio::test]
async fn confirmed_writes_count_by_origin() {
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("note", SKILL)
                .done(),
        ),
        ..Default::default()
    })
    .await;

    // Two human writes (no runner provenance)…
    for i in 0..2 {
        let w = call(
            &p,
            "update_page",
            json!({
                "page_id": format!("markdown/instances/note/h{i}.md"),
                "content": format!("---\ntype: instance\nskill: note\nid: h{i}\n---\n# h{i}\n"),
            }),
        )
        .await;
        assert_eq!(w["result"]["structuredContent"]["ok"], true, "{w}");
    }
    // …one runner write (carries #246 provenance)…
    let w = call(
        &p,
        "update_page",
        json!({
            "page_id": "markdown/instances/note/r0.md",
            "content": "---\ntype: instance\nskill: note\nid: r0\n---\n# r0\n",
            "provenance": { "runner": { "run_id": "run-1" }, "workflow": null },
        }),
    )
    .await;
    assert_eq!(w["result"]["structuredContent"]["ok"], true, "{w}");

    // …and one REFUSED write, which must count toward neither (the
    // metric is CONFIRMED writes).
    let w = call(
        &p,
        "update_page",
        json!({
            "page_id": "markdown/base/some-pack/skills/x.md",
            "content": "---\ntype: skill\nid: x\ndescription: y\n---\n# x\n",
        }),
    )
    .await;
    assert_eq!(w["result"]["structuredContent"]["ok"], false, "{w}");

    let metrics = reqwest::get(p.metrics_url().expect("metrics listener"))
        .await
        .expect("scrape")
        .text()
        .await
        .expect("text");
    assert_eq!(counter(&metrics, "human"), 2, "{metrics}");
    assert_eq!(counter(&metrics, "runner"), 1, "{metrics}");

    p.shutdown().await;
}
