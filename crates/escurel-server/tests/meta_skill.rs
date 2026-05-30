//! End-to-end tests for the mandatory `escurel` meta-skill
//! (`docs/contract/agent-interface.md` locked decision 3).
//!
//! Real Indexer (DuckDB + FsStore + ZeroEmbedder), real tonic server
//! on a random port, real tonic client + OidcVerifier against the
//! in-process JWKS. A *fresh* tenant (no fixtures) must already expose
//! the meta-skill, and a write that removes it must be rejected.

use escurel_index::META_SKILL_MD;
use escurel_proto::v1::escurel_client::EscurelClient;
use escurel_proto::v1::{ExpandRequest, ListSkillsRequest, UpdatePageRequest};
use escurel_test_support::{AuthMode, EscurelProcess, Opts, Role};
use tonic::Request;
use tonic::metadata::MetadataValue;
use tonic::transport::Channel;

const TENANT: &str = "acme";
const META_PAGE_ID: &str = "markdown/skills/escurel.md";

/// A fresh tenant: no fixtures, so the only skill present is the one
/// the server auto-ships.
async fn start_fresh() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        ..Default::default()
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
async fn fresh_tenant_ships_the_meta_skill() {
    let p = start_fresh().await;
    let mut a = authed_client(&p).await;
    let resp = a
        .client
        .list_skills(a.req(ListSkillsRequest::default()))
        .await
        .unwrap()
        .into_inner();
    let meta = resp
        .skills
        .iter()
        .find(|s| s.id == "escurel")
        .expect("fresh tenant must ship the `escurel` meta-skill");
    assert!(
        meta.description.contains("navigate"),
        "meta-skill description: {}",
        meta.description
    );
    // Its body is expandable and documents the tool surface.
    let expanded = a
        .client
        .expand(a.req(ExpandRequest {
            page_id: META_PAGE_ID.to_owned(),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(expanded.body.contains("## Tool surface summary"));
    p.shutdown().await;
}

#[tokio::test]
async fn removing_a_standard_section_is_rejected() {
    let p = start_fresh().await;
    let mut a = authed_client(&p).await;
    // Drop the "## Anti-patterns" section heading.
    let mangled = META_SKILL_MD.replace("## Anti-patterns", "## Something Else");
    let resp = a
        .client
        .update_page(a.req(UpdatePageRequest {
            page_id: META_PAGE_ID.to_owned(),
            content: mangled,
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(!resp.ok, "removing a standard section must be rejected");
    assert!(
        resp.issues.iter().any(|i| i.code == "meta_skill_protected"),
        "expected a meta_skill_protected issue, got: {:?}",
        resp.issues
    );
    p.shutdown().await;
}

#[tokio::test]
async fn appending_tenant_guidance_is_accepted() {
    let p = start_fresh().await;
    let mut a = authed_client(&p).await;
    let extended = format!("{META_SKILL_MD}\n## Tenant-specific notes\n\nLocal guidance.\n");
    let resp = a
        .client
        .update_page(a.req(UpdatePageRequest {
            page_id: META_PAGE_ID.to_owned(),
            content: extended,
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(resp.ok, "appending a custom section must be accepted");
    p.shutdown().await;
}
