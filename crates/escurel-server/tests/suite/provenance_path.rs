//! PR-4 (ADR-0010) — the path mode of `provenance_ancestry` (the old
//! `provenance_path` tool, consolidated): shortest path / reachability
//! between two pages over `resolved_links`.
//!
//! No mocks: real gateway + real DuckDB. A three-node chain
//!   a1 --derived_from--> b1 --derived_from--> c1
//! is seeded; we ask whether a1 reaches c1 (yes, 2 hops), whether the
//! reverse holds (no), and confirm the fail-closed rule: a route through
//! an ACL-private interior node reports `reachable: false` with no path.

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "lab";
const A1: &str = "markdown/instances/pub_node/a1.md";
const B1: &str = "markdown/instances/mid/b1.md";
const C1: &str = "markdown/instances/pub_node/c1.md";

fn skill(id: &str, extra: &str) -> String {
    format!("---\ntype: skill\nid: {id}\ndescription: {id}.\n{extra}---\n# {id}\n")
}

fn fixtures(mid_private: bool) -> FixtureBuilder {
    let mid_skill = if mid_private {
        skill("mid", "visibility: owner\nowner_field: owner\n")
    } else {
        skill("mid", "visibility: public\n")
    };
    let b1 = if mid_private {
        "---\ntype: instance\nskill: mid\nid: b1\nowner: \"owner-x\"\n\
         derived_from: \"[[pub_node::c1]]\"\n---\n# b1\n"
    } else {
        "---\ntype: instance\nskill: mid\nid: b1\n\
         derived_from: \"[[pub_node::c1]]\"\n---\n# b1\n"
    };

    FixtureBuilder::new()
        .tenant(TENANT)
        .skill("pub_node", skill("pub_node", "visibility: public\n"))
        .skill("mid", mid_skill)
        .instance(
            "pub_node",
            "a1",
            "---\ntype: instance\nskill: pub_node\nid: a1\n\
             derived_from: \"[[mid::b1]]\"\n---\n# a1\n",
        )
        .instance("mid", "b1", b1)
        .instance(
            "pub_node",
            "c1",
            "---\ntype: instance\nskill: pub_node\nid: c1\n---\n# c1\n",
        )
        .done()
}

async fn start(mid_private: bool) -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(fixtures(mid_private)),
        ..Default::default()
    })
    .await
}

async fn path(p: &EscurelProcess, subject: &str, args: Value) -> Value {
    let token = p.mint_token_with_sub(TENANT, Role::Agent, subject);
    let resp: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "provenance_ancestry", "arguments": args },
        }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json");
    assert!(
        resp.get("error").is_none(),
        "provenance_ancestry error: {resp}"
    );
    resp["result"]["structuredContent"].clone()
}

#[tokio::test]
async fn provenance_path_finds_the_chain_and_respects_direction() {
    let p = start(false).await;

    // a1 reaches c1 in two derived_from hops.
    let r = path(
        &p,
        "reader",
        json!({ "from_page": A1, "to_page": C1, "direction": "up",
                "relations": ["derived_from"], "max_hops": 5 }),
    )
    .await;
    assert_eq!(r["reachable"], true, "a1→c1 reachable: {r}");
    assert_eq!(r["depth"], 2, "two hops: {r}");
    let path_ids: Vec<&str> = r["path"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(path_ids, vec![A1, B1, C1], "ordered path: {r}");

    // The reverse does not hold walking `up`.
    let r = path(
        &p,
        "reader",
        json!({ "from_page": C1, "to_page": A1, "direction": "up",
                "relations": ["derived_from"], "max_hops": 5 }),
    )
    .await;
    assert_eq!(r["reachable"], false, "c1 does not rest on a1: {r}");

    p.shutdown().await;
}

#[tokio::test]
async fn provenance_path_is_fail_closed_through_a_private_interior() {
    // b1 (the only route from a1 to c1) is owner-private. A non-owner must
    // get reachable:false with no path — revealing the connection would leak
    // b1's existence.
    let p = start(true).await;

    let hidden = path(
        &p,
        "not-the-owner",
        json!({ "from_page": A1, "to_page": C1, "direction": "up",
                "relations": ["derived_from"], "max_hops": 5 }),
    )
    .await;
    assert_eq!(
        hidden["reachable"], false,
        "route hidden via private b1: {hidden}"
    );
    assert_eq!(
        hidden["path"].as_array().map(Vec::len),
        Some(0),
        "no path leaked: {hidden}"
    );

    // The owner sees the full route.
    let seen = path(
        &p,
        "owner-x",
        json!({ "from_page": A1, "to_page": C1, "direction": "up",
                "relations": ["derived_from"], "max_hops": 5 }),
    )
    .await;
    assert_eq!(seen["reachable"], true, "owner sees the route: {seen}");
    assert_eq!(seen["depth"], 2);

    p.shutdown().await;
}
