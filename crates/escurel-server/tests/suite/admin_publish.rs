//! `publish_snapshot` admin MCP tool + `PublishTask` (the writer's
//! optional periodic publish loop) — DuckLake program, PR 7.
//!
//! Offline harness, no Docker: a DuckDB-file catalog + a local-directory
//! `DATA_PATH`, `ZeroEmbedder` — mirrors `reader_role.rs`'s (PR 6) and
//! `snapshot_refresh.rs`'s (PR 5) harness shape. Real DuckDB, real
//! `ducklake` extension, real Parquet; no mocks.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::snapshot::{LakeConfig, ObjectStoreSecret, adopt_lake, publish_lake};
use escurel_index::{Indexer, IndexerHandle, Migrator};
use escurel_server::EscurelConfig;
use escurel_server::snapshot_publish::PublishTask;
use escurel_storage::{FsStore, LaneStore};
use tempfile::TempDir;

const TENANT: &str = "acme";

fn page(n: usize) -> (String, String) {
    (
        format!("markdown/instances/customer/c{n}.md"),
        format!(
            "---\ntype: instance\nskill: customer\nid: c{n}\n---\n# Customer {n}\n\nRevenue unit {n}.\n"
        ),
    )
}

const CUSTOMER_SKILL: (&str, &str) = (
    "markdown/skills/customer.md",
    "---\n\
     type: skill\n\
     id: customer\n\
     description: a customer\n\
     ---\n\
     # customer\n",
);

fn env_map(pairs: Vec<(&str, String)>) -> HashMap<String, String> {
    pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()
}

fn source(map: HashMap<String, String>) -> impl Fn(&str) -> Option<String> {
    move |k: &str| map.get(k).cloned()
}

struct LakeDirs {
    lake_dir: TempDir,
}

fn fresh_lake_dirs() -> LakeDirs {
    let lake_dir = TempDir::new().unwrap();
    std::fs::create_dir_all(lake_dir.path().join("data")).unwrap();
    LakeDirs { lake_dir }
}

fn lake_config(dirs: &LakeDirs) -> LakeConfig {
    LakeConfig {
        catalog_dsn: dirs
            .lake_dir
            .path()
            .join("catalog.ducklake")
            .to_str()
            .unwrap()
            .to_owned(),
        data_path: dirs
            .lake_dir
            .path()
            .join("data")
            .to_str()
            .unwrap()
            .to_owned(),
        object_store: ObjectStoreSecret::None,
    }
}

/// Build a ducklake WRITER `EscurelConfig` pointed at `dirs`'s lake, with
/// its own scratch `ESCUREL_SERVER_DATA_DIR`.
fn writer_cfg(data_dir: &TempDir, dirs: &LakeDirs, extra: Vec<(&str, String)>) -> EscurelConfig {
    let lake = lake_config(dirs);
    let mut pairs = vec![
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
        ("ESCUREL_EMBEDDING_PROVIDER", "zero".to_owned()),
        ("ESCUREL_INDEX_BACKEND", "ducklake".to_owned()),
        ("ESCUREL_ROLE", "writer".to_owned()),
        ("ESCUREL_DUCKLAKE_CATALOG_DSN", lake.catalog_dsn),
        ("ESCUREL_DUCKLAKE_DATA_PATH", lake.data_path),
    ];
    pairs.extend(extra);
    EscurelConfig::from_source(&source(env_map(pairs))).expect("writer config parses")
}

fn reader_conn(cfg: &LakeConfig) -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(&escurel_index::snapshot::install_load_sql(cfg))
        .unwrap();
    conn.execute_batch(&escurel_index::snapshot::attach_sql(cfg, true).unwrap())
        .unwrap();
    conn
}

fn lake_snapshot_count(cfg: &LakeConfig) -> i64 {
    let conn = reader_conn(cfg);
    conn.query_row("SELECT count(*) FROM ducklake_snapshots('lake')", [], |r| {
        r.get(0)
    })
    .unwrap()
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
async fn admin_tool_publishes_and_returns_report() {
    let data_dir = TempDir::new().unwrap();
    let dirs = fresh_lake_dirs();
    let cfg = writer_cfg(&data_dir, &dirs, vec![]);
    let lake = lake_config(&dirs);
    let booted = cfg.build().await.expect("writer boots");
    let base = format!("http://{}", booted.handle.local_addr);

    // Seed two pages so the publish has something to report.
    let (skill_path, skill_body) = CUSTOMER_SKILL;
    let resp = call(
        &base,
        "update_page",
        serde_json::json!({ "page_id": skill_path, "content": skill_body }),
    )
    .await;
    assert!(resp.get("error").is_none(), "seed skill failed: {resp}");
    let (inst_path, inst_body) = page(1);
    let resp = call(
        &base,
        "update_page",
        serde_json::json!({ "page_id": inst_path, "content": inst_body }),
    )
    .await;
    assert!(resp.get("error").is_none(), "seed instance failed: {resp}");

    let resp = call(&base, "publish_snapshot", serde_json::json!({})).await;
    assert!(resp.get("error").is_none(), "publish failed: {resp}");
    let result = &resp["result"]["structuredContent"];
    assert_eq!(result["skipped"], false, "{resp}");
    // 3, not 2: every writer boot also seeds the mandatory `escurel`
    // meta-skill page (`Indexer::ensure_meta_skill`), published right
    // alongside the two pages this test seeded.
    assert_eq!(result["pages"], 3, "{resp}");
    assert_eq!(result["blocks"], 3, "{resp}");
    assert!(
        result["snapshot_id"].as_i64().unwrap() >= 0,
        "must report a real snapshot id: {resp}"
    );

    // A second call with nothing changed must be a clean no-op.
    let resp2 = call(&base, "publish_snapshot", serde_json::json!({})).await;
    let result2 = &resp2["result"]["structuredContent"];
    assert_eq!(result2["skipped"], true, "{resp2}");

    booted.handle.shutdown().await;

    // Round-trip: a fresh reader adopts the exact lake the admin tool
    // just published.
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(data_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::new(768));
    let adopted = adopt_lake(&lake, store, embedder, TENANT, None)
        .await
        .expect("adopt")
        .expect("lake was published, adopt must return Some");
    let expanded = adopted
        .indexer
        .expand(&inst_path, None, None)
        .await
        .expect("expand")
        .expect("reader must serve the published page");
    assert_eq!(expanded.page.page_id, inst_path);
}

#[tokio::test]
async fn publish_unavailable_on_single_file_backend() {
    let cfg = EscurelConfig::from_source(&source(env_map(vec![
        ("ESCUREL_SERVER_LISTEN_HTTP", "127.0.0.1:0".to_owned()),
        (
            "ESCUREL_OBSERVABILITY_METRICS_LISTEN",
            "127.0.0.1:0".to_owned(),
        ),
        ("ESCUREL_TENANT", TENANT.to_owned()),
        ("ESCUREL_EMBEDDING_PROVIDER", "zero".to_owned()),
    ])))
    .unwrap();
    let data_dir = TempDir::new().unwrap();
    let mut cfg = cfg;
    cfg.data_dir = data_dir.path().to_path_buf();
    let booted = cfg.build().await.expect("single-file writer boots");
    let base = format!("http://{}", booted.handle.local_addr);

    let resp = call(&base, "publish_snapshot", serde_json::json!({})).await;
    let err = &resp["error"];
    assert_eq!(err["code"], -32006, "unexpected error shape: {resp}");
    assert!(
        err["message"].as_str().unwrap().contains("unavailable"),
        "error must name the reason: {resp}"
    );

    booted.handle.shutdown().await;
}

#[tokio::test]
async fn publish_unavailable_on_reader() {
    let data_dir = TempDir::new().unwrap();
    let dirs = fresh_lake_dirs();

    // Seed + publish once from a raw writer indexer so the reader has a
    // lake to boot from.
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(data_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::new(768));
    let db_dir = TempDir::new().unwrap();
    let conn = Connection::open(db_dir.path().join("harness.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let seed_indexer = Indexer::new(Arc::clone(&store), Arc::clone(&embedder), conn, TENANT)
        .expect("seed indexer");
    let (skill_path, skill_body) = CUSTOMER_SKILL;
    seed_indexer
        .update_page(skill_path, skill_body)
        .await
        .unwrap();
    let lake = lake_config(&dirs);
    publish_lake(&seed_indexer, &lake, None)
        .await
        .expect("seed publish");

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
        ("ESCUREL_EMBEDDING_PROVIDER", "zero".to_owned()),
        ("ESCUREL_INDEX_BACKEND", "ducklake".to_owned()),
        ("ESCUREL_ROLE", "reader".to_owned()),
        ("ESCUREL_DUCKLAKE_CATALOG_DSN", lake.catalog_dsn.clone()),
        ("ESCUREL_DUCKLAKE_DATA_PATH", lake.data_path.clone()),
        ("ESCUREL_SNAPSHOT_REFRESH_SECS", "30".to_owned()),
    ])))
    .expect("reader config parses");
    let booted = cfg.build().await.expect("reader boots");
    let base = format!("http://{}", booted.handle.local_addr);

    let resp = call(&base, "publish_snapshot", serde_json::json!({})).await;
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

/// Publish `ESCUREL_SNAPSHOT_KEEP + 3` times with a distinct page each
/// time via the admin tool, and assert the lake's snapshot count settles
/// at the retention target (empirically verified: `ducklake_expire_snapshots`
/// prunes down to exactly `keep`, never below — see
/// docs/notes/discovered/2026-07-18-ducklake-snapshot-gc.md).
#[tokio::test]
async fn expire_keeps_recent_snapshots() {
    let data_dir = TempDir::new().unwrap();
    let dirs = fresh_lake_dirs();
    let keep = 3u32;
    let cfg = writer_cfg(
        &data_dir,
        &dirs,
        vec![("ESCUREL_SNAPSHOT_KEEP", keep.to_string())],
    );
    let lake = lake_config(&dirs);
    let booted = cfg.build().await.expect("writer boots");
    let base = format!("http://{}", booted.handle.local_addr);

    let total_publishes = keep as usize + 3;
    for n in 0..total_publishes {
        let (path, body) = page(n);
        let resp = call(
            &base,
            "update_page",
            serde_json::json!({ "page_id": path, "content": body }),
        )
        .await;
        assert!(resp.get("error").is_none(), "seed page {n} failed: {resp}");
        let resp = call(&base, "publish_snapshot", serde_json::json!({})).await;
        assert!(resp.get("error").is_none(), "publish {n} failed: {resp}");
    }

    booted.handle.shutdown().await;
    assert_eq!(
        lake_snapshot_count(&lake),
        i64::from(keep),
        "gc must settle the snapshot count at exactly ESCUREL_SNAPSHOT_KEEP"
    );
}

/// `PublishTask` (in-process, not via a full server boot — mirrors
/// `snapshot_refresh.rs`'s style): fires on a dirty indexer, is a no-op
/// on a clean one.
#[tokio::test]
async fn periodic_publish_fires_when_dirty_and_skips_when_clean() {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let dirs = fresh_lake_dirs();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::new(768));
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Arc::new(Indexer::new(Arc::clone(&store), embedder, conn, TENANT).unwrap());
    let handle = IndexerHandle::fixed(Arc::clone(&indexer));
    let lake = lake_config(&dirs);
    let last_published_epoch = Arc::new(std::sync::Mutex::new(None));

    let task = PublishTask::new(
        handle,
        lake.clone(),
        Duration::from_millis(30),
        5,
        Arc::clone(&last_published_epoch),
    );
    let publish_handle = task.spawn();

    let (skill_path, skill_body) = CUSTOMER_SKILL;
    indexer.update_page(skill_path, skill_body).await.unwrap();

    // Wait for the loop to observe the dirty epoch and publish it.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if last_published_epoch.lock().unwrap().is_some() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "periodic publish never fired on a dirty indexer"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        lake_snapshot_count(&lake),
        2,
        "attach + one publish snapshot"
    );

    // Let a few more clean ticks pass — the snapshot count must not
    // advance (publish_lake's own dirty-check skips a clean tick).
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        lake_snapshot_count(&lake),
        2,
        "a clean indexer must not produce more snapshots"
    );

    publish_handle.shutdown().await;
}
