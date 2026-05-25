//! End-to-end tests for the `EscurelAdmin::CompactLanes` RPC.
//!
//! Exercised against a real tonic server, a real `OidcVerifier`
//! against the in-process JWKS the support crate stands up, a real
//! `DuckdbCrdtBackend` over a real DuckDB file, and a real raw
//! `Connection` (handed to the gateway via `Arc<Mutex<Connection>>`)
//! that the test uses both to seed CRDT state and to inspect the
//! post-compaction row counts.
//!
//! Per `docs/spec/storage.md §Compaction`, ops with
//! `hlc <= latest_snapshot_hlc` for a page are eligible for
//! deletion after the snapshot row lands; ops with strictly greater
//! `hlc` must survive. These tests pin both halves of that contract
//! plus the path-traversal defence flagged on the sibling
//! `tenant_export` RPC (codex P2 on PR M4.5b).

use std::sync::Arc;

use duckdb::Connection;
use duckdb::params;
use escurel_crdt::{CrdtBackend, DuckdbCrdtBackend, Op, Snapshot};
use escurel_index::Migrator;
use escurel_proto::v1::escurel_admin_client::EscurelAdminClient;
use escurel_proto::v1::{CompactLanesRequest, CompactProgress};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, Opts, Role};
use tempfile::TempDir;
use tokio::sync::Mutex;
use tokio_stream::StreamExt;
use tonic::Request;
use tonic::metadata::MetadataValue;
use tonic::transport::Channel;

const TENANT: &str = "acme";

struct Harness {
    process: EscurelProcess,
    /// Shared handle on the same DuckDB connection the gateway's
    /// `CrdtBackend` is built over. The test uses it to seed
    /// `crdt_ops` / `crdt_snapshots` rows directly and to count
    /// surviving rows after compaction — no second connection,
    /// per the second-connection-stale gotcha.
    conn: Arc<Mutex<Connection>>,
    _db_dir: TempDir,
}

async fn start() -> Harness {
    let db_dir = TempDir::new().unwrap();
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let shared = Arc::new(Mutex::new(conn));

    let backend: Arc<dyn CrdtBackend> = Arc::new(DuckdbCrdtBackend::new(Arc::clone(&shared)));

    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: None,
        config_overrides: ConfigOverrides {
            crdt_backend: Some(backend),
            // The compact_lanes RPC only touches the CRDT backend,
            // so the indexer is dead weight here; disabling it also
            // sidesteps the HNSW autoload gotcha on the second
            // connection if one were opened.
            disable_indexer: true,
            ..Default::default()
        },
    })
    .await;

    Harness {
        process,
        conn: shared,
        _db_dir: db_dir,
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

/// Seed `n_pre` ops at hlc 1..=n_pre, a snapshot at hlc n_pre, and
/// `n_post` ops at hlc n_pre+1..=n_pre+n_post for `page_id`.
async fn seed_page(
    backend: &dyn CrdtBackend,
    page_id: &str,
    n_pre: i64,
    n_post: i64,
    op_payload: &[u8],
) {
    for hlc in 1..=n_pre {
        backend
            .append_op(
                page_id,
                &format!("op-{page_id}-{hlc}"),
                hlc,
                &Op::new(op_payload.to_vec()),
            )
            .await
            .unwrap();
    }
    if n_pre > 0 {
        backend
            .snapshot(page_id, n_pre, &Snapshot::new(b"snap".to_vec()))
            .await
            .unwrap();
    }
    for hlc in (n_pre + 1)..=(n_pre + n_post) {
        backend
            .append_op(
                page_id,
                &format!("op-{page_id}-{hlc}"),
                hlc,
                &Op::new(op_payload.to_vec()),
            )
            .await
            .unwrap();
    }
}

async fn count_ops(conn: &Mutex<Connection>, page_id: &str) -> i64 {
    let guard = conn.lock().await;
    guard
        .query_row(
            "SELECT count(*) FROM crdt_ops WHERE page_id = ?",
            params![page_id],
            |row| row.get(0),
        )
        .unwrap()
}

async fn drain<T>(stream: &mut tonic::Streaming<T>) -> Vec<T> {
    let mut out = Vec::new();
    while let Some(msg) = stream.next().await {
        out.push(msg.unwrap());
    }
    out
}

#[tokio::test]
async fn compact_lanes_streams_one_progress_per_page_with_subsumed_ops() {
    let h = start().await;

    // Two pages with a snapshot apiece. The third page (no
    // snapshot) must NOT appear in the stream — `compact_lanes`
    // walks `crdt_snapshots` so pages with no snapshot are
    // ineligible by construction.
    let backend = DuckdbCrdtBackend::new(Arc::clone(&h.conn));
    seed_page(&backend, "page-a", 3, 0, b"op-bytes-a").await;
    seed_page(&backend, "page-b", 2, 0, b"op-bytes-b-longer").await;
    // No snapshot, no entry in the stream.
    backend
        .append_op("page-c", "op-c-1", 1, &Op::new(b"nope".to_vec()))
        .await
        .unwrap();

    let mut client = admin_client(&h).await;
    let mut stream = client
        .compact_lanes(req(
            &admin_bearer(&h),
            CompactLanesRequest {
                tenant_id: TENANT.to_owned(),
            },
        ))
        .await
        .unwrap()
        .into_inner();

    let chunks: Vec<CompactProgress> = drain(&mut stream).await;
    assert_eq!(
        chunks.len(),
        2,
        "expected one progress per snapshotted page; got {chunks:?}"
    );
    // Total ops across the two snapshotted pages = 3 + 2 = 5.
    let total_ops: u64 = chunks.iter().map(|c| c.ops_compacted).sum();
    assert_eq!(total_ops, 5);
    // bytes_reclaimed must be strictly positive on every chunk;
    // we inserted non-empty op_bytes on each.
    for c in &chunks {
        assert!(
            c.bytes_reclaimed > 0,
            "bytes_reclaimed must be > 0 when ops are deleted; got {c:?}"
        );
    }

    h.process.shutdown().await;
}

#[tokio::test]
async fn compact_lanes_emits_zero_for_pages_with_no_snapshot() {
    // A page with a snapshot but every op already older-than-snap
    // exhausted earlier. Re-snapshot at a later hlc with no new
    // ops between: the second sweep finds zero deletable rows.
    let h = start().await;
    let backend = DuckdbCrdtBackend::new(Arc::clone(&h.conn));
    seed_page(&backend, "page-a", 2, 0, b"op-a").await;

    let mut client = admin_client(&h).await;
    // First sweep wipes the eligible ops.
    let _ = drain(
        &mut client
            .compact_lanes(req(
                &admin_bearer(&h),
                CompactLanesRequest {
                    tenant_id: TENANT.to_owned(),
                },
            ))
            .await
            .unwrap()
            .into_inner(),
    )
    .await;

    // Second sweep: the page row in `crdt_snapshots` is still
    // there, but `crdt_ops` for the page is empty, so the
    // progress chunk must report (0, 0).
    let mut stream = client
        .compact_lanes(req(
            &admin_bearer(&h),
            CompactLanesRequest {
                tenant_id: TENANT.to_owned(),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    let chunks: Vec<CompactProgress> = drain(&mut stream).await;
    assert_eq!(chunks.len(), 1, "snapshot row still warrants a chunk");
    assert_eq!(chunks[0].ops_compacted, 0);
    assert_eq!(chunks[0].bytes_reclaimed, 0);

    h.process.shutdown().await;
}

#[tokio::test]
async fn compact_lanes_actually_deletes_subsumed_ops_in_db() {
    let h = start().await;
    let backend = DuckdbCrdtBackend::new(Arc::clone(&h.conn));
    seed_page(&backend, "page-a", 4, 0, b"data").await;
    assert_eq!(count_ops(&h.conn, "page-a").await, 4, "pre-sweep");

    let mut client = admin_client(&h).await;
    let _ = drain(
        &mut client
            .compact_lanes(req(
                &admin_bearer(&h),
                CompactLanesRequest {
                    tenant_id: TENANT.to_owned(),
                },
            ))
            .await
            .unwrap()
            .into_inner(),
    )
    .await;
    assert_eq!(
        count_ops(&h.conn, "page-a").await,
        0,
        "all ops with hlc <= snapshot_hlc must be gone"
    );

    h.process.shutdown().await;
}

#[tokio::test]
async fn compact_lanes_keeps_ops_newer_than_latest_snapshot() {
    let h = start().await;
    let backend = DuckdbCrdtBackend::new(Arc::clone(&h.conn));
    // 3 ops at hlc 1..=3, snapshot at hlc 3, then 2 more ops at
    // hlc 4 and 5. Compaction must remove the first 3 only.
    seed_page(&backend, "page-a", 3, 2, b"x").await;
    assert_eq!(count_ops(&h.conn, "page-a").await, 5, "pre-sweep");

    let mut client = admin_client(&h).await;
    let mut stream = client
        .compact_lanes(req(
            &admin_bearer(&h),
            CompactLanesRequest {
                tenant_id: TENANT.to_owned(),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    let chunks: Vec<CompactProgress> = drain(&mut stream).await;
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].ops_compacted, 3);

    assert_eq!(
        count_ops(&h.conn, "page-a").await,
        2,
        "ops with hlc > snapshot_hlc must survive"
    );

    h.process.shutdown().await;
}

#[tokio::test]
async fn compact_lanes_rejects_path_traversal_tenant_id() {
    // Defence mirroring the codex P2 finding on `tenant_export`:
    // tenant_id must be validated before any downstream work.
    let h = start().await;
    let mut client = admin_client(&h).await;
    let err = client
        .compact_lanes(req(
            &admin_bearer(&h),
            CompactLanesRequest {
                tenant_id: format!("../{TENANT}"),
            },
        ))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    h.process.shutdown().await;
}

#[tokio::test]
async fn compact_lanes_still_requires_admin_role() {
    let h = start().await;
    let mut client = admin_client(&h).await;
    let err = client
        .compact_lanes(req(
            &agent_bearer(&h),
            CompactLanesRequest {
                tenant_id: TENANT.to_owned(),
            },
        ))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::PermissionDenied);
    h.process.shutdown().await;
}
