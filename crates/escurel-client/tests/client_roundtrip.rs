//! End-to-end tests for the `escurel-client` typed wrapper.
//!
//! Real gateway via `escurel-test-support`, real tonic transport,
//! real `OidcVerifier` against the in-process JWKS the support
//! crate stands up, real `Indexer` with a real DuckDB file. No
//! mocks at the boundary the test exercises (CLAUDE principle 2).

use escurel_client::{
    Client, ExpandRequest, ListSkillsRequest, ResolveRequest, SearchRequest, SecretString,
    UpdatePageRequest,
};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};

const TENANT: &str = "acme";

const CUSTOMER_SKILL: &str = "---\n\
type: skill\n\
id: customer\n\
description: A buying organisation.\n\
required_frontmatter: [id, name]\n\
optional_frontmatter: [tier]\n\
---\n\
# customer\n";

const ACME_INSTANCE: &str = "---\n\
type: instance\n\
skill: customer\n\
id: acme\n\
name: Acme Corp\n\
tier: gold\n\
---\n\
# Acme Corp\n\nKey account. See [[customer::initech]].\n";

const INITECH_INSTANCE: &str = "---\n\
type: instance\n\
skill: customer\n\
id: initech\n\
name: Initech\n\
---\n\
# Initech\n";

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("customer", CUSTOMER_SKILL)
                .instance("customer", "acme", ACME_INSTANCE)
                .instance("customer", "initech", INITECH_INSTANCE)
                .done(),
        ),
        config_overrides: ConfigOverrides {
            gateway_version: Some("1.0.0-test".to_owned()),
            ..Default::default()
        },
    })
    .await
}

async fn authed_client(p: &EscurelProcess) -> Client {
    let endpoint = p.grpc_endpoint().expect("grpc endpoint").to_owned();
    let token = p.mint_token(TENANT, Role::Agent);
    Client::connect(&endpoint, SecretString::from(token))
        .await
        .unwrap()
}

#[tokio::test]
async fn connect_succeeds_against_running_gateway() {
    let p = start().await;
    // A successful connect + a trivial RPC proves the channel is
    // up and the token is being threaded through.
    let client = authed_client(&p).await;
    client
        .list_skills(ListSkillsRequest::default())
        .await
        .unwrap();
    p.shutdown().await;
}

#[tokio::test]
async fn list_skills_round_trips() {
    let p = start().await;
    let client = authed_client(&p).await;
    let resp = client
        .list_skills(ListSkillsRequest::default())
        .await
        .unwrap();
    assert_eq!(resp.skills.len(), 1);
    assert_eq!(resp.skills[0].id, "customer");
    assert_eq!(resp.skills[0].description, "A buying organisation.");
    p.shutdown().await;
}

#[tokio::test]
async fn resolve_round_trips() {
    let p = start().await;
    let client = authed_client(&p).await;
    let resp = client
        .resolve(ResolveRequest {
            wikilink: "[[customer::acme]]".to_owned(),
        })
        .await
        .unwrap();
    assert!(resp.exists);
    let page = resp.page.expect("page present");
    assert_eq!(page.skill, "customer");
    assert_eq!(page.slug, "acme");
    p.shutdown().await;
}

#[tokio::test]
async fn expand_round_trips() {
    let p = start().await;
    let client = authed_client(&p).await;
    let resolved = client
        .resolve(ResolveRequest {
            wikilink: "[[customer::acme]]".to_owned(),
        })
        .await
        .unwrap();
    let page_id = resolved.page.unwrap().page_id;
    let resp = client
        .expand(ExpandRequest {
            page_id,
            anchor: String::new(),
            version: String::new(),
        })
        .await
        .unwrap();
    assert!(!resp.body.is_empty());
    assert!(resp.wikilinks_out.iter().any(|w| w.id == "initech"));
    p.shutdown().await;
}

#[tokio::test]
async fn search_round_trips() {
    let p = start().await;
    let client = authed_client(&p).await;
    // ZeroEmbedder + FTS-backed search; query the seeded body text.
    let resp = client
        .search(SearchRequest {
            q: "Acme".to_owned(),
            k: 5,
            granularity: String::new(),
            page_type: String::new(),
            skill: String::new(),
            filter_json: String::new(),
        })
        .await
        .unwrap();
    // The response shape is what the contract commits to — the
    // surface returns whatever the indexer ranked. Asserting on
    // `granularity` is the cheapest stable invariant.
    assert_eq!(resp.granularity, "block");
    p.shutdown().await;
}

#[tokio::test]
async fn update_page_round_trips() {
    let p = start().await;
    let client = authed_client(&p).await;
    let body = "---\n\
type: instance\n\
skill: customer\n\
id: globex\n\
name: Globex\n\
---\n\
# Globex\n";
    let resp = client
        .update_page(UpdatePageRequest {
            page_id: "markdown/instances/customer/globex.md".to_owned(),
            content: body.to_owned(),
        })
        .await
        .unwrap();
    assert!(resp.ok, "update_page should succeed: {resp:?}");
    p.shutdown().await;
}

#[tokio::test]
async fn missing_token_surfaces_unauthenticated_error() {
    let p = start().await;
    // Bogus token: parses fine as a header but the verifier rejects
    // it. Surface should be `Error::Rpc` carrying
    // `Code::Unauthenticated`.
    let endpoint = p.grpc_endpoint().unwrap().to_owned();
    let client = Client::connect(&endpoint, SecretString::from("not.a.real.jwt".to_owned()))
        .await
        .unwrap();
    let err = client
        .list_skills(ListSkillsRequest::default())
        .await
        .unwrap_err();
    match err {
        escurel_client::Error::Rpc(status) => {
            assert_eq!(
                status.code(),
                tonic::Code::Unauthenticated,
                "status: {status:?}"
            );
        }
        other => panic!("expected Error::Rpc(Unauthenticated), got {other:?}"),
    }
    p.shutdown().await;
}

#[tokio::test]
async fn invalid_endpoint_url_returns_error() {
    // Not a URL at all — must surface as `InvalidEndpoint`, never
    // as a panic, never as a connect-timeout.
    let err = Client::connect("not a url", SecretString::from("x".to_owned()))
        .await
        .unwrap_err();
    assert!(
        matches!(err, escurel_client::Error::InvalidEndpoint(_)),
        "got {err:?}"
    );
}

#[tokio::test]
async fn token_is_not_leaked_in_debug_output() {
    // The secret marker must not appear in any `{:?}` formatting of
    // the client. This is the only mechanical check we have against
    // accidental log leaks.
    let secret = "THIS_TOKEN_SHOULD_NEVER_APPEAR_IN_LOGS_xyz123";
    let p = start().await;
    let endpoint = p.grpc_endpoint().unwrap().to_owned();
    let client = Client::connect(&endpoint, SecretString::from(secret.to_owned()))
        .await
        .unwrap();
    let dbg = format!("{client:?}");
    assert!(
        !dbg.contains(secret),
        "bearer token leaked into Debug output: {dbg}"
    );
    p.shutdown().await;
}
