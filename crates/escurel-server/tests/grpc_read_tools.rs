//! End-to-end tests for the gRPC `Escurel` read-tools surface.
//! Real Indexer (DuckDB + FsStore + ZeroEmbedder), real tonic
//! server on a random port, real tonic client, real OidcVerifier
//! against the in-process JWKS the support crate stands up, real
//! QuotaManager.

use std::sync::Arc;

use escurel_proto::v1::escurel_client::EscurelClient;
use escurel_proto::v1::{ExpandRequest, ListInstancesRequest, ListSkillsRequest, ResolveRequest};
use escurel_quota::{QuotaConfig, QuotaManager};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use tonic::Request;
use tonic::metadata::MetadataValue;
use tonic::transport::Channel;

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

// Scenario-B-only customer: hidden in base, visible under scenario B.
const FUTURE_B_INSTANCE: &str = "---\n\
type: instance\n\
skill: customer\n\
id: future\n\
name: Future Corp\n\
scenario: B\n\
---\n\
# Future Corp\n";

async fn start(quota: Option<Arc<QuotaManager>>) -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("customer", CUSTOMER_SKILL)
                .instance("customer", "acme", ACME_INSTANCE)
                .instance("customer", "initech", INITECH_INSTANCE)
                .instance("customer", "future", FUTURE_B_INSTANCE)
                .done(),
        ),
        config_overrides: ConfigOverrides {
            quota,
            ..Default::default()
        },
    })
    .await
}

struct Authed {
    client: EscurelClient<Channel>,
    bearer: MetadataValue<tonic::metadata::Ascii>,
}

impl Authed {
    fn req<T>(&self, body: T) -> Request<T> {
        let mut r = Request::new(body);
        r.metadata_mut()
            .insert("authorization", self.bearer.clone());
        r
    }
}

async fn authed_client(p: &EscurelProcess) -> Authed {
    let endpoint = p.grpc_endpoint().expect("grpc endpoint");
    let channel = Channel::from_shared(endpoint.to_owned())
        .unwrap()
        .connect()
        .await
        .unwrap();
    let t = p.mint_token(TENANT, Role::Agent);
    let bearer: MetadataValue<_> = format!("Bearer {t}").parse().unwrap();
    Authed {
        client: EscurelClient::new(channel),
        bearer,
    }
}

#[tokio::test]
async fn list_skills_returns_seeded_skill() {
    let p = start(None).await;
    let mut a = authed_client(&p).await;
    let resp = a
        .client
        .list_skills(a.req(ListSkillsRequest::default()))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.skills.len(), 1);
    let s = &resp.skills[0];
    assert_eq!(s.id, "customer");
    assert_eq!(s.description, "A buying organisation.");
    assert!(s.required_frontmatter.contains(&"id".to_owned()));
    assert!(s.optional_frontmatter.contains(&"tier".to_owned()));
    assert!(!s.is_event_typed);
    p.shutdown().await;
}

#[tokio::test]
async fn list_instances_returns_seeded_instances() {
    let p = start(None).await;
    let mut a = authed_client(&p).await;
    let resp = a
        .client
        .list_instances(a.req(ListInstancesRequest {
            skill: "customer".to_owned(),
            order_by_at: String::new(),
            limit: 0,
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.instances.len(), 2);
    let ids: Vec<_> = resp.instances.iter().map(|i| i.skill.clone()).collect();
    assert!(ids.iter().all(|s| s == "customer"));
    assert!(
        resp.instances
            .iter()
            .all(|i| !i.frontmatter_json.is_empty())
    );
    p.shutdown().await;
}

#[tokio::test]
async fn list_instances_scenario_overlay_through_grpc() {
    let p = start(None).await;
    let mut a = authed_client(&p).await;

    // Base view (scenario empty) hides the B-only customer.
    let base = a
        .client
        .list_instances(a.req(ListInstancesRequest {
            skill: "customer".to_owned(),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(base.instances.len(), 2, "base view is base-only over gRPC");

    // scenario = "B" adds the overlay-only instance — proving the proto
    // field round-trips through the gRPC handler to the indexer.
    let b = a
        .client
        .list_instances(a.req(ListInstancesRequest {
            skill: "customer".to_owned(),
            scenario: "B".to_owned(),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        b.instances.len(),
        3,
        "scenario B = base ∪ overlay over gRPC"
    );
    p.shutdown().await;
}

#[tokio::test]
async fn resolve_returns_existing_page() {
    let p = start(None).await;
    let mut a = authed_client(&p).await;
    let resp = a
        .client
        .resolve(a.req(ResolveRequest {
            wikilink: "[[customer::acme]]".to_owned(),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(resp.exists);
    let parsed = resp.parsed.unwrap();
    assert_eq!(parsed.skill, "customer");
    assert_eq!(parsed.id, "acme");
    let page = resp.page.unwrap();
    assert_eq!(page.skill, "customer");
    assert_eq!(page.slug, "acme");
    assert_eq!(page.page_type, "instance");
    p.shutdown().await;
}

#[tokio::test]
async fn expand_returns_body_and_outbound_wikilinks() {
    let p = start(None).await;
    let mut a = authed_client(&p).await;
    let resolved = a
        .client
        .resolve(a.req(ResolveRequest {
            wikilink: "[[customer::acme]]".to_owned(),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();
    let page_id = resolved.page.unwrap().page_id;
    let resp = a
        .client
        .expand(a.req(ExpandRequest {
            page_id,
            anchor: String::new(),
            version: String::new(),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();
    let page = resp.page.unwrap();
    assert_eq!(page.skill, "customer");
    assert!(!resp.body.is_empty());
    assert!(!resp.blocks.is_empty());
    assert!(resp.wikilinks_out.iter().any(|w| w.id == "initech"));
    p.shutdown().await;
}

#[tokio::test]
async fn missing_bearer_returns_unauthenticated() {
    let p = start(None).await;
    let endpoint = p.grpc_endpoint().unwrap().to_owned();
    let channel = Channel::from_shared(endpoint)
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut client = EscurelClient::new(channel);
    let status = client
        .list_skills(ListSkillsRequest::default())
        .await
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::Unauthenticated);
    p.shutdown().await;
}

#[tokio::test]
async fn invalid_token_returns_unauthenticated() {
    let p = start(None).await;
    let endpoint = p.grpc_endpoint().unwrap().to_owned();
    let channel = Channel::from_shared(endpoint)
        .unwrap()
        .connect()
        .await
        .unwrap();
    let metadata: MetadataValue<_> = "Bearer not.a.real.jwt".parse().unwrap();
    let mut client = EscurelClient::new(channel);
    let mut req = Request::new(ListSkillsRequest::default());
    req.metadata_mut().insert("authorization", metadata);
    let status = client.list_skills(req).await.unwrap_err();
    assert_eq!(status.code(), tonic::Code::Unauthenticated);
    p.shutdown().await;
}

#[tokio::test]
async fn quota_exhaustion_returns_resource_exhausted() {
    let q = QuotaConfig {
        queries_per_minute: 1,
        writes_per_minute: 60,
        embeds_per_minute: 60,
        concurrent_sessions: 32,
    };
    let p = start(Some(Arc::new(QuotaManager::new(q)))).await;
    let mut a = authed_client(&p).await;
    // First call passes.
    a.client
        .list_skills(a.req(ListSkillsRequest::default()))
        .await
        .unwrap();
    // Second call hits the empty bucket.
    let status = a
        .client
        .list_skills(a.req(ListSkillsRequest::default()))
        .await
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::ResourceExhausted);
    let retry = status
        .metadata()
        .get("retry-after-ms")
        .expect("retry-after-ms metadata present");
    assert!(retry.to_str().unwrap().parse::<u64>().unwrap() > 0);
    p.shutdown().await;
}
