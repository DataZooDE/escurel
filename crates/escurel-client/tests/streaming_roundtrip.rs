//! Long-op + live-session paths over the typed `escurel-client`:
//! the admin one-shot long-ops (`rebuild`, `compact_lanes`,
//! `tenant_export` / `tenant_import`) and the WS `live_session`.
//!
//! Real gateway via `escurel-test-support`, real HTTP (MCP + WS)
//! transport, real CRDT backend (`DuckdbCrdtBackend`) so
//! `live_session` has a live doc to attach to. No mocks at the
//! boundary (CLAUDE principle 2).

use std::path::PathBuf;
use std::sync::Arc;

use duckdb::Connection;
use escurel_admin::{FsTenantStore, TenantStore};
use escurel_client::{
    AdminClient, Client, CompactLanesRequest, LiveOp, RebuildRequest, SecretString,
    TenantCreateRequest, TenantExportRequest, TenantSpec,
};
use escurel_crdt::{CrdtBackend, DuckdbCrdtBackend};
use escurel_index::Migrator;
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, Opts, Role};
use futures_util::StreamExt as _;
use tempfile::TempDir;
use tokio::sync::Mutex;

const TENANT: &str = "acme";

/// Build a `DuckdbCrdtBackend` over a fresh on-disk DuckDB. The
/// tempdir is leaked so the file outlives the backend for the test's
/// duration (the test process is short-lived).
fn crdt_backend() -> Arc<dyn CrdtBackend> {
    let dir = TempDir::new().unwrap();
    let conn = Connection::open(dir.path().join("crdt.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    std::mem::forget(dir);
    Arc::new(DuckdbCrdtBackend::new(Arc::new(Mutex::new(conn))))
}

struct AdminHarness {
    process: EscurelProcess,
    tenants_root: PathBuf,
    _tenants_dir: TempDir,
}

/// Gateway with a real tenant store + CRDT backend so the admin
/// long-ops have something to act on.
async fn start_admin() -> AdminHarness {
    let tenants_dir = TempDir::new().unwrap();
    let tenants_root = tenants_dir.path().to_path_buf();
    let store: Arc<dyn TenantStore> = Arc::new(FsTenantStore::new(tenants_root.clone()));
    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            tenant_store: Some(store),
            crdt_backend: Some(crdt_backend()),
            ..Default::default()
        },
        ..Default::default()
    })
    .await;
    AdminHarness {
        process,
        tenants_root,
        _tenants_dir: tenants_dir,
    }
}

async fn admin(p: &EscurelProcess) -> AdminClient {
    let token = p.mint_token(TENANT, Role::Admin);
    AdminClient::connect(p.base_url(), SecretString::from(token))
        .await
        .unwrap()
}

/// `rebuild` returns the terminal `{done, total}` over the one-shot
/// MCP transport (no streaming).
#[tokio::test]
async fn rebuild_returns_terminal_progress() {
    let h = start_admin().await;
    let client = admin(&h.process).await;
    // Default tenant the gateway's indexer is bound to is "acme".
    let progress = client
        .rebuild(RebuildRequest {
            tenant_id: TENANT.to_owned(),
            scope: String::new(),
        })
        .await
        .expect("rebuild");
    // done == total for a completed rebuild (terminal counts).
    assert_eq!(progress.done, progress.total);
    h.process.shutdown().await;
}

/// `compact_lanes` returns the terminal `{ops_compacted,
/// bytes_reclaimed}`.
#[tokio::test]
async fn compact_lanes_returns_terminal_counts() {
    let h = start_admin().await;
    let client = admin(&h.process).await;
    let progress = client
        .compact_lanes(CompactLanesRequest {
            tenant_id: TENANT.to_owned(),
        })
        .await
        .expect("compact_lanes");
    // A fresh backend with no ops reclaims nothing — the assertion is
    // that the call succeeds and returns the terminal counts shape.
    let _ = (progress.ops_compacted, progress.bytes_reclaimed);
    h.process.shutdown().await;
}

/// `tenant_export` decodes the base64 tarball to bytes; `tenant_import`
/// re-encodes and replays it. Round-trips a freshly created tenant.
#[tokio::test]
async fn tenant_export_then_import_round_trips() {
    let h = start_admin().await;
    let client = admin(&h.process).await;

    client
        .tenant_create(TenantCreateRequest {
            spec: Some(TenantSpec {
                tenant_id: "globex".to_owned(),
                display_name: "Globex".to_owned(),
            }),
        })
        .await
        .expect("tenant_create");
    assert!(h.tenants_root.join("globex").join("tenant.json").is_file());

    let bytes = client
        .tenant_export(TenantExportRequest {
            tenant_id: "globex".to_owned(),
        })
        .await
        .expect("tenant_export");
    assert!(!bytes.is_empty(), "export tarball must be non-empty");

    let imported = client
        .tenant_import("globex", bytes)
        .await
        .expect("tenant_import");
    assert!(imported > 0, "import must report the bytes it ingested");

    h.process.shutdown().await;
}

/// `live_session` over the WS channel: open a session via the raw
/// `open_session` tool to learn its id + seed content, then drive the
/// WS channel with one op and assert an `op_ack` comes back.
#[tokio::test]
async fn live_session_attach_and_one_op_acks() {
    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            crdt_backend: Some(crdt_backend()),
            disable_indexer: true,
            ..Default::default()
        },
        ..Default::default()
    })
    .await;
    let client = process.client_for(TENANT, Role::Agent).await;

    // Open a session (and learn its id) via the raw MCP tool.
    let opened = client
        .call_raw(
            "open_session",
            serde_json::json!({ "page_id": "markdown/instances/customer__acme.md" }),
        )
        .await
        .expect("open_session");
    let session = opened["session"]
        .as_str()
        .expect("session id present")
        .to_owned();

    // Build one Loro op against a fresh doc.
    let op_bytes = {
        use loro::LoroDoc;
        let doc = LoroDoc::new();
        doc.get_text("content").insert(0, "hello live").unwrap();
        doc.export(loro::ExportMode::Snapshot).unwrap()
    };

    // Drive the WS channel: attach (first op carries the session id),
    // then the op; expect one ack back.
    let ops = futures_util::stream::iter(vec![LiveOp {
        session: session.clone(),
        op: op_bytes,
    }]);
    let mut acks = client.live_session(ops).await.expect("live_session open");
    let ack = acks
        .next()
        .await
        .expect("at least one ack")
        .expect("ack ok");
    assert_eq!(ack.session, session, "ack echoes the session id");
    assert!(
        !ack.merged_version.is_empty(),
        "ack carries a merged version"
    );

    process.shutdown().await;
}

/// `live_session` against a gateway with no CRDT backend wired is
/// refused at the WS upgrade (or first frame), surfacing as a
/// `LiveSession` error rather than a panic.
#[tokio::test]
async fn live_session_without_backend_errors() {
    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            disable_indexer: true,
            ..Default::default()
        },
        ..Default::default()
    })
    .await;
    let client = process.client_for(TENANT, Role::Agent).await;

    let ops = futures_util::stream::iter(vec![LiveOp {
        session: "sess-does-not-exist".to_owned(),
        op: vec![1, 2, 3],
    }]);
    let opened = client.live_session(ops).await;
    // Either the open fails, or the first ack is an error.
    let errored = match opened {
        Err(_) => true,
        Ok(mut acks) => matches!(acks.next().await, Some(Err(_)) | None),
    };
    assert!(errored, "live_session must error without a CRDT backend");

    process.shutdown().await;
}

// Silence unused-import lints when only a subset of the helpers are
// exercised by a given build configuration.
#[allow(dead_code)]
fn _client_type_anchor(_: &Client) {}
