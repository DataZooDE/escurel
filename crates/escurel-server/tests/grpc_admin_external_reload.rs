//! End-to-end tests for the last two `EscurelAdmin` RPCs:
//! `attach_external` and `embedding_reload`.
//!
//! No mocks at the boundary (CLAUDE.md principle 2):
//!
//! * `attach_external` runs DuckDB's native `ATTACH` against a
//!   *second real* DuckDB file holding a table + a row, then a real
//!   `[[query::*]]` stored query reads that row back through the
//!   attached catalog — proving the external lane is wired into the
//!   indexer's live connection.
//! * `embedding_reload` boots the gateway *degraded* (a real
//!   `ReloadableEmbedder` in the degraded state) with a real
//!   embedder factory that fails its first build and succeeds its
//!   second; the RPC swaps the freshly-built (real `ZeroEmbedder`)
//!   model in and `/readyz` flips `embedder` from `false` to `true`.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use duckdb::Connection;
use escurel_embed::{Embedder, ReloadableEmbedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_proto::v1::escurel_admin_client::EscurelAdminClient;
use escurel_proto::v1::escurel_client::EscurelClient;
use escurel_proto::v1::{AttachExternalRequest, EmbeddingReloadRequest, RunStoredQueryRequest};
use escurel_server::EmbedderFactory;
use escurel_storage::{FsStore, Key, LaneStore};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, Opts, Role};
use tempfile::TempDir;
use tonic::Request;
use tonic::metadata::MetadataValue;
use tonic::transport::Channel;

const TENANT: &str = "acme";
const DIM: usize = 768;

// --- shared client plumbing ----------------------------------------

async fn admin_client(p: &EscurelProcess) -> EscurelAdminClient<Channel> {
    let endpoint = p.grpc_endpoint().expect("grpc endpoint").to_owned();
    let channel = Channel::from_shared(endpoint)
        .unwrap()
        .connect()
        .await
        .unwrap();
    EscurelAdminClient::new(channel)
}

async fn agent_client(p: &EscurelProcess) -> EscurelClient<Channel> {
    let endpoint = p.grpc_endpoint().expect("grpc endpoint").to_owned();
    let channel = Channel::from_shared(endpoint)
        .unwrap()
        .connect()
        .await
        .unwrap();
    EscurelClient::new(channel)
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

// --- attach_external ------------------------------------------------

/// Build a second real DuckDB file with one table + one row and
/// return its on-disk path (kept alive by the returned `TempDir`).
fn make_external_db(rows: &[(i64, &str)]) -> (TempDir, String) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("external.duckdb");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("CREATE TABLE events(id BIGINT, label VARCHAR);")
        .unwrap();
    for (id, label) in rows {
        conn.execute(
            "INSERT INTO events(id, label) VALUES (?, ?)",
            duckdb::params![id, label],
        )
        .unwrap();
    }
    drop(conn);
    (dir, path.to_string_lossy().into_owned())
}

/// Spawn a gateway whose indexer we own, so the `attach_external`
/// ATTACH and the follow-up `run_stored_query` hit the same live
/// connection. `pages` are seeded directly through the owned
/// indexer + its lane store (no side-door: `update_page` is exactly
/// the gateway's write path). Returns the process plus the tempdirs
/// that back the indexer (the caller keeps them alive).
async fn spawn_with_owned_indexer(pages: &[(&str, &str)]) -> (EscurelProcess, TempDir, TempDir) {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Arc::new(Indexer::new(Arc::clone(&store), embedder, conn, TENANT).unwrap());

    for (path, body) in pages {
        let key = Key::new(TENANT, (*path).to_owned()).unwrap();
        store
            .write(&key, bytes::Bytes::copy_from_slice(body.as_bytes()))
            .await
            .unwrap();
        indexer.update_page(path, body).await.unwrap();
    }

    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: None,
        config_overrides: ConfigOverrides {
            indexer: Some(indexer),
            ..Default::default()
        },
    })
    .await;
    (process, store_dir, db_dir)
}

/// A `[[query::ext-events]]` stored query that reads the attached
/// external catalog. `attach_external` derives the catalog alias
/// from the source path's stem (`external`), so the SQL targets
/// `external.events`.
fn ext_query_page(alias: &str) -> String {
    format!(
        "---\ntype: instance\nskill: query\nid: ext-events\ndb: relational\n\
         sql: |\n  SELECT id, label FROM {alias}.events ORDER BY id\n---\n# ext events\n"
    )
}

#[tokio::test]
async fn attach_external_makes_external_table_queryable() {
    let (_ext_dir, source_url) = make_external_db(&[(1, "alpha"), (2, "beta")]);

    // The external db file is `external.duckdb`; the ATTACH alias is
    // derived from the stem → `external`.
    let body = ext_query_page("external");
    let pages = [("markdown/instances/query/ext-events.md", body.as_str())];
    let (process, _store_dir, _db_dir) = spawn_with_owned_indexer(&pages).await;

    let mut admin = admin_client(&process).await;
    let resp = admin
        .attach_external(req(
            &bearer(&process, Role::Admin),
            AttachExternalRequest {
                tenant_id: TENANT.to_owned(),
                source_url: source_url.clone(),
            },
        ))
        .await
        .expect("attach_external should succeed")
        .into_inner();
    assert_eq!(
        resp.source_id, "external",
        "source_id should be the derived catalog alias"
    );

    // Now read the external table through a real stored query on the
    // agent surface — proving the ATTACH wired the catalog into the
    // indexer's live connection.
    let mut agent = agent_client(&process).await;
    let out = agent
        .run_stored_query(req(
            &bearer(&process, Role::Agent),
            RunStoredQueryRequest {
                query_id: "ext-events".to_owned(),
                params_json: String::new(),
            },
        ))
        .await
        .expect("run_stored_query over external catalog should succeed")
        .into_inner();
    let rows: serde_json::Value = serde_json::from_str(&out.rows_json).unwrap();
    let arr = rows.as_array().unwrap();
    assert_eq!(arr.len(), 2, "external table had two rows: {arr:?}");
    assert_eq!(arr[0]["label"], "alpha");
    assert_eq!(arr[1]["label"], "beta");

    process.shutdown().await;
}

#[tokio::test]
async fn attach_external_rejects_path_traversal_tenant_id() {
    let (_ext_dir, source_url) = make_external_db(&[(1, "x")]);
    let (process, _store_dir, _db_dir) = spawn_with_owned_indexer(&[]).await;
    let mut admin = admin_client(&process).await;
    let err = admin
        .attach_external(req(
            &bearer(&process, Role::Admin),
            AttachExternalRequest {
                tenant_id: format!("../{TENANT}"),
                source_url,
            },
        ))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    process.shutdown().await;
}

#[tokio::test]
async fn attach_external_rejects_unsafe_source_url() {
    let (process, _store_dir, _db_dir) = spawn_with_owned_indexer(&[]).await;
    let mut admin = admin_client(&process).await;
    // A source_url carrying a quote + a stacked statement — the
    // classic ATTACH injection. Must be rejected before it reaches
    // the SQL.
    let err = admin
        .attach_external(req(
            &bearer(&process, Role::Admin),
            AttachExternalRequest {
                tenant_id: TENANT.to_owned(),
                source_url: "evil.duckdb' AS x); DROP TABLE pages; --".to_owned(),
            },
        ))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    process.shutdown().await;
}

#[tokio::test]
async fn attach_external_still_requires_admin_role() {
    let (_ext_dir, source_url) = make_external_db(&[(1, "x")]);
    let (process, _store_dir, _db_dir) = spawn_with_owned_indexer(&[]).await;
    let mut admin = admin_client(&process).await;
    let err = admin
        .attach_external(req(
            &bearer(&process, Role::Agent),
            AttachExternalRequest {
                tenant_id: TENANT.to_owned(),
                source_url,
            },
        ))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::PermissionDenied);
    process.shutdown().await;
}

// --- embedding_reload ----------------------------------------------

/// Spawn a gateway with no reloadable embedder wired (default).
async fn spawn_no_reload() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: None,
        config_overrides: ConfigOverrides::default(),
    })
    .await
}

#[tokio::test]
async fn embedding_reload_without_reloadable_returns_failed_precondition() {
    let process = spawn_no_reload().await;
    let mut admin = admin_client(&process).await;
    let err = admin
        .embedding_reload(req(
            &bearer(&process, Role::Admin),
            EmbeddingReloadRequest::default(),
        ))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::FailedPrecondition);
    process.shutdown().await;
}

#[tokio::test]
async fn embedding_reload_recovers_from_degraded_start() {
    // A real reloadable embedder, booted degraded (placeholder
    // ZeroEmbedder, is_loaded = false). The factory fails its first
    // call (simulating a cold model load that could not reach the
    // weights) and succeeds the second, returning a *real*
    // ZeroEmbedder — no mock of the Embedder trait.
    let reload = Arc::new(ReloadableEmbedder::degraded(DIM));
    let calls = Arc::new(AtomicU32::new(0));
    let calls_in_factory = Arc::clone(&calls);
    let factory: EmbedderFactory = Arc::new(move || {
        let calls = Arc::clone(&calls_in_factory);
        Box::pin(async move {
            let n = calls.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Err("model weights unreachable (cold start)".to_owned())
            } else {
                let e: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::new(DIM));
                Ok((e, "test-model-v2".to_owned()))
            }
        })
    });

    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: None,
        config_overrides: ConfigOverrides {
            embedder_reload: Some(Arc::clone(&reload)),
            embedder_factory: Some(factory),
            ..Default::default()
        },
    })
    .await;

    // Sanity: booted degraded — the readiness probe behind /readyz
    // reports embedder = false.
    assert!(!reload.is_loaded(), "should boot degraded");

    let mut admin = admin_client(&process).await;

    // First reload attempt fails (factory's first build errors) →
    // internal, still degraded.
    let err = admin
        .embedding_reload(req(
            &bearer(&process, Role::Admin),
            EmbeddingReloadRequest::default(),
        ))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::Internal);
    assert!(!reload.is_loaded(), "first reload failed; still degraded");

    // Second attempt succeeds → the real embedder is swapped in and
    // the revision string is returned.
    let resp = admin
        .embedding_reload(req(
            &bearer(&process, Role::Admin),
            EmbeddingReloadRequest::default(),
        ))
        .await
        .expect("second reload should succeed")
        .into_inner();
    assert!(
        !resp.model_revision.is_empty(),
        "model_revision must be reported on success"
    );
    assert!(reload.is_loaded(), "reload should flip is_loaded true");

    process.shutdown().await;
}

#[tokio::test]
async fn embedding_reload_still_requires_admin_role() {
    let reload = Arc::new(ReloadableEmbedder::degraded(DIM));
    let factory: EmbedderFactory = Arc::new(move || {
        Box::pin(async move {
            let e: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::new(DIM));
            Ok((e, "test-model".to_owned()))
        })
    });
    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: None,
        config_overrides: ConfigOverrides {
            embedder_reload: Some(reload),
            embedder_factory: Some(factory),
            ..Default::default()
        },
    })
    .await;
    let mut admin = admin_client(&process).await;
    let err = admin
        .embedding_reload(req(
            &bearer(&process, Role::Agent),
            EmbeddingReloadRequest::default(),
        ))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::PermissionDenied);
    process.shutdown().await;
}
