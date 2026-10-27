//! `whoami` (governed chat agent): the caller's verified identity, so a client
//! with no OIDC session of its own (escurel-explore behind a BFF) can address
//! its own page chat (`page-chat:v1:<sha256(subject)>:<page>`). Real gateway,
//! real tokens from the test issuer.

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "acme";

async fn whoami(p: &EscurelProcess, token: &str) -> Value {
    let body: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": { "name": "whoami", "arguments": {} } }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json");
    assert!(body.get("error").is_none(), "{body}");
    body["result"]["structuredContent"].clone()
}

#[tokio::test]
async fn whoami_names_the_verified_subject_tenant_and_admin_flag() {
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(FixtureBuilder::new().tenant(TENANT).done()),
        ..Default::default()
    })
    .await;
    let alice = whoami(
        &p,
        &p.mint_token_with_sub(TENANT, Role::Agent, "user:alice"),
    )
    .await;
    assert_eq!(
        alice,
        json!({ "subject": "user:alice", "tenant": TENANT, "is_admin": false })
    );
    let admin = whoami(&p, &p.mint_token(TENANT, Role::Admin)).await;
    assert_eq!(admin["is_admin"], true);
    assert_eq!(admin["tenant"], TENANT);
}
