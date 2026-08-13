//! The Triton-fronted trust shape, end to end: one gateway that trusts its
//! primary TestIssuer AND a second issuer (`ConfigOverrides::extra_issuers`),
//! reading group memberships from a **custom claim**
//! (`ConfigOverrides::groups_claim = "triton_sender_groups"` — the claim
//! Triton's static-upstream signer mints; escurel's production knob is
//! `ESCUREL_AUTH_GROUPS_CLAIM`). Real gateway, real Indexer, real JWKS over
//! the wire for both issuers; no mocks.
//!
//! Pins three things:
//!  1. a second-issuer token with an **audience array** (`aud: [agents,
//!     escurel]`, the multi-audience shape Triton mints) and groups under
//!     the custom claim passes verification and gains the group read;
//!  2. the same issuer without the group is fail-closed (hidden, no error);
//!  3. the PRIMARY TestIssuer's minted principals keep their groups when
//!     the custom claim is configured (claim-aware minting — without it,
//!     setting the knob would silently strip every existing test principal).

use escurel_test_support::{
    AuthMode, ConfigOverrides, EscurelProcess, ExtraIssuer, FixtureBuilder, Opts,
};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const GROUPS_CLAIM: &str = "triton_sender_groups";

const DEAL_SKILL: &str = "---\ntype: skill\nid: deal\n\
    description: A sales deal, readable by the sales group.\n\
    acl:\n  read: [sales]\n\
    ---\n# deal\n";
const DEAL: &str = "---\ntype: instance\nskill: deal\nid: q3-renewal\n---\n\
    # Q3 renewal\nBeverages GmbH renewal.\n";
const DEAL_PAGE: &str = "markdown/instances/deal/q3-renewal.md";

async fn call(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    let resp = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": args },
        }))
        .send()
        .await
        .expect("post");
    assert_eq!(resp.status(), 200, "http status");
    resp.json().await.unwrap()
}

#[tokio::test]
async fn second_issuer_token_with_custom_groups_claim_gains_group_read() {
    let extra = ExtraIssuer::start().await;
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("deal", DEAL_SKILL)
                .instance("deal", "q3-renewal", DEAL)
                .done(),
        ),
        config_overrides: ConfigOverrides {
            groups_claim: Some(GROUPS_CLAIM.to_owned()),
            extra_issuers: vec![(extra.issuer_url().to_owned(), extra.jwks_url().to_owned())],
            ..Default::default()
        },
    })
    .await;

    // 1. Triton-shaped token: second issuer, audience ARRAY naming escurel
    //    among others, groups under the custom claim → the group read works.
    let maria = extra.mint(
        TENANT,
        "maria",
        &["agents-test", "escurel"],
        GROUPS_CLAIM,
        &["sales"],
    );
    let body = call(&p, &maria, "expand", json!({ "page_id": DEAL_PAGE })).await;
    assert!(body.get("error").is_none(), "maria verifies: {body}");
    let page = &body["result"]["structuredContent"];
    assert!(
        page["body"]
            .as_str()
            .is_some_and(|b| b.contains("Q3 renewal")),
        "sales-group read via the second issuer + custom claim: {page}"
    );

    // 2. Same issuer, no group → fail-closed (page hidden, not an error).
    let outsider = extra.mint(
        TENANT,
        "outsider",
        &["agents-test", "escurel"],
        GROUPS_CLAIM,
        &[],
    );
    let body = call(&p, &outsider, "expand", json!({ "page_id": DEAL_PAGE })).await;
    assert!(body.get("error").is_none(), "outsider verifies: {body}");
    assert!(
        body["result"]["structuredContent"]["page"].is_null()
            || body["result"]["structuredContent"]["body"].is_null(),
        "no group, no read: {body}"
    );

    // 3. The PRIMARY issuer's mint helpers stay truthful under the knob:
    //    a TestIssuer principal with the sales group still reads (the
    //    claim-aware-minting regression).
    let analyst = p.mint_token_with_groups(TENANT, "analyst-1", &["sales"], false);
    let body = call(&p, &analyst, "expand", json!({ "page_id": DEAL_PAGE })).await;
    assert!(body.get("error").is_none(), "analyst verifies: {body}");
    assert!(
        body["result"]["structuredContent"]["body"]
            .as_str()
            .is_some_and(|b| b.contains("Q3 renewal")),
        "primary-issuer principal keeps its groups under the custom claim: {body}"
    );

    p.shutdown().await;
}
