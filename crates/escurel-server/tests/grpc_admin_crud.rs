//! End-to-end tests for the M4.5 admin tenant-CRUD endpoints.
//!
//! These tests stand up a real tonic server with a real
//! `OidcVerifier` against the in-process JWKS the support crate
//! stands up, and a real tempdir-backed `FsTenantStore`. No mocks
//! at the boundary — each assertion exercises the production code
//! path verbatim.

use std::path::PathBuf;
use std::sync::Arc;

use escurel_admin::{FsTenantStore, TenantStore};
use escurel_proto::v1::escurel_admin_client::EscurelAdminClient;
use escurel_proto::v1::{
    AuditRequest, TenantCreateRequest, TenantDeleteRequest, TenantGetRequest, TenantListRequest,
    TenantSpec, TenantUpdateRequest,
};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use tempfile::TempDir;
use tonic::Request;
use tonic::metadata::MetadataValue;
use tonic::transport::Channel;

const TENANT: &str = "acme";

const CUSTOMER_SKILL: &str = "---\ntype: skill\nid: customer\ndescription: x\n---\n# customer\n";

struct Harness {
    process: EscurelProcess,
    tenants_root: PathBuf,
    _tenants_dir: TempDir,
}

async fn start() -> Harness {
    let tenants_dir = TempDir::new().unwrap();
    let tenants_root = tenants_dir.path().to_path_buf();
    let tenant_store: Arc<dyn TenantStore> = Arc::new(FsTenantStore::new(tenants_root.clone()));

    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("customer", CUSTOMER_SKILL)
                .done(),
        ),
        config_overrides: ConfigOverrides {
            gateway_version: Some("1.0.0-test".to_owned()),
            tenant_store: Some(tenant_store),
            ..Default::default()
        },
    })
    .await;
    Harness {
        process,
        tenants_root,
        _tenants_dir: tenants_dir,
    }
}

fn req<T>(bearer: &MetadataValue<tonic::metadata::Ascii>, body: T) -> Request<T> {
    let mut r = Request::new(body);
    r.metadata_mut().insert("authorization", bearer.clone());
    r
}

async fn admin_client(h: &Harness) -> EscurelAdminClient<Channel> {
    let endpoint = h.process.grpc_endpoint().expect("grpc endpoint").to_owned();
    let channel = Channel::from_shared(endpoint)
        .unwrap()
        .connect()
        .await
        .unwrap();
    EscurelAdminClient::new(channel)
}

fn admin_bearer(h: &Harness) -> MetadataValue<tonic::metadata::Ascii> {
    let t = h.process.mint_token(TENANT, Role::Admin);
    format!("Bearer {t}").parse().unwrap()
}

fn agent_bearer(h: &Harness) -> MetadataValue<tonic::metadata::Ascii> {
    let t = h.process.mint_token(TENANT, Role::Agent);
    format!("Bearer {t}").parse().unwrap()
}

fn spec(id: &str, name: &str) -> TenantSpec {
    TenantSpec {
        tenant_id: id.to_owned(),
        display_name: name.to_owned(),
    }
}

// --- tests ---------------------------------------------------------

#[tokio::test]
async fn tenant_create_seeds_directory_and_duckdb_file() {
    let h = start().await;
    let mut client = admin_client(&h).await;
    let resp = client
        .tenant_create(req(
            &admin_bearer(&h),
            TenantCreateRequest {
                spec: Some(spec("acme", "Acme Corp")),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.spec.as_ref().unwrap().tenant_id, "acme");
    assert_eq!(resp.spec.as_ref().unwrap().display_name, "Acme Corp");
    let dir = h.tenants_root.join("acme");
    assert!(dir.join("tenant.json").is_file());
    assert!(dir.join("markdown").is_dir());
    assert!(dir.join("db").join("escurel.duckdb").is_file());
    h.process.shutdown().await;
}

#[tokio::test]
async fn tenant_create_rejects_invalid_id() {
    let h = start().await;
    let mut client = admin_client(&h).await;
    let status = client
        .tenant_create(req(
            &admin_bearer(&h),
            TenantCreateRequest {
                spec: Some(spec("Bad/Id", "x")),
            },
        ))
        .await
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::InvalidArgument);
    h.process.shutdown().await;
}

#[tokio::test]
async fn tenant_create_returns_already_exists_for_duplicate() {
    let h = start().await;
    let mut client = admin_client(&h).await;
    client
        .tenant_create(req(
            &admin_bearer(&h),
            TenantCreateRequest {
                spec: Some(spec("acme", "Acme")),
            },
        ))
        .await
        .unwrap();
    let status = client
        .tenant_create(req(
            &admin_bearer(&h),
            TenantCreateRequest {
                spec: Some(spec("acme", "Acme Again")),
            },
        ))
        .await
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::AlreadyExists);
    h.process.shutdown().await;
}

#[tokio::test]
async fn tenant_list_returns_created_tenants() {
    let h = start().await;
    let mut client = admin_client(&h).await;
    for id in ["acme", "globex"] {
        client
            .tenant_create(req(
                &admin_bearer(&h),
                TenantCreateRequest {
                    spec: Some(spec(id, id)),
                },
            ))
            .await
            .unwrap();
    }
    let resp = client
        .tenant_list(req(&admin_bearer(&h), TenantListRequest::default()))
        .await
        .unwrap()
        .into_inner();
    let mut ids: Vec<String> = resp.tenants.into_iter().map(|t| t.tenant_id).collect();
    ids.sort();
    assert_eq!(ids, vec!["acme".to_owned(), "globex".to_owned()]);
    h.process.shutdown().await;
}

#[tokio::test]
async fn tenant_get_returns_spec_for_existing_tenant() {
    let h = start().await;
    let mut client = admin_client(&h).await;
    client
        .tenant_create(req(
            &admin_bearer(&h),
            TenantCreateRequest {
                spec: Some(spec("acme", "Acme Corp")),
            },
        ))
        .await
        .unwrap();
    let resp = client
        .tenant_get(req(
            &admin_bearer(&h),
            TenantGetRequest {
                tenant_id: "acme".to_owned(),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    let s = resp.spec.unwrap();
    assert_eq!(s.tenant_id, "acme");
    assert_eq!(s.display_name, "Acme Corp");
    h.process.shutdown().await;
}

#[tokio::test]
async fn tenant_get_returns_not_found_for_missing_tenant() {
    let h = start().await;
    let mut client = admin_client(&h).await;
    let status = client
        .tenant_get(req(
            &admin_bearer(&h),
            TenantGetRequest {
                tenant_id: "ghost".to_owned(),
            },
        ))
        .await
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::NotFound);
    h.process.shutdown().await;
}

#[tokio::test]
async fn tenant_update_changes_display_name() {
    let h = start().await;
    let mut client = admin_client(&h).await;
    client
        .tenant_create(req(
            &admin_bearer(&h),
            TenantCreateRequest {
                spec: Some(spec("acme", "Old")),
            },
        ))
        .await
        .unwrap();
    let resp = client
        .tenant_update(req(
            &admin_bearer(&h),
            TenantUpdateRequest {
                spec: Some(spec("acme", "New Name")),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.spec.as_ref().unwrap().display_name, "New Name");
    // Re-read via get to confirm persistence.
    let got = client
        .tenant_get(req(
            &admin_bearer(&h),
            TenantGetRequest {
                tenant_id: "acme".to_owned(),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(got.spec.unwrap().display_name, "New Name");
    h.process.shutdown().await;
}

#[tokio::test]
async fn tenant_delete_removes_directory_and_returns_true() {
    let h = start().await;
    let mut client = admin_client(&h).await;
    client
        .tenant_create(req(
            &admin_bearer(&h),
            TenantCreateRequest {
                spec: Some(spec("acme", "Acme")),
            },
        ))
        .await
        .unwrap();
    let dir = h.tenants_root.join("acme");
    assert!(dir.exists());
    let resp = client
        .tenant_delete(req(
            &admin_bearer(&h),
            TenantDeleteRequest {
                tenant_id: "acme".to_owned(),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    assert!(resp.deleted);
    assert!(!dir.exists());
    h.process.shutdown().await;
}

#[tokio::test]
async fn tenant_delete_returns_false_for_missing_tenant() {
    let h = start().await;
    let mut client = admin_client(&h).await;
    let resp = client
        .tenant_delete(req(
            &admin_bearer(&h),
            TenantDeleteRequest {
                tenant_id: "ghost".to_owned(),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    assert!(!resp.deleted);
    h.process.shutdown().await;
}

#[tokio::test]
async fn audit_returns_clean_drift_for_seeded_tenant() {
    let h = start().await;
    let mut client = admin_client(&h).await;
    let resp = client
        .audit(req(
            &admin_bearer(&h),
            AuditRequest {
                tenant_id: TENANT.to_owned(),
                scope: String::new(),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    assert!(resp.markdown_not_in_duckdb.is_empty());
    assert!(resp.indexed_but_no_markdown.is_empty());
    h.process.shutdown().await;
}

#[tokio::test]
async fn audit_rejects_mismatched_tenant() {
    let h = start().await;
    let mut client = admin_client(&h).await;
    let status = client
        .audit(req(
            &admin_bearer(&h),
            AuditRequest {
                tenant_id: "other-tenant".to_owned(),
                scope: String::new(),
            },
        ))
        .await
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::FailedPrecondition);
    h.process.shutdown().await;
}

#[tokio::test]
async fn admin_role_still_required_for_crud() {
    let h = start().await;
    let mut client = admin_client(&h).await;
    let status = client
        .tenant_create(req(
            &agent_bearer(&h),
            TenantCreateRequest {
                spec: Some(spec("acme", "x")),
            },
        ))
        .await
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::PermissionDenied);
    h.process.shutdown().await;
}
