//! Okta as an OAuth 2.0 service app (#835): instead of an admin's `SSWS` API
//! token, the gateway authenticates with a client-credentials grant whose
//! client assertion it signs with the app's private key (`private_key_jwt`),
//! and reads the groups with the `Bearer` token it gets back.
//!
//! Real gateway, real egress, real token cache; only Okta is stood in for,
//! and the stand-in verifies the assertion (signature, `iss`/`sub`/`aud`,
//! lifetime, an unused `jti`), the grant and the scopes before it issues a
//! token, and wants that token on the groups call.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use escurel_server::egress::{Egress, EgressPolicy};
use escurel_server::group_directory::{DirectoryConfig, DirectoryError, GroupDirectory, okta};
use escurel_test_support::{
    AuthMode, ConfigOverrides, DirectoryStandIn, EscurelProcess, FixtureBuilder, Opts, bridge_mint,
};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const ALICE_EMAIL: &str = "alice@acme.example";

/// A gateway that mints bridge turns with Okta as its only directory, and
/// asks Okta again on every turn (no membership cache) so the token cache is
/// what is observed.
async fn gateway(okta: &DirectoryStandIn) -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(FixtureBuilder::new().tenant(TENANT).done()),
        config_overrides: ConfigOverrides {
            signing: true,
            bridge_mint: Some(bridge_mint(
                "user:{aadObjectId}",
                vec![okta.config("escurel-")],
                Some((Duration::ZERO, Duration::ZERO)),
            )),
            ..Default::default()
        },
    })
    .await
}

/// The groups (`roles`) of the token the gateway mints for alice's 1:1 turn.
async fn minted_roles(gw: &EscurelProcess) -> Value {
    let bridge = gw.mint_token_with_groups(TENANT, "svc:chat-bridge", &["escurel:bridge"], false);
    let body: Value = reqwest::Client::new()
        .post(gw.mcp_url())
        .bearer_auth(bridge)
        .json(
            &json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
            "name": "mint_user_token",
            "arguments": { "channel": "msteams", "tenant_id": "entra-tenant",
                "aad_object_id": "alice-oid", "user_id": "29:alice-oid", "email": ALICE_EMAIL,
                "conversation": { "kind": "personal", "id": "a:1personal" } } } }),
        )
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json");
    assert!(body.get("error").is_none(), "{body}");
    let token = body["result"]["structuredContent"]["token"]
        .as_str()
        .expect("a token")
        .to_owned();
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(token.split('.').nth(1).unwrap())
        .unwrap();
    serde_json::from_slice::<Value>(&payload).unwrap()["roles"].clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_okta_service_app_reads_the_groups_with_one_cached_token() {
    let okta = DirectoryStandIn::okta_oauth().await;
    okta.member_of(ALICE_EMAIL, &["escurel-sales", "escurel-support"]);
    let gw = gateway(&okta).await;
    assert_eq!(minted_roles(&gw).await, json!(["sales", "support"]));
    assert_eq!(minted_roles(&gw).await, json!(["sales", "support"]));
    assert_eq!(okta.lookups(), 4, "every turn asked Okta (two pages each)");
    assert_eq!(okta.tokens_issued(), 1, "two lookups, one token request");

    // A second gateway signs its own assertion: a new `jti`, which the stand-in accepts once.
    let other = gateway(&okta).await;
    assert_eq!(minted_roles(&other).await, json!(["sales", "support"]));
    assert_eq!(okta.tokens_issued(), 2);
}

/// Okta can require DPoP of a service app; escurel sends no DPoP proof, so
/// the lookup fails naming the switch to turn off, and the turn carries no
/// Okta groups (a refusal narrows, never widens).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_okta_app_that_requires_dpop_is_refused_naming_the_fix() {
    let okta = DirectoryStandIn::okta_oauth().await;
    okta.member_of(ALICE_EMAIL, &["escurel-sales"]);
    okta.require_dpop();

    let DirectoryConfig::Okta(cfg) = okta.config("escurel-") else {
        unreachable!()
    };
    let egress = Arc::new(Egress::new(EgressPolicy::dev()));
    let dir = okta::OktaDirectory::new(cfg, egress).expect("the app key parses");
    match dir.groups_for(ALICE_EMAIL).await {
        Err(DirectoryError::Permanent(why)) => {
            assert!(why.contains("DPoP") && why.contains("turn off"), "{why}");
        }
        other => panic!("expected a permanent refusal naming DPoP, got {other:?}"),
    }

    let gw = gateway(&okta).await;
    assert_eq!(minted_roles(&gw).await, json!([]));
    assert_eq!(okta.tokens_issued(), 0);
}
