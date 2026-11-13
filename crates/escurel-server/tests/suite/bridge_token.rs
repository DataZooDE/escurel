//! A chat bridge's token at the gateway (#824, crew F1): the bridge signs for
//! the people it answers with a key of its OWN, under an issuer the gateway
//! names as the bridge's (`ConfigOverrides::bridge_issuer`, production
//! `ESCUREL_AUTH_BRIDGE_ISSUER`, pinned to `ESCUREL_AUTH_BRIDGE_KID`). Whatever the bridge signs under it, the gateway
//! bounds it: never admin, no groups, short-lived, no agent/run claims — and
//! what it captures says the bridge relayed it. Real gateway, real JWKS for
//! both issuers; no mocks.

use escurel_test_support::{
    AuthMode, ConfigOverrides, EscurelProcess, ExtraIssuer, FixtureBuilder, Opts, Role,
};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const BRIDGE_KID: &str = "escurel-chat-bridge";

const DEAL_SKILL: &str = "---\nkind: skill\nid: deal\n\
    description: A sales deal, readable by the sales group.\n\
    acl:\n  read: [sales]\n---\n# deal\n";
const DEAL: &str = "---\nkind: instance\nskill: deal\nid: q3\n---\n# Q3\nRenewal.\n";
const DEAL_PAGE: &str = "markdown/instances/deal/q3.md";

async fn gateway(bridge: &ExtraIssuer) -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("deal", DEAL_SKILL)
                .instance("deal", "q3", DEAL)
                .done(),
        ),
        config_overrides: ConfigOverrides {
            extra_issuers: vec![(bridge.issuer_url().to_owned(), bridge.jwks_url().to_owned())],
            bridge_issuer: Some(bridge.issuer_url().to_owned()),
            bridge_kids: vec![BRIDGE_KID.to_owned()],
            ..Default::default()
        },
    })
    .await
}

/// `(http status, body)` of one tool call.
async fn call(p: &EscurelProcess, token: &str, name: &str, args: Value) -> (u16, Value) {
    let resp = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": { "name": name, "arguments": args } }))
        .send()
        .await
        .expect("post");
    let status = resp.status().as_u16();
    (status, resp.json().await.unwrap_or(Value::Null))
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// What a compromised bridge would sign: admin, operator, a group, and the
/// channel claim.
fn greedy(bridge: &ExtraIssuer, ttl: u64) -> Value {
    let now = now();
    json!({
        "iss": bridge.issuer_url(), "aud": "escurel", "sub": "user:alice", "tenant": TENANT,
        "roles": ["escurel:admin", "escurel:operator", "sales"],
        "bridge_channel": "msteams", "iat": now, "exp": now + ttl,
    })
}

#[tokio::test]
async fn a_bridge_token_acts_as_the_person_without_admin_or_groups_and_its_captures_say_so() {
    let bridge = ExtraIssuer::start_with_own_key(BRIDGE_KID).await;
    let p = gateway(&bridge).await;
    let token = bridge.sign(&greedy(&bridge, 900));

    let (_, who) = call(&p, &token, "whoami", json!({})).await;
    let who = &who["result"]["structuredContent"];
    assert_eq!(who["subject"], "user:alice", "{who}");
    assert_eq!(
        who["is_admin"], false,
        "the admin role is not honoured: {who}"
    );

    // `sales` grants the read, but a bridge token carries no groups.
    let (_, page) = call(&p, &token, "expand", json!({ "page_id": DEAL_PAGE })).await;
    assert!(
        page["result"]["structuredContent"]["page"].is_null(),
        "a group-granted page is not visible: {page}"
    );

    // The capture names the bridge as the relay, whatever the caller wrote.
    let (_, captured) = call(
        &p,
        &token,
        "capture_event",
        json!({ "event_id": "bridge-ev-1", "source": "msteams", "label_skill": "deal",
                "title": "chat turn",
                "provenance": { "captured_via": "escurel-runner", "captured_by": "user:bob" } }),
    )
    .await;
    assert!(captured.get("error").is_none(), "{captured}");
    let admin = p.mint_token(TENANT, Role::Admin);
    let (_, listed) = call(&p, &admin, "list_events", json!({ "label_skill": "deal" })).await;
    let events = listed["result"]["structuredContent"]["events"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let ev = events
        .iter()
        .find(|e| e["event_id"] == "bridge-ev-1")
        .unwrap_or_else(|| panic!("captured: {listed}"));
    assert_eq!(ev["provenance"]["captured_by"], "user:alice");
    assert_eq!(ev["provenance"]["captured_via"], "bridge:msteams");
}

#[tokio::test]
async fn a_long_lived_or_agent_shaped_bridge_token_is_refused() {
    let bridge = ExtraIssuer::start_with_own_key(BRIDGE_KID).await;
    let p = gateway(&bridge).await;
    let mut refused = vec![greedy(&bridge, 3600)];
    for (claim, value) in [
        ("agent", json!("deploy-bot")),
        ("run_id", json!("r-1")),
        ("act", json!({ "sub": "escurel-runner" })),
    ] {
        let mut c = greedy(&bridge, 900);
        c[claim] = value;
        refused.push(c);
    }
    let mut unnamed = greedy(&bridge, 900);
    unnamed.as_object_mut().unwrap().remove("bridge_channel");
    refused.push(unnamed);
    for claims in refused {
        let (status, body) = call(&p, &bridge.sign(&claims), "whoami", json!({})).await;
        assert_eq!(status, 401, "{claims} → {body}");
    }
}
