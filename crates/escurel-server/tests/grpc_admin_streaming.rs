//! End-to-end tests for the M4.5b streaming + small admin RPCs.
//!
//! Exercised against a real tonic server, a real `OidcVerifier`
//! against the in-process JWKS the support crate stands up, a real
//! `Indexer` over a real DuckDB file, and a real `FsTenantStore`
//! backing the export / import bytes.
//!
//! Covered surface:
//!
//! * `rebuild` — server-streaming, one `RebuildProgress` chunk
//!   per page, terminator chunk with `done == total`.
//! * `tenant_export` / `tenant_import` — round-trip via real
//!   tar+gz frames over the wire.
//! * `quota_get` — bucket-floor remaining tokens reflect recent
//!   debits.
//! * `embedding_reload` — `failed_precondition` when no reloadable
//!   embedder is wired (the recovery path lives in
//!   `grpc_admin_external_reload.rs`).
//! * Role gate — agent-role token cannot drive Rebuild.

use std::path::PathBuf;
use std::sync::Arc;

use escurel_admin::{FsTenantStore, TenantSpec as AdminTenantSpec, TenantStore};
use escurel_proto::v1::escurel_admin_client::EscurelAdminClient;
use escurel_proto::v1::escurel_client::EscurelClient;
use escurel_proto::v1::{
    EmbeddingReloadRequest, ListSkillsRequest, QuotaGetRequest, RebuildRequest,
    TenantCreateRequest, TenantExportRequest, TenantImportChunk, TenantSpec,
};
use escurel_quota::{QuotaConfig, QuotaManager};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use tempfile::TempDir;
use tokio_stream::StreamExt;
use tonic::Request;
use tonic::metadata::MetadataValue;
use tonic::transport::Channel;

const TENANT: &str = "acme";

const PAGES: &[(&str, &str)] = &[
    (
        "markdown/skills/customer.md",
        "---\ntype: skill\nid: customer\ndescription: x\n---\n# customer\n",
    ),
    (
        "markdown/skills/order.md",
        "---\ntype: skill\nid: order\ndescription: y\n---\n# order\n",
    ),
    (
        "markdown/instances/customer/acme.md",
        "---\ntype: instance\nskill: customer\nid: acme\n---\n# Acme\n",
    ),
];

struct Harness {
    process: EscurelProcess,
    tenants_root: PathBuf,
    _tenants_dir: TempDir,
}

/// Mirror every seeded markdown body into
/// `<tenants_root>/<TENANT>/markdown/...`. The `tenant_export`
/// RPC walks that tree to build the tarball, so the support
/// crate's fixture seed (which only fills the LaneStore +
/// indexer) needs a companion here.
async fn mirror_to_tenants_root(tenants_root: &std::path::Path) {
    let md_root = tenants_root.join(TENANT).join("markdown");
    for (path, body) in PAGES {
        let abs = md_root.join(path.strip_prefix("markdown/").unwrap());
        if let Some(parent) = abs.parent() {
            tokio::fs::create_dir_all(parent).await.unwrap();
        }
        tokio::fs::write(&abs, body).await.unwrap();
    }
}

async fn start() -> Harness {
    start_with_quota(QuotaConfig::defaults()).await
}

async fn start_with_quota(quota_cfg: QuotaConfig) -> Harness {
    let tenants_dir = TempDir::new().unwrap();
    let tenants_root = tenants_dir.path().to_path_buf();
    let tenant_store: Arc<dyn TenantStore> = Arc::new(FsTenantStore::new(tenants_root.clone()));
    // Provision the tenant in the FsTenantStore so its `markdown/`
    // subtree is present for the export RPC to walk.
    tenant_store
        .create(&AdminTenantSpec {
            tenant_id: TENANT.to_owned(),
            display_name: "Acme".to_owned(),
        })
        .await
        .unwrap();
    mirror_to_tenants_root(&tenants_root).await;

    let mut fixtures = FixtureBuilder::new().tenant(TENANT);
    for (path, body) in PAGES {
        fixtures = fixtures.page(path, *body);
    }
    let quota = Arc::new(QuotaManager::new(quota_cfg));

    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(fixtures.done()),
        config_overrides: ConfigOverrides {
            gateway_version: Some("1.0.0-test".to_owned()),
            quota: Some(quota),
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

async fn agent_client(h: &Harness) -> EscurelClient<Channel> {
    let endpoint = h.process.grpc_endpoint().expect("grpc endpoint").to_owned();
    let channel = Channel::from_shared(endpoint)
        .unwrap()
        .connect()
        .await
        .unwrap();
    EscurelClient::new(channel)
}

fn admin_bearer(h: &Harness) -> MetadataValue<tonic::metadata::Ascii> {
    let t = h.process.mint_token(TENANT, Role::Admin);
    format!("Bearer {t}").parse().unwrap()
}

fn agent_bearer(h: &Harness) -> MetadataValue<tonic::metadata::Ascii> {
    let t = h.process.mint_token(TENANT, Role::Agent);
    format!("Bearer {t}").parse().unwrap()
}

// --- rebuild --------------------------------------------------------

#[tokio::test]
async fn rebuild_streams_one_progress_chunk_per_page() {
    let h = start().await;
    let mut client = admin_client(&h).await;
    let mut stream = client
        .rebuild(req(
            &admin_bearer(&h),
            RebuildRequest {
                tenant_id: TENANT.to_owned(),
                scope: String::new(),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    let mut chunks = Vec::new();
    while let Some(msg) = stream.next().await {
        chunks.push(msg.unwrap());
    }
    assert_eq!(
        chunks.len(),
        PAGES.len(),
        "expected one progress chunk per page; got {chunks:?}"
    );
    h.process.shutdown().await;
}

#[tokio::test]
async fn rebuild_emits_final_chunk_with_done_equal_total() {
    let h = start().await;
    let mut client = admin_client(&h).await;
    let mut stream = client
        .rebuild(req(
            &admin_bearer(&h),
            RebuildRequest {
                tenant_id: TENANT.to_owned(),
                scope: String::new(),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    let mut last = None;
    while let Some(msg) = stream.next().await {
        last = Some(msg.unwrap());
    }
    let last = last.expect("at least one progress chunk");
    assert_eq!(last.total, PAGES.len() as u64);
    assert_eq!(last.done, last.total);
    assert!(
        !last.current_page.is_empty(),
        "current_page must be set on every chunk"
    );
    h.process.shutdown().await;
}

// --- export / import -----------------------------------------------

async fn drain_export(
    client: &mut EscurelAdminClient<Channel>,
    h: &Harness,
    tenant: &str,
) -> Vec<u8> {
    let mut stream = client
        .tenant_export(req(
            &admin_bearer(h),
            TenantExportRequest {
                tenant_id: tenant.to_owned(),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&chunk.unwrap().data);
    }
    out
}

#[tokio::test]
async fn tenant_export_streams_tarball_chunks() {
    let h = start().await;
    let mut client = admin_client(&h).await;
    let bytes = drain_export(&mut client, &h, TENANT).await;
    assert!(!bytes.is_empty(), "export must produce at least one chunk");
    // gzip magic bytes — `flate2`'s default encoder writes the
    // standard gzip header.
    assert_eq!(
        &bytes[..2],
        b"\x1f\x8b",
        "exported stream must be gzip-framed"
    );
    h.process.shutdown().await;
}

#[tokio::test]
async fn tenant_export_round_trips_through_tenant_import() {
    let h = start().await;
    let mut client = admin_client(&h).await;
    // Pull the tarball bytes via the streaming export RPC.
    let bytes = drain_export(&mut client, &h, TENANT).await;

    // Provision a fresh tenant in the same backing store, then
    // pipe the bytes back through TenantImport. The round-trip
    // proves we use real tar+gz on both sides.
    let target = "globex";
    client
        .tenant_create(req(
            &admin_bearer(&h),
            TenantCreateRequest {
                spec: Some(TenantSpec {
                    tenant_id: target.to_owned(),
                    display_name: "Globex".to_owned(),
                }),
            },
        ))
        .await
        .unwrap();

    let chunks: Vec<TenantImportChunk> = bytes
        .chunks(32 * 1024)
        .map(|c| TenantImportChunk {
            tenant_id: target.to_owned(),
            data: c.to_vec(),
        })
        .collect();
    let resp = client
        .tenant_import(req(&admin_bearer(&h), tokio_stream::iter(chunks)))
        .await
        .unwrap()
        .into_inner();
    assert!(resp.bytes_imported > 0);

    // Every seeded markdown file must be present under the new
    // tenant's `markdown/` tree on disk.
    for (path, _) in PAGES {
        let rel = path.strip_prefix("markdown/").unwrap();
        let abs = h.tenants_root.join(target).join("markdown").join(rel);
        assert!(abs.is_file(), "import did not restore `{}`", abs.display());
    }
    h.process.shutdown().await;
}

/// Codex P2 (PR M4.5b): `tenant_export` skipped tenant_id
/// validation before invoking `TenantStore::tenant_dir`, which is
/// `root.join(tenant_id)`. A crafted `../other` would have walked
/// up into a sibling tenant's markdown tree. Fix validates ids up
/// front; this test pins the gate.
#[tokio::test]
async fn tenant_export_rejects_path_traversal_tenant_id() {
    let h = start().await;
    let mut client = admin_client(&h).await;
    let err = client
        .tenant_export(req(
            &admin_bearer(&h),
            TenantExportRequest {
                tenant_id: format!("../{TENANT}"),
            },
        ))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    h.process.shutdown().await;
}

#[tokio::test]
async fn tenant_import_rejects_unknown_tenant() {
    let h = start().await;
    let mut client = admin_client(&h).await;
    let chunks = vec![TenantImportChunk {
        tenant_id: "ghost".to_owned(),
        data: vec![0_u8; 10],
    }];
    let status = client
        .tenant_import(req(&admin_bearer(&h), tokio_stream::iter(chunks)))
        .await
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::NotFound);
    h.process.shutdown().await;
}

// --- quota_get ------------------------------------------------------

#[tokio::test]
async fn quota_get_returns_remaining_tokens() {
    let h = start().await;
    let mut client = admin_client(&h).await;
    let resp = client
        .quota_get(req(
            &admin_bearer(&h),
            QuotaGetRequest {
                tenant_id: TENANT.to_owned(),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    let defaults = QuotaConfig::defaults();
    assert_eq!(resp.queries_remaining, defaults.queries_per_minute);
    assert_eq!(resp.writes_remaining, defaults.writes_per_minute);
    assert_eq!(resp.embeds_remaining, defaults.embeds_per_minute);
    // Field name in the proto is `concurrent_sessions`, but the
    // semantics we ship are "currently occupied" so the
    // baseline reads as zero.
    assert_eq!(resp.concurrent_sessions, 0);
    h.process.shutdown().await;
}

#[tokio::test]
async fn quota_get_reflects_recent_debits() {
    // Use a deliberately *low* per-minute rate so the token bucket
    // barely refills during the test window. With the production
    // default (600/min = 10 tokens/sec) the 3 burned tokens refill
    // within ~0.3 s, so the snapshot races the refill and the
    // assertion flakes under load (it did, on CI's 2-core runner).
    // At 6/min = 0.1 tokens/sec, no whole token refills for 10 s —
    // far longer than this test takes even when contended — so the
    // floored `queries_remaining` deterministically reflects the
    // debits.
    let quota_cfg = QuotaConfig {
        queries_per_minute: 6,
        ..QuotaConfig::defaults()
    };
    let h = start_with_quota(quota_cfg).await;
    let mut agent = agent_client(&h).await;
    // Burn 3 `queries`-bucket tokens via the real agent surface.
    // The agent client carries a tenant-claim matching the quota
    // manager's tenant key.
    let agent_bearer_md = agent_bearer(&h);
    for _ in 0..3_u32 {
        let mut r = Request::new(ListSkillsRequest::default());
        r.metadata_mut()
            .insert("authorization", agent_bearer_md.clone());
        agent.list_skills(r).await.unwrap();
    }

    let mut admin = admin_client(&h).await;
    let resp = admin
        .quota_get(req(
            &admin_bearer(&h),
            QuotaGetRequest {
                tenant_id: TENANT.to_owned(),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    // 6 capacity − 3 consumed = 3, ± negligible refill (<1 whole
    // token over the test window).
    assert!(
        resp.queries_remaining <= 3,
        "expected at least 3 of 6 tokens consumed; got {}",
        resp.queries_remaining
    );
    h.process.shutdown().await;
}

// --- embedding_reload ----------------------------------------------

/// With no reloadable embedder wired (this harness does not install
/// one), `embedding_reload` reports `failed_precondition`. The
/// degraded-start recovery path is covered in
/// `grpc_admin_external_reload.rs`.
#[tokio::test]
async fn embedding_reload_without_reloadable_is_failed_precondition() {
    let h = start().await;
    let mut client = admin_client(&h).await;
    let err = client
        .embedding_reload(req(&admin_bearer(&h), EmbeddingReloadRequest::default()))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::FailedPrecondition);
    h.process.shutdown().await;
}

// --- role gate ------------------------------------------------------

#[tokio::test]
async fn streaming_admin_rpcs_still_require_admin_role() {
    let h = start().await;
    let mut client = admin_client(&h).await;
    let status = client
        .rebuild(req(
            &agent_bearer(&h),
            RebuildRequest {
                tenant_id: TENANT.to_owned(),
                scope: String::new(),
            },
        ))
        .await
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::PermissionDenied);
    h.process.shutdown().await;
}
