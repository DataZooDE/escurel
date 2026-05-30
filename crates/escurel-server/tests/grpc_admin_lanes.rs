//! End-to-end tests for the admin lane-introspection RPCs
//! (`AdminListLanes` / `AdminLaneKeys` / `AdminLaneBlob`) on the gRPC
//! `EscurelAdmin` service — `docs/contract/agent-interface.md`.
//!
//! Real Indexer (DuckDB + FsStore + ZeroEmbedder), real tonic server,
//! real OidcVerifier against the in-process JWKS, real admin/agent
//! role gating.

use escurel_proto::v1::escurel_admin_client::EscurelAdminClient;
use escurel_proto::v1::{AdminLaneBlobRequest, AdminLaneKeysRequest, AdminListLanesRequest};
use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use tonic::Request;
use tonic::metadata::MetadataValue;
use tonic::transport::Channel;

const TENANT: &str = "acme";
const SMALL_PAGE: &str = "markdown/instances/note/hello.md";
const BIG_PAGE: &str = "markdown/instances/note/big.md";

fn small_body() -> String {
    "---\ntype: instance\nskill: note\nid: hello\n---\n# Hello\n".to_owned()
}

/// A page whose body pushes the blob over the 1 MiB admin cap.
fn big_body() -> String {
    let mut s = "---\ntype: instance\nskill: note\nid: big\n---\n# Big\n".to_owned();
    s.push_str(&"A".repeat(1_100_000));
    s
}

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .page(SMALL_PAGE, small_body())
                .page(BIG_PAGE, big_body())
                .done(),
        ),
        ..Default::default()
    })
    .await
}

async fn admin_client(p: &EscurelProcess) -> EscurelAdminClient<Channel> {
    let endpoint = p.grpc_endpoint().expect("grpc endpoint").to_owned();
    let channel = Channel::from_shared(endpoint)
        .unwrap()
        .connect()
        .await
        .unwrap();
    EscurelAdminClient::new(channel)
}

fn bearer(p: &EscurelProcess, role: Role) -> MetadataValue<tonic::metadata::Ascii> {
    let t = p.mint_token(TENANT, role);
    format!("Bearer {t}").parse().unwrap()
}

fn req<T>(b: &MetadataValue<tonic::metadata::Ascii>, body: T) -> Request<T> {
    let mut r = Request::new(body);
    r.metadata_mut().insert("authorization", b.clone());
    r
}

#[tokio::test]
async fn list_lanes_reports_the_markdown_fs_lane() {
    let p = start().await;
    let mut c = admin_client(&p).await;
    let resp = c
        .admin_list_lanes(req(&bearer(&p, Role::Admin), AdminListLanesRequest {}))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.lanes.len(), 1);
    let lane = &resp.lanes[0];
    assert_eq!(lane.name, "markdown");
    assert_eq!(lane.backend, "fs");
    assert!(lane.tenants_present.contains(&TENANT.to_owned()));
    p.shutdown().await;
}

#[tokio::test]
async fn lane_keys_lists_keys_with_sizes() {
    let p = start().await;
    let mut c = admin_client(&p).await;
    let resp = c
        .admin_lane_keys(req(
            &bearer(&p, Role::Admin),
            AdminLaneKeysRequest {
                lane: String::new(),
                prefix: "markdown/skills".to_owned(),
                limit: 0,
            },
        ))
        .await
        .unwrap()
        .into_inner();
    // The mandatory meta-skill lives under markdown/skills and has a
    // non-zero size.
    let meta = resp
        .keys
        .iter()
        .find(|k| k.key == "markdown/skills/escurel.md")
        .expect("meta-skill key present under markdown/skills");
    assert!(meta.size_bytes > 0);
    p.shutdown().await;
}

#[tokio::test]
async fn lane_blob_fetches_markdown_with_content_type() {
    let p = start().await;
    let mut c = admin_client(&p).await;
    let resp = c
        .admin_lane_blob(req(
            &bearer(&p, Role::Admin),
            AdminLaneBlobRequest {
                lane: String::new(),
                key: SMALL_PAGE.to_owned(),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.content_type, "text/markdown");
    assert!(String::from_utf8_lossy(&resp.bytes).contains("# Hello"));
    p.shutdown().await;
}

#[tokio::test]
async fn lane_blob_over_cap_is_rejected() {
    let p = start().await;
    let mut c = admin_client(&p).await;
    let status = c
        .admin_lane_blob(req(
            &bearer(&p, Role::Admin),
            AdminLaneBlobRequest {
                lane: String::new(),
                key: BIG_PAGE.to_owned(),
            },
        ))
        .await
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::FailedPrecondition);
    p.shutdown().await;
}

#[tokio::test]
async fn lane_introspection_requires_admin_role() {
    let p = start().await;
    let mut c = admin_client(&p).await;
    let status = c
        .admin_list_lanes(req(&bearer(&p, Role::Agent), AdminListLanesRequest {}))
        .await
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::PermissionDenied);
    p.shutdown().await;
}
