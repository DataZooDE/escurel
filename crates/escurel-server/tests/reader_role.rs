//! `ESCUREL_INDEX_BACKEND` / `ESCUREL_ROLE` — booting a real ducklake
//! reader through `EscurelConfig`, and gating its tool surface
//! (DuckLake program, PR 6).
//!
//! Offline harness, no Docker: a DuckDB-file catalog + a local-directory
//! `DATA_PATH`, `ZeroEmbedder` (model_id `"zero"`, dim 768 — matched on
//! both ends so `adopt_lake`'s manifest-compatibility gate passes). The
//! WRITER side of each test is a raw `Indexer` built directly (mirrors
//! `escurel-server/tests/snapshot_refresh.rs`'s `fresh_harness`) — there
//! is no `publish` MCP tool yet (that lands with a future admin-tool
//! PR), so a test that needs a *published* lake seeds + publishes
//! through the raw indexer first, then boots the READER half through
//! the real `EscurelConfig::from_source(..).build()` path this PR wires.
//! Real DuckDB, real `ducklake` extension, real Parquet; no mocks.

use std::collections::HashMap;
use std::sync::Arc;

use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::snapshot::{LakeConfig, ObjectStoreSecret, publish_lake};
use escurel_index::{Indexer, Migrator};
use escurel_server::EscurelConfig;
use escurel_server::config::{ConfigError, IndexBackend, ServerRole};
use escurel_storage::{FsStore, LaneStore};
use tempfile::TempDir;

const TENANT: &str = "acme";

const CUSTOMER_SKILL: (&str, &str) = (
    "markdown/skills/customer.md",
    "---\n\
     type: skill\n\
     id: customer\n\
     description: a customer\n\
     ---\n\
     # customer\n\
     \n\
     A customer is the unit of revenue.\n",
);

/// The writer-side raw harness: a real `Indexer` over its own scratch
/// DuckDB, sharing the `data_root` `FsStore` a reader boot via
/// `EscurelConfig` will also point at (`ESCUREL_SERVER_DATA_DIR`) —
/// the same shared-object-store shape production has (both writer and
/// readers read/write the same S3/GCS bucket; here, the same directory).
struct Harness {
    indexer: Arc<Indexer>,
    data_root: TempDir,
    lake_dir: TempDir,
    _db_dir: TempDir,
}

fn fresh_harness() -> Harness {
    let data_root = TempDir::new().unwrap();
    let lake_dir = TempDir::new().unwrap();
    std::fs::create_dir_all(lake_dir.path().join("data")).unwrap();
    let db_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(data_root.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::new(768));
    let conn = Connection::open(db_dir.path().join("harness.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Arc::new(Indexer::new(store, embedder, conn, TENANT).unwrap());
    Harness {
        indexer,
        data_root,
        lake_dir,
        _db_dir: db_dir,
    }
}

fn lake_config(h: &Harness) -> LakeConfig {
    LakeConfig {
        catalog_dsn: h
            .lake_dir
            .path()
            .join("catalog.ducklake")
            .to_str()
            .unwrap()
            .to_owned(),
        data_path: h.lake_dir.path().join("data").to_str().unwrap().to_owned(),
        object_store: ObjectStoreSecret::None,
    }
}

/// Seed one page and publish it as the lake's first snapshot.
async fn seed_and_publish(h: &Harness) {
    h.indexer
        .update_page(CUSTOMER_SKILL.0, CUSTOMER_SKILL.1)
        .await
        .unwrap();
    publish_lake(&h.indexer, &lake_config(h), None)
        .await
        .expect("publish");
}

fn env_map(pairs: Vec<(&str, String)>) -> HashMap<String, String> {
    pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()
}

fn source(map: HashMap<String, String>) -> impl Fn(&str) -> Option<String> {
    move |k: &str| map.get(k).cloned()
}

/// Build a reader `EscurelConfig` pointed at `h`'s lake + shared data
/// root. `refresh_secs` is deliberately tiny in tests that need to
/// observe a hot-swap; large (or irrelevant) elsewhere.
fn reader_cfg(h: &Harness, refresh_secs: &str) -> EscurelConfig {
    let lake = lake_config(h);
    EscurelConfig::from_source(&source(env_map(vec![
        (
            "ESCUREL_SERVER_DATA_DIR",
            h.data_root.path().to_str().unwrap().to_owned(),
        ),
        ("ESCUREL_SERVER_LISTEN_HTTP", "127.0.0.1:0".to_owned()),
        (
            "ESCUREL_OBSERVABILITY_METRICS_LISTEN",
            "127.0.0.1:0".to_owned(),
        ),
        ("ESCUREL_TENANT", TENANT.to_owned()),
        ("ESCUREL_EMBEDDING_PROVIDER", "zero".to_owned()),
        ("ESCUREL_INDEX_BACKEND", "ducklake".to_owned()),
        ("ESCUREL_ROLE", "reader".to_owned()),
        ("ESCUREL_DUCKLAKE_CATALOG_DSN", lake.catalog_dsn),
        ("ESCUREL_DUCKLAKE_DATA_PATH", lake.data_path),
        ("ESCUREL_SNAPSHOT_REFRESH_SECS", refresh_secs.to_owned()),
    ])))
    .expect("reader config parses")
}

async fn call(base: &str, name: &str, args: serde_json::Value) -> serde_json::Value {
    reqwest::Client::new()
        .post(format!("{base}/mcp"))
        .json(&serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": args }
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

#[tokio::test]
async fn reader_boots_and_serves_from_lake_without_local_duckdb() {
    let h = fresh_harness();
    seed_and_publish(&h).await;

    let cfg = reader_cfg(&h, "30");
    let booted = cfg
        .build()
        .await
        .expect("reader boots from a published lake");
    assert!(
        booted.refresh_handle.is_some(),
        "a ducklake reader boot must spawn a RefreshTask"
    );
    let base = format!("http://{}", booted.handle.local_addr);

    let resp = call(
        &base,
        "expand",
        serde_json::json!({ "page_id": CUSTOMER_SKILL.0 }),
    )
    .await;
    assert!(resp.get("error").is_none(), "expand failed: {resp}");
    let page = &resp["result"]["structuredContent"]["page"];
    assert_eq!(
        page["page_id"], CUSTOMER_SKILL.0,
        "reader must serve the seeded page: {resp}"
    );

    booted.refresh_handle.unwrap().shutdown().await;
    booted.handle.shutdown().await;
}

#[tokio::test]
async fn reader_rejects_update_page_with_typed_error() {
    let h = fresh_harness();
    seed_and_publish(&h).await;
    let booted = reader_cfg(&h, "30").build().await.expect("reader boots");
    let base = format!("http://{}", booted.handle.local_addr);

    let resp = call(
        &base,
        "update_page",
        serde_json::json!({ "page_id": CUSTOMER_SKILL.0, "content": CUSTOMER_SKILL.1 }),
    )
    .await;
    let err = &resp["error"];
    assert_eq!(err["code"], -32004, "unexpected error shape: {resp}");
    assert!(
        err["message"]
            .as_str()
            .unwrap()
            .contains("read-only ducklake replica"),
        "error must name the read-only-replica reason: {resp}"
    );

    booted.refresh_handle.unwrap().shutdown().await;
    booted.handle.shutdown().await;
}

#[tokio::test]
async fn reader_rejects_chat_tools_unsupported_on_replica() {
    let h = fresh_harness();
    seed_and_publish(&h).await;
    let booted = reader_cfg(&h, "30").build().await.expect("reader boots");
    let base = format!("http://{}", booted.handle.local_addr);

    let resp = call(
        &base,
        "append_message",
        serde_json::json!({
            "chat_group_id": "agent:dev-user",
            "role": "user",
            "content": "hi",
            "embed": false,
        }),
    )
    .await;
    let err = &resp["error"];
    assert_eq!(err["code"], -32005, "unexpected error shape: {resp}");
    assert!(
        err["message"].as_str().unwrap().contains("unsupported"),
        "error must name the unsupported-on-replica reason: {resp}"
    );

    booted.refresh_handle.unwrap().shutdown().await;
    booted.handle.shutdown().await;
}

/// Given this PR's design (a reader `adopt_lake`s SYNCHRONOUSLY before
/// the HTTP listener even binds — see `EscurelConfig::build`), the
/// `index_snapshot` readiness field is true for every reader that can
/// answer `/readyz` at all; there is no async cold-start window in
/// which it could observe `false`. This test asserts exactly that
/// (trivial-but-real) behaviour rather than inventing a state machine
/// this PR does not build.
#[tokio::test]
async fn readyz_gates_on_snapshot_adopted() {
    let h = fresh_harness();
    seed_and_publish(&h).await;
    let booted = reader_cfg(&h, "30").build().await.expect("reader boots");
    let base = format!("http://{}", booted.handle.local_addr);

    let ready = reqwest::Client::new()
        .get(format!("{base}/readyz"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        ready.status(),
        200,
        "a synchronously-adopted reader must be ready the instant it can be asked"
    );

    booted.refresh_handle.unwrap().shutdown().await;
    booted.handle.shutdown().await;
}

#[test]
fn single_file_writer_boot_still_works_unchanged() {
    let cfg = EscurelConfig::from_source(&source(HashMap::new())).unwrap();
    assert_eq!(cfg.index_backend, IndexBackend::SingleFile);
    assert_eq!(cfg.role, ServerRole::Writer);
    assert!(cfg.lake.is_none());
}

#[test]
fn reader_role_requires_ducklake_backend() {
    let err = EscurelConfig::from_source(&source(env_map(vec![(
        "ESCUREL_ROLE",
        "reader".to_owned(),
    )])))
    .expect_err("must reject reader role without ducklake backend");
    assert!(
        matches!(
            err,
            ConfigError::InvalidValue {
                var: "ESCUREL_ROLE",
                ..
            }
        ),
        "{err}"
    );
}

#[test]
fn ducklake_backend_requires_catalog_dsn_and_data_path() {
    let err = EscurelConfig::from_source(&source(env_map(vec![(
        "ESCUREL_INDEX_BACKEND",
        "ducklake".to_owned(),
    )])))
    .expect_err("must reject ducklake backend with no lake vars");
    assert!(
        matches!(
            err,
            ConfigError::MissingLakeField {
                var: "ESCUREL_DUCKLAKE_CATALOG_DSN"
            }
        ),
        "{err}"
    );
}

#[test]
fn ducklake_gcs_data_path_requires_gcs_credentials() {
    let err = EscurelConfig::from_source(&source(env_map(vec![
        ("ESCUREL_INDEX_BACKEND", "ducklake".to_owned()),
        (
            "ESCUREL_DUCKLAKE_CATALOG_DSN",
            "/tmp/cat.ducklake".to_owned(),
        ),
        (
            "ESCUREL_DUCKLAKE_DATA_PATH",
            "gs://bucket/prefix/".to_owned(),
        ),
    ])))
    .expect_err("must reject a gs:// data_path with no GCS credentials");
    assert!(
        matches!(
            err,
            ConfigError::MissingLakeField {
                var: "ESCUREL_DUCKLAKE_GCS_KEY_ID"
            }
        ),
        "{err}"
    );
}

#[test]
fn ducklake_local_data_path_needs_no_secret() {
    let tmp = TempDir::new().unwrap();
    let cfg = EscurelConfig::from_source(&source(env_map(vec![
        ("ESCUREL_INDEX_BACKEND", "ducklake".to_owned()),
        ("ESCUREL_ROLE", "writer".to_owned()),
        (
            "ESCUREL_DUCKLAKE_CATALOG_DSN",
            "/tmp/cat.ducklake".to_owned(),
        ),
        (
            "ESCUREL_DUCKLAKE_DATA_PATH",
            tmp.path().to_str().unwrap().to_owned(),
        ),
    ])))
    .expect("a local-directory data_path needs no object-store secret");
    let lake = cfg.lake.expect("lake config present");
    assert_eq!(lake.object_store, ObjectStoreSecret::None);
}

#[test]
fn snapshot_refresh_secs_defaults_to_30() {
    let cfg = EscurelConfig::from_source(&source(HashMap::new())).unwrap();
    assert_eq!(cfg.snapshot_refresh_secs, 30);
}

/// Acceptance sketch of the plan's `two_readers_one_writer_share_lake`
/// criterion, offline (no Docker): two independently-booted readers
/// adopting the SAME published lake must both serve the seeded page.
#[tokio::test]
async fn two_readers_share_the_same_published_lake() {
    let h = fresh_harness();
    seed_and_publish(&h).await;

    let booted_a = reader_cfg(&h, "30").build().await.expect("reader a boots");
    let booted_b = reader_cfg(&h, "30").build().await.expect("reader b boots");

    for booted in [&booted_a, &booted_b] {
        let base = format!("http://{}", booted.handle.local_addr);
        let resp = call(
            &base,
            "expand",
            serde_json::json!({ "page_id": CUSTOMER_SKILL.0 }),
        )
        .await;
        assert!(resp.get("error").is_none(), "expand failed: {resp}");
        assert_eq!(
            resp["result"]["structuredContent"]["page"]["page_id"],
            CUSTOMER_SKILL.0
        );
    }

    booted_a.refresh_handle.unwrap().shutdown().await;
    booted_a.handle.shutdown().await;
    booted_b.refresh_handle.unwrap().shutdown().await;
    booted_b.handle.shutdown().await;
}

/// A ducklake WRITER boot (unlike single-file) idempotently attaches
/// the lake on the indexer's own connection, but keeps every other
/// writer behaviour intact — normal MCP writes still work.
#[tokio::test]
async fn ducklake_writer_boots_and_still_accepts_writes() {
    let lake_dir = TempDir::new().unwrap();
    std::fs::create_dir_all(lake_dir.path().join("data")).unwrap();
    let data_dir = TempDir::new().unwrap();

    let cfg = EscurelConfig::from_source(&source(env_map(vec![
        (
            "ESCUREL_SERVER_DATA_DIR",
            data_dir.path().to_str().unwrap().to_owned(),
        ),
        ("ESCUREL_SERVER_LISTEN_HTTP", "127.0.0.1:0".to_owned()),
        (
            "ESCUREL_OBSERVABILITY_METRICS_LISTEN",
            "127.0.0.1:0".to_owned(),
        ),
        ("ESCUREL_TENANT", TENANT.to_owned()),
        ("ESCUREL_INDEX_BACKEND", "ducklake".to_owned()),
        ("ESCUREL_ROLE", "writer".to_owned()),
        (
            "ESCUREL_DUCKLAKE_CATALOG_DSN",
            lake_dir
                .path()
                .join("catalog.ducklake")
                .to_str()
                .unwrap()
                .to_owned(),
        ),
        (
            "ESCUREL_DUCKLAKE_DATA_PATH",
            lake_dir.path().join("data").to_str().unwrap().to_owned(),
        ),
    ])))
    .unwrap();
    assert_eq!(cfg.index_backend, IndexBackend::DuckLake);
    assert_eq!(cfg.role, ServerRole::Writer);

    let booted = cfg.build().await.expect("ducklake writer boots + attaches");
    assert!(
        booted.refresh_handle.is_none(),
        "a writer boot must not spawn a reader RefreshTask"
    );
    let base = format!("http://{}", booted.handle.local_addr);

    let resp = call(
        &base,
        "update_page",
        serde_json::json!({ "page_id": CUSTOMER_SKILL.0, "content": CUSTOMER_SKILL.1 }),
    )
    .await;
    assert!(
        resp.get("error").is_none(),
        "a ducklake writer must still accept normal writes: {resp}"
    );

    booted.handle.shutdown().await;
}
