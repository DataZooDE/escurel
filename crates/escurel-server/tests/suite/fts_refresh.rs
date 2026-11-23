//! A page written while serving reaches lexical search (#868). Real gateway,
//! real HTTP: the BM25 index is rebuilt on the server's timer when stale.

use std::time::{Duration, Instant};

use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const TOKEN: &str = "zyxquorbit";
/// Reciprocal-rank fusion: a hit one arm returns scores at most 1/61.
const ONE_ARM: f64 = 1.0 / 61.0 + 1e-9;

async fn rpc(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    let v: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": { "name": name, "arguments": args } }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json");
    v["result"]["structuredContent"].clone()
}

async fn fresh_score(p: &EscurelProcess, token: &str) -> f64 {
    let s = rpc(p, token, "search", json!({ "q": TOKEN, "k": 5 })).await;
    s["hits"]
        .as_array()
        .and_then(|h| {
            h.iter()
                .find(|h| h["page_id"] == "markdown/instances/note/fresh.md")
        })
        .and_then(|h| h["score"].as_f64())
        .unwrap_or(0.0)
}

#[tokio::test]
async fn a_page_written_while_serving_reaches_lexical_search() {
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            fts_refresh: Some(Duration::from_secs(1)),
            ..Default::default()
        },
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill(
                    "note",
                    "---\nkind: skill\nid: note\ndescription: A note.\n---\n# note\n",
                )
                .done(),
        ),
    })
    .await;
    let admin = p.mint_token(TENANT, Role::Admin);
    let w = rpc(
        &p,
        &admin,
        "update_page",
        json!({ "page_id": "markdown/instances/note/fresh.md",
                "content": format!("---\nkind: instance\nskill: note\nid: fresh\n---\n# fresh\nThe {TOKEN} shipment.\n") }),
    )
    .await;
    assert_eq!(w["ok"], true, "{w}");
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut score = fresh_score(&p, &admin).await;
    while score <= ONE_ARM && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(250)).await;
        score = fresh_score(&p, &admin).await;
    }
    assert!(
        score > ONE_ARM,
        "the page never reached the BM25 arm (score {score}): nothing rebuilt the index after the write"
    );
}
