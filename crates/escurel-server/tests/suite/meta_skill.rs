//! The mandatory `escurel` meta-skill ships with every tenant
//! (`docs/contract/agent-interface.md` locked decision 3).
//!
//! Real Indexer (DuckDB + FsStore + ZeroEmbedder), real gateway on a
//! random port, real `escurel_client::Client` over MCP-over-HTTP
//! against the in-process JWKS. A *fresh* tenant (no fixtures) must
//! already expose the meta-skill, and a write that removes a standard
//! section must be rejected.

use escurel_client::{ExpandRequest, ListSkillsRequest, UpdatePageRequest};
use escurel_index::META_SKILL_MD;
use escurel_test_support::{AuthMode, EscurelProcess, Opts, Role};

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

#[tokio::test]
async fn fresh_tenant_ships_the_meta_skill() {
    let p = start_fresh().await;
    let c = p.client_for(TENANT, Role::Agent).await;

    let resp = c
        .list_skills(ListSkillsRequest::default())
        .await
        .expect("list_skills");
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
    let expanded = c
        .expand(ExpandRequest {
            page_id: META_PAGE_ID.to_owned(),
            ..Default::default()
        })
        .await
        .expect("expand");
    assert!(expanded.body.contains("## Tool surface summary"));
    p.shutdown().await;
}

#[tokio::test]
async fn removing_a_standard_section_is_rejected() {
    let p = start_fresh().await;
    let c = p.client_for(TENANT, Role::Agent).await;
    // Drop the "## Anti-patterns" section heading.
    let mangled = META_SKILL_MD.replace("## Anti-patterns", "## Something Else");
    let resp = c
        .update_page(UpdatePageRequest {
            page_id: META_PAGE_ID.to_owned(),
            content: mangled,
        })
        .await
        .expect("update_page");
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
    let c = p.client_for(TENANT, Role::Agent).await;
    let extended = format!("{META_SKILL_MD}\n## Tenant-specific notes\n\nLocal guidance.\n");
    let resp = c
        .update_page(UpdatePageRequest {
            page_id: META_PAGE_ID.to_owned(),
            content: extended,
        })
        .await
        .expect("update_page");
    assert!(resp.ok, "appending a custom section must be accepted");
    p.shutdown().await;
}
