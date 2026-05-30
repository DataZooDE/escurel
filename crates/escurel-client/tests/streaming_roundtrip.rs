//! End-to-end tests for the `escurel-client` **admin streaming**
//! surface: `rebuild` (server-stream) and `tenant_export` /
//! `tenant_import` (server- and client-stream).
//!
//! Real gateway via `escurel-test-support`, real tonic transport,
//! real `OidcVerifier`, real tempdir-backed `FsTenantStore`, real
//! `Indexer` over a real DuckDB file. No mocks at the boundary the
//! test exercises (CLAUDE principle 2).
//!
//! `Client::live_session` (the agent bidi stream) is intentionally not
//! e2e-tested here: a pure-gRPC client cannot *open* a CRDT session —
//! `open_session` is an HTTP-MCP-only tool — so the bidi stream can
//! only ever *attach* to a session opened over HTTP. The bidi wire
//! behaviour is covered at the server layer in
//! `escurel-server/tests/grpc_live_session.rs`; the client wrapper is a
//! thin passthrough over the same generated stub the admin streams use
//! (exercised here), so it shares their transport coverage.

use std::path::PathBuf;
use std::sync::Arc;

use escurel_admin::{FsTenantStore, TenantSpec as AdminTenantSpec, TenantStore};
use escurel_client::{
    AdminClient, RebuildRequest, SecretString, TenantCreateRequest, TenantExportRequest,
    TenantImportChunk, TenantSpec,
};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use futures::StreamExt;
use tempfile::TempDir;

const TENANT: &str = "acme";

const PAGES: &[(&str, &str)] = &[
    (
        "markdown/skills/customer.md",
        "---\ntype: skill\nid: customer\ndescription: x\n---\n# customer\n",
    ),
    (
        "markdown/instances/customer/acme.md",
        "---\ntype: instance\nskill: customer\nid: acme\n---\n# Acme\n",
    ),
];

struct AdminHarness {
    process: EscurelProcess,
    tenants_root: PathBuf,
    _tenants_dir: TempDir,
}

/// Mirror the seeded markdown into `<root>/<TENANT>/markdown/...` so
/// the `tenant_export` RPC (which walks the on-disk tree) has bytes.
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

async fn start_admin() -> AdminHarness {
    let tenants_dir = TempDir::new().unwrap();
    let tenants_root = tenants_dir.path().to_path_buf();
    let tenant_store: Arc<dyn TenantStore> = Arc::new(FsTenantStore::new(tenants_root.clone()));
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
    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(fixtures.done()),
        config_overrides: ConfigOverrides {
            gateway_version: Some("1.0.0-test".to_owned()),
            tenant_store: Some(tenant_store),
            ..Default::default()
        },
    })
    .await;
    AdminHarness {
        process,
        tenants_root,
        _tenants_dir: tenants_dir,
    }
}

async fn admin_client(p: &EscurelProcess) -> AdminClient {
    let endpoint = p.grpc_endpoint().expect("grpc endpoint").to_owned();
    let token = p.mint_token(TENANT, Role::Admin);
    AdminClient::connect(&endpoint, SecretString::from(token))
        .await
        .unwrap()
}

/// `rebuild` streams progress chunks and the terminator has
/// `done == total` with a non-zero total.
#[tokio::test]
async fn rebuild_streams_progress_to_completion() {
    let h = start_admin().await;
    let client = admin_client(&h.process).await;
    let mut stream = client
        .rebuild(RebuildRequest {
            tenant_id: TENANT.to_owned(),
            scope: String::new(),
        })
        .await
        .unwrap();
    let mut last = None;
    while let Some(msg) = stream.next().await {
        last = Some(msg.expect("progress chunk ok"));
    }
    let last = last.expect("at least one progress chunk");
    assert!(last.total > 0, "rebuild should report a page total");
    assert_eq!(last.done, last.total, "terminator: done == total");
    h.process.shutdown().await;
}

/// Realistic operator backup/restore: export the tenant to a tar+gz
/// byte stream through the typed client, then stream those bytes back
/// into a freshly-created tenant via `tenant_import`.
#[tokio::test]
async fn tenant_export_then_import_round_trips() {
    let h = start_admin().await;
    let client = admin_client(&h.process).await;

    // Drain the export stream into one buffer.
    let mut export = client
        .tenant_export(TenantExportRequest {
            tenant_id: TENANT.to_owned(),
        })
        .await
        .unwrap();
    let mut bytes = Vec::new();
    while let Some(chunk) = export.next().await {
        bytes.extend_from_slice(&chunk.expect("export chunk ok").data);
    }
    assert!(!bytes.is_empty(), "export should produce tarball bytes");

    // Create a destination tenant on disk so import has somewhere to
    // land (mirrors the server's existence check).
    client
        .tenant_create(TenantCreateRequest {
            spec: Some(TenantSpec {
                tenant_id: "globex".to_owned(),
                display_name: "Globex".to_owned(),
            }),
        })
        .await
        .unwrap();

    // Stream the bytes back in. The first chunk carries the target id.
    let chunks = tokio_stream::iter(vec![TenantImportChunk {
        tenant_id: "globex".to_owned(),
        data: bytes.clone(),
    }]);
    let resp = client.tenant_import(chunks).await.unwrap();
    assert_eq!(
        resp.bytes_imported as usize,
        bytes.len(),
        "import should account for every exported byte"
    );
    // The imported markdown is now under the destination tenant.
    assert!(
        h.tenants_root
            .join("globex")
            .join("markdown")
            .join("skills")
            .join("customer.md")
            .is_file(),
        "imported tree should contain the exported markdown"
    );
    h.process.shutdown().await;
}
