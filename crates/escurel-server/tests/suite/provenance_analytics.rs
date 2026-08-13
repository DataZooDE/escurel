//! PR-3 (ADR-0010) — `expectation_drift` (the cross-graph "lost context"
//! query) and `abandoned_paths`, over `resolved_links`.
//!
//! No mocks: real gateway + real DuckDB. A tiny churn-style project is
//! seeded where an expectation drifts:
//!
//!   goal g1
//!   expectation e1 (refines g1, 2026-01-01)
//!   decision  d1 (motivated_by e1, 2026-02-01)   ← made under e1
//!   expectation e2 (supersedes e1, 2026-03-01)   ← e1 superseded AFTER d1
//!   decision  d2 (motivated_by e2, 2026-04-01)   ← rests on the current e2
//!   hypothesis h1 (2026-01-15); decision d3 abandons h1 (2026-02-15)
//!
//! So d1 is stale (its motivating expectation e1 was later superseded) but
//! d2 is not; e1 (superseded) and h1 (abandoned) are dead-ended.

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "proj";

fn skill(id: &str, extra: &str) -> String {
    format!("---\ntype: skill\nid: {id}\ndescription: {id}.\n{extra}---\n# {id}\n")
}

fn inst(skill: &str, id: &str, extra: &str) -> String {
    format!("---\ntype: instance\nskill: {skill}\nid: {id}\n{extra}---\n# {id}\n")
}

fn fixtures(decision_owner_private: bool) -> FixtureBuilder {
    let decision_skill = if decision_owner_private {
        skill("decision", "visibility: owner\nowner_field: owner\n")
    } else {
        skill("decision", "visibility: public\n")
    };
    let d1_owner = if decision_owner_private {
        "owner: \"owner-x\"\n"
    } else {
        ""
    };

    FixtureBuilder::new()
        .tenant(TENANT)
        .skill("goal", skill("goal", "visibility: public\n"))
        .skill("expectation", skill("expectation", "visibility: public\n"))
        .skill("hypothesis", skill("hypothesis", "visibility: public\n"))
        .skill("decision", decision_skill)
        .instance("goal", "g1", inst("goal", "g1", ""))
        .instance(
            "expectation",
            "e1",
            inst(
                "expectation",
                "e1",
                "refines: \"[[goal::g1]]\"\nat: 2026-01-01T00:00:00Z\n",
            ),
        )
        .instance(
            "expectation",
            "e2",
            inst(
                "expectation",
                "e2",
                "refines: \"[[goal::g1]]\"\nsupersedes: \"[[expectation::e1]]\"\nat: 2026-03-01T00:00:00Z\n",
            ),
        )
        .instance(
            "decision",
            "d1",
            inst(
                "decision",
                "d1",
                &format!("motivated_by: \"[[expectation::e1]]\"\nat: 2026-02-01T00:00:00Z\n{d1_owner}"),
            ),
        )
        .instance(
            "decision",
            "d2",
            inst(
                "decision",
                "d2",
                "motivated_by: \"[[expectation::e2]]\"\nat: 2026-04-01T00:00:00Z\n",
            ),
        )
        .instance(
            "hypothesis",
            "h1",
            inst("hypothesis", "h1", "at: 2026-01-15T00:00:00Z\n"),
        )
        .instance(
            "decision",
            "d3",
            inst(
                "decision",
                "d3",
                "abandons: \"[[hypothesis::h1]]\"\nat: 2026-02-15T00:00:00Z\n",
            ),
        )
        .done()
}

async fn start(decision_owner_private: bool) -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(fixtures(decision_owner_private)),
        ..Default::default()
    })
    .await
}

async fn call(p: &EscurelProcess, subject: &str, name: &str, args: Value) -> Value {
    let token = p.mint_token_with_sub(TENANT, Role::Agent, subject);
    let resp: Value = reqwest::Client::new()
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
        .expect("json");
    assert!(resp.get("error").is_none(), "{name} error: {resp}");
    resp["result"]["structuredContent"].clone()
}

fn has(arr: &Value, key: &str, needle: &str) -> bool {
    arr.as_array()
        .unwrap_or(&vec![])
        .iter()
        .any(|r| r[key].as_str().unwrap_or_default().contains(needle))
}

#[tokio::test]
async fn expectation_drift_flags_the_stale_decision_only() {
    let p = start(false).await;
    let sc = call(&p, "reader", "expectation_drift", json!({})).await;
    let rows = &sc["rows"];
    let arr = rows.as_array().expect("rows");

    // Exactly d1 is stale: motivated_by e1, superseded by e2 at 2026-03-01
    // (> d1.at 2026-02-01). d2 rests on the current e2 and must NOT appear.
    assert_eq!(arr.len(), 1, "only d1 drifts: {rows}");
    let row = &arr[0];
    assert!(
        row["decision_page_id"]
            .as_str()
            .unwrap()
            .contains("decision/d1"),
        "{row}"
    );
    assert!(
        row["expectation_page_id"]
            .as_str()
            .unwrap()
            .contains("expectation/e1"),
        "{row}"
    );
    assert!(
        row["superseding_page_id"]
            .as_str()
            .unwrap()
            .contains("expectation/e2"),
        "{row}"
    );
    assert!(
        !has(rows, "decision_page_id", "decision/d2"),
        "d2 is current: {rows}"
    );

    p.shutdown().await;
}

#[tokio::test]
async fn abandoned_paths_lists_superseded_and_abandoned_nodes() {
    let p = start(false).await;
    let sc = call(&p, "reader", "abandoned_paths", json!({})).await;
    let nodes = &sc["nodes"];

    // e1 was superseded (by e2); h1 was abandoned (by d3).
    assert!(
        has(nodes, "page_id", "expectation/e1"),
        "e1 superseded: {nodes}"
    );
    assert!(
        has(nodes, "page_id", "hypothesis/h1"),
        "h1 abandoned: {nodes}"
    );
    // The current head e2 and the live decision d1 are not retired.
    assert!(
        !has(nodes, "page_id", "expectation/e2"),
        "e2 is current: {nodes}"
    );

    // The skill filter narrows to one entity type.
    let only_exp = call(
        &p,
        "reader",
        "abandoned_paths",
        json!({ "skill": "expectation" }),
    )
    .await;
    assert!(has(&only_exp["nodes"], "page_id", "expectation/e1"));
    assert!(
        !has(&only_exp["nodes"], "page_id", "hypothesis/h1"),
        "skill filter excludes hypotheses: {only_exp}"
    );

    p.shutdown().await;
}

#[tokio::test]
async fn expectation_drift_is_fail_closed_when_the_decision_is_private() {
    // decision is owner-private, d1 owned by "owner-x". A non-owner must not
    // see the drift row (it references d1); the owner does.
    let p = start(true).await;

    let hidden = call(&p, "not-the-owner", "expectation_drift", json!({})).await;
    assert_eq!(
        hidden["rows"].as_array().map(Vec::len),
        Some(0),
        "private decision hidden from non-owner: {hidden}"
    );

    let seen = call(&p, "owner-x", "expectation_drift", json!({})).await;
    assert!(
        has(&seen["rows"], "decision_page_id", "decision/d1"),
        "owner sees the drift row: {seen}"
    );

    p.shutdown().await;
}
