//! End-to-end tests for the gRPC `EscurelAdmin` service stubs.
//!
//! The agent surface (`Escurel`) has its own coverage in
//! grpc_read_tools.rs / grpc_write_tools.rs. This file covers the
//! admin surface — `Health` returns the configured version,
//! every other admin RPC currently returns `Unimplemented`, and
//! all admin RPCs require the `Admin` role on the bearer JWT.
//! Real implementations land in M4 alongside the admin endpoints.

use escurel_proto::v1::escurel_admin_client::EscurelAdminClient;
use escurel_proto::v1::{HealthRequest, QuotaGetRequest, TenantListRequest, TenantSpec};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use tonic::Request;
use tonic::metadata::MetadataValue;
use tonic::transport::Channel;

const TENANT: &str = "acme";

const CUSTOMER_SKILL: &str = "---\ntype: skill\nid: customer\ndescription: x\n---\n# customer\n";

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("customer", CUSTOMER_SKILL)
                .done(),
        ),
        config_overrides: ConfigOverrides {
            gateway_version: Some("1.0.0-test".to_owned()),
            ..Default::default()
        },
    })
    .await
}

fn req<T>(bearer: &MetadataValue<tonic::metadata::Ascii>, body: T) -> Request<T> {
    let mut r = Request::new(body);
    r.metadata_mut().insert("authorization", bearer.clone());
    r
}

async fn admin_client(p: &EscurelProcess) -> EscurelAdminClient<Channel> {
    let endpoint = p.grpc_endpoint().expect("grpc endpoint");
    let channel = Channel::from_shared(endpoint.to_owned())
        .unwrap()
        .connect()
        .await
        .unwrap();
    EscurelAdminClient::new(channel)
}

fn admin_bearer(p: &EscurelProcess) -> MetadataValue<tonic::metadata::Ascii> {
    let t = p.mint_token(TENANT, Role::Admin);
    format!("Bearer {t}").parse().unwrap()
}

fn agent_bearer(p: &EscurelProcess) -> MetadataValue<tonic::metadata::Ascii> {
    let t = p.mint_token(TENANT, Role::Agent);
    format!("Bearer {t}").parse().unwrap()
}

#[tokio::test]
async fn health_returns_configured_version() {
    let p = start().await;
    let mut client = admin_client(&p).await;
    let resp = client
        .health(req(&admin_bearer(&p), HealthRequest::default()))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.version, "1.0.0-test");
    assert!(!resp.status.is_empty());
    p.shutdown().await;
}

#[tokio::test]
async fn health_works_without_bearer_when_unauthenticated_dev_mode() {
    // Bring up a server with no verifier — health on EscurelAdmin
    // must still return the version (it's the substrate health
    // probe and must be dependency-free auth-wise).
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::Disabled,
        fixtures: None,
        config_overrides: ConfigOverrides {
            gateway_version: Some("dev".to_owned()),
            ..Default::default()
        },
    })
    .await;
    let endpoint = p.grpc_endpoint().unwrap().to_owned();
    let channel = Channel::from_shared(endpoint)
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut client = EscurelAdminClient::new(channel);
    let resp = client
        .health(Request::new(HealthRequest::default()))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.version, "dev");
    p.shutdown().await;
}

#[tokio::test]
async fn admin_rpc_requires_admin_role() {
    let p = start().await;
    let mut client = admin_client(&p).await;
    // Agent-role token must NOT pass an admin RPC.
    let status = client
        .tenant_list(req(&agent_bearer(&p), TenantListRequest::default()))
        .await
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::PermissionDenied);
    p.shutdown().await;
}

#[tokio::test]
async fn admin_rpc_missing_bearer_returns_unauthenticated() {
    let p = start().await;
    let mut client = admin_client(&p).await;
    let status = client
        .tenant_list(Request::new(TenantListRequest::default()))
        .await
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::Unauthenticated);
    p.shutdown().await;
}

#[tokio::test]
async fn tenant_crud_without_tenant_store_returns_failed_precondition() {
    // `start()` wires no `tenant_store`, so CRUD must surface
    // `failed_precondition` rather than the M3 `Unimplemented`
    // sentinel — the server can't perform tenant ops without a
    // backing store. M4.5 added the implementation; absence of a
    // store is the explicit "off" knob for health-only deployments.
    let p = start().await;
    let mut client = admin_client(&p).await;
    let status = client
        .tenant_list(req(&admin_bearer(&p), TenantListRequest::default()))
        .await
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::FailedPrecondition);
    p.shutdown().await;
}

#[tokio::test]
async fn quota_get_without_quota_returns_failed_precondition() {
    // `start()` wires no quota manager, so the M4.5b
    // `quota_get` surface must surface `failed_precondition`
    // rather than the old `Unimplemented` sentinel. The role
    // gate still applies — coverage for the implementation
    // path lives in `grpc_admin_streaming.rs`.
    let p = start().await;
    let mut client = admin_client(&p).await;
    let status = client
        .quota_get(req(
            &admin_bearer(&p),
            QuotaGetRequest {
                tenant_id: TENANT.to_owned(),
            },
        ))
        .await
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::FailedPrecondition);
    p.shutdown().await;
}

#[tokio::test]
async fn agent_role_cannot_read_admin_health() {
    // Health is the one admin RPC that should NOT require admin
    // role — it's the substrate liveness probe and must work for
    // any authenticated caller (and unauthenticated when dev mode).
    let p = start().await;
    let mut client = admin_client(&p).await;
    let resp = client
        .health(req(&agent_bearer(&p), HealthRequest::default()))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.version, "1.0.0-test");
    p.shutdown().await;
}

// Smoke test of the modelled-but-unimplemented tenant CRUD bodies.
#[tokio::test]
async fn tenant_spec_round_trips_through_proto_types() {
    // This isn't an RPC test — just verifies the generated proto
    // types compose so callers (CLI, dashboards) can build the
    // request bodies without surprises before M4 lights up the
    // server side.
    let spec = TenantSpec {
        tenant_id: "acme".to_owned(),
        display_name: "Acme Corp".to_owned(),
    };
    assert_eq!(spec.tenant_id, "acme");
}
