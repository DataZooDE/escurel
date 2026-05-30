//! End-to-end tests for the gRPC write tools and the remaining
//! read tools: `search`, `neighbours`, `run_stored_query`,
//! `update_page`.
//!
//! Real Indexer (DuckDB + FsStore + ZeroEmbedder), real tonic
//! server on a random port, real tonic client, real OidcVerifier
//! against the in-process JWKS the support crate stands up, real
//! QuotaManager.

use std::sync::Arc;

use escurel_proto::v1::escurel_client::EscurelClient;
use escurel_proto::v1::{
    NeighboursRequest, RunStoredQueryRequest, SearchRequest, UpdatePageRequest,
};
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

const QUERY_SKILL: &str = "---\n\
type: skill\n\
id: query\n\
description: SQL view over the indexed corpus.\n\
---\n\
# query\n";

const COUNT_QUERY: &str = "---\n\
type: instance\n\
skill: query\n\
id: count-customers\n\
db: relational\n\
sql: \"SELECT count(*) AS n FROM pages WHERE skill = 'customer' AND page_type = 'instance'\"\n\
---\n\
# count-customers\n";

async fn start(quota: Option<Arc<QuotaManager>>) -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("customer", CUSTOMER_SKILL)
                .skill("query", QUERY_SKILL)
                .instance("customer", "acme", ACME_INSTANCE)
                .instance("customer", "initech", INITECH_INSTANCE)
                .instance("query", "count-customers", COUNT_QUERY)
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
async fn search_returns_hits_for_query() {
    let p = start(None).await;
    let mut a = authed_client(&p).await;
    let resp = a
        .client
        .search(a.req(SearchRequest {
            q: "Acme".to_owned(),
            k: 5,
            granularity: String::new(),
            page_type: String::new(),
            skill: String::new(),
            filter_json: String::new(),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(!resp.hits.is_empty(), "expected at least one search hit");
    assert_eq!(resp.granularity, "block");
    assert!(resp.hits.iter().any(|h| h.skill == "customer"));
    p.shutdown().await;
}

#[tokio::test]
async fn search_page_granularity_reports_page_and_drops_anchor() {
    let p = start(None).await;
    let mut a = authed_client(&p).await;
    let resp = a
        .client
        .search(a.req(SearchRequest {
            q: "customer".to_owned(),
            k: 10,
            granularity: "page".to_owned(),
            page_type: String::new(),
            skill: "customer".to_owned(),
            filter_json: String::new(),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.granularity, "page");
    assert!(!resp.hits.is_empty());
    // Page-level hits carry no block anchor…
    assert!(resp.hits.iter().all(|h| h.anchor.is_empty()));
    // …and there is at most one hit per page.
    let mut pages: Vec<&str> = resp.hits.iter().map(|h| h.page_id.as_str()).collect();
    let n = pages.len();
    pages.sort_unstable();
    pages.dedup();
    assert_eq!(pages.len(), n, "page granularity yields one hit per page");
    p.shutdown().await;
}

#[tokio::test]
async fn search_filter_narrows_by_frontmatter() {
    let p = start(None).await;
    let mut a = authed_client(&p).await;
    let base = SearchRequest {
        q: "customer".to_owned(),
        k: 10,
        granularity: String::new(),
        page_type: String::new(),
        skill: "customer".to_owned(),
        filter_json: String::new(),
        ..Default::default()
    };
    let unfiltered = a
        .client
        .search(a.req(base.clone()))
        .await
        .unwrap()
        .into_inner();
    // acme is tier=gold; initech has no tier → the equality clause
    // drops it.
    let filtered = a
        .client
        .search(a.req(SearchRequest {
            filter_json: r#"{"tier":"gold"}"#.to_owned(),
            ..base
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(
        filtered.hits.iter().any(|h| h.page_id.contains("acme")),
        "gold-tier acme survives the filter"
    );
    assert!(
        !filtered.hits.iter().any(|h| h.page_id.contains("initech")),
        "initech (no tier) is filtered out: {:?}",
        filtered.hits.iter().map(|h| &h.page_id).collect::<Vec<_>>()
    );
    assert!(filtered.hits.len() <= unfiltered.hits.len());
    p.shutdown().await;
}

#[tokio::test]
async fn neighbours_returns_outbound_edges() {
    let p = start(None).await;
    let mut a = authed_client(&p).await;
    let resp = a
        .client
        .neighbours(a.req(NeighboursRequest {
            page_id: "markdown/instances/customer/acme.md".to_owned(),
            direction: "out".to_owned(),
            link_skill: String::new(),
            link_skill_in: Vec::new(),
            order_by: String::new(),
            limit: 0,
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(
        resp.edges.iter().any(|e| e.dst_page == "initech"),
        "expected acme → initech edge, got: {:?}",
        resp.edges
    );
    p.shutdown().await;
}

#[tokio::test]
async fn run_stored_query_executes_count() {
    let p = start(None).await;
    let mut a = authed_client(&p).await;
    let resp = a
        .client
        .run_stored_query(a.req(RunStoredQueryRequest {
            query_id: "count-customers".to_owned(),
            params_json: String::new(),
        }))
        .await
        .unwrap()
        .into_inner();
    let rows: serde_json::Value = serde_json::from_str(&resp.rows_json).unwrap();
    assert_eq!(rows[0]["n"], 2);
    assert!(resp.schema.iter().any(|c| c.name == "n"));
    p.shutdown().await;
}

#[tokio::test]
async fn update_page_round_trips_through_grpc() {
    let p = start(None).await;
    let mut a = authed_client(&p).await;
    let new_body = "---\n\
                    type: instance\n\
                    skill: customer\n\
                    id: globex\n\
                    name: Globex\n\
                    ---\n\
                    # Globex\n";
    let resp = a
        .client
        .update_page(a.req(UpdatePageRequest {
            page_id: "markdown/instances/customer/globex.md".to_owned(),
            content: new_body.to_owned(),
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(resp.ok);
    assert_eq!(resp.new_version, "v1");
    assert!(resp.issues.is_empty());
    p.shutdown().await;
}

#[tokio::test]
async fn update_page_debits_writes_dimension_independently() {
    // Writes bucket is 1/min so the first update_page passes, the
    // second is rejected. A read tool (Queries dimension) should
    // still succeed because its bucket is untouched.
    let q = QuotaConfig {
        queries_per_minute: 60,
        writes_per_minute: 1,
        embeds_per_minute: 60,
        concurrent_sessions: 32,
    };
    let p = start(Some(Arc::new(QuotaManager::new(q)))).await;
    let mut a = authed_client(&p).await;
    let body = "---\n\
                type: instance\n\
                skill: customer\n\
                id: tmp\n\
                name: tmp\n\
                ---\n\
                # tmp\n";
    a.client
        .update_page(a.req(UpdatePageRequest {
            page_id: "markdown/instances/customer/tmp.md".to_owned(),
            content: body.to_owned(),
        }))
        .await
        .unwrap();
    let err = a
        .client
        .update_page(a.req(UpdatePageRequest {
            page_id: "markdown/instances/customer/tmp2.md".to_owned(),
            content: body.to_owned(),
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::ResourceExhausted);

    // The Queries bucket is independent — a read should still pass.
    a.client
        .neighbours(a.req(NeighboursRequest {
            page_id: "markdown/instances/customer/acme.md".to_owned(),
            direction: "out".to_owned(),
            link_skill: String::new(),
            link_skill_in: Vec::new(),
            order_by: String::new(),
            limit: 0,
            ..Default::default()
        }))
        .await
        .unwrap();
    p.shutdown().await;
}

#[tokio::test]
async fn search_rejects_invalid_page_type() {
    let p = start(None).await;
    let mut a = authed_client(&p).await;
    let err = a
        .client
        .search(a.req(SearchRequest {
            q: "x".to_owned(),
            k: 1,
            granularity: String::new(),
            page_type: "bogus".to_owned(),
            skill: String::new(),
            filter_json: String::new(),
            ..Default::default()
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    p.shutdown().await;
}
