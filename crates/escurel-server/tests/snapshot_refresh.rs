//! `RefreshTask` — the reader's background poll/adopt/hot-swap loop
//! (DuckLake program, PR 5). Offline harness: DuckDB-file catalog +
//! local-directory DATA_PATH (no Docker), `HashEmbedder` — same shape
//! as `escurel-index/tests/ducklake_adopt.rs`. Real DuckDB, real
//! `ducklake` extension, real Parquet; no mocks.

use std::sync::Arc;
use std::time::Duration;

use duckdb::Connection;
use escurel_embed::{Embedder, HashEmbedder};
use escurel_index::snapshot::{LakeConfig, ObjectStoreSecret, adopt_lake, publish_lake};
use escurel_index::{Indexer, IndexerHandle, Migrator};
use escurel_server::snapshot_refresh::RefreshTask;
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

const ACME: (&str, &str) = (
    "markdown/instances/customer/acme-corp.md",
    "---\n\
     type: instance\n\
     skill: customer\n\
     id: acme-corp\n\
     ---\n\
     # Acme Corp\n\
     \n\
     Industrial manufacturing conglomerate headquartered in Stuttgart.\n",
);

struct Harness {
    store: Arc<dyn LaneStore>,
    indexer: Arc<Indexer>,
    _store_dir: TempDir,
    _db_dir: TempDir,
    lake_dir: TempDir,
}

fn fresh_harness() -> Harness {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let lake_dir = TempDir::new().unwrap();
    std::fs::create_dir_all(lake_dir.path().join("data")).unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(HashEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Arc::new(Indexer::new(Arc::clone(&store), embedder, conn, TENANT).unwrap());
    Harness {
        store,
        indexer,
        _store_dir: store_dir,
        _db_dir: db_dir,
        lake_dir,
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

fn reader_embedder() -> Arc<dyn Embedder> {
    Arc::new(HashEmbedder::default())
}

async fn seed(h: &Harness, pages: &[(&str, &'static str)]) {
    for (path, body) in pages {
        h.indexer.update_page(path, body).await.unwrap();
    }
}

/// Poll `handle.current()` for up to `timeout` (in 50ms steps) until it
/// serves `page_id`, returning the last-seen indexer either way. Used
/// instead of a raw sleep so the test is fast on a healthy loop and
/// still bounded on a stuck one.
async fn wait_for_page(handle: &IndexerHandle, page_id: &str, timeout: Duration) -> Arc<Indexer> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let current = handle.current();
        if serves_page(&current, page_id).await || tokio::time::Instant::now() >= deadline {
            return current;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn serves_page(indexer: &Indexer, page_id: &str) -> bool {
    indexer
        .expand(page_id, None, None)
        .await
        .ok()
        .flatten()
        .is_some()
}

#[tokio::test]
async fn reader_adopts_new_snapshot_without_restart() {
    let h = fresh_harness();
    let cfg = lake_config(&h);
    seed(&h, &[CUSTOMER_SKILL]).await;
    let first_report = publish_lake(&h.indexer, &cfg, None).await.expect("publish");

    let first_adopt = adopt_lake(&cfg, Arc::clone(&h.store), reader_embedder(), TENANT, None)
        .await
        .expect("first adopt")
        .expect("a first adopt must return an indexer");
    assert_eq!(first_adopt.snapshot_id, first_report.snapshot_id);

    let handle = IndexerHandle::fixed(first_adopt.indexer);
    let task = RefreshTask::new(
        handle.clone(),
        cfg.clone(),
        Arc::clone(&h.store),
        reader_embedder(),
        TENANT,
        Duration::from_millis(100),
        Some(first_adopt.snapshot_id),
    );
    let refresh = task.spawn();

    // Not yet published: the reader must not see ACME.
    assert!(!serves_page(&handle.current(), ACME.0).await);

    // A SECOND snapshot lands on the SAME writer's lake (no restart of
    // the reader side — the same RefreshTask/handle stays alive).
    h.indexer.update_page(ACME.0, ACME.1).await.unwrap();
    let second_report = publish_lake(&h.indexer, &cfg, Some(first_report.epoch))
        .await
        .expect("second publish");
    assert!(second_report.snapshot_id > first_report.snapshot_id);

    let latest = wait_for_page(&handle, ACME.0, Duration::from_secs(2)).await;
    assert!(
        serves_page(&latest, ACME.0).await,
        "the refresh task must hot-swap in the second snapshot within 2s"
    );

    refresh.shutdown().await;
}

#[tokio::test]
async fn inflight_query_finishes_on_old_snapshot() {
    let h = fresh_harness();
    let cfg = lake_config(&h);
    seed(&h, &[CUSTOMER_SKILL]).await;
    let first_report = publish_lake(&h.indexer, &cfg, None).await.expect("publish");
    let first_adopt = adopt_lake(&cfg, Arc::clone(&h.store), reader_embedder(), TENANT, None)
        .await
        .expect("first adopt")
        .expect("indexer");

    let handle = IndexerHandle::fixed(first_adopt.indexer);
    // Capture the "in-flight" Arc BEFORE any swap happens.
    let old = handle.current();
    assert!(serves_page(&old, CUSTOMER_SKILL.0).await);
    assert!(!serves_page(&old, ACME.0).await);

    let task = RefreshTask::new(
        handle.clone(),
        cfg.clone(),
        Arc::clone(&h.store),
        reader_embedder(),
        TENANT,
        Duration::from_millis(100),
        Some(first_adopt.snapshot_id),
    );
    let refresh = task.spawn();

    h.indexer.update_page(ACME.0, ACME.1).await.unwrap();
    publish_lake(&h.indexer, &cfg, Some(first_report.epoch))
        .await
        .expect("second publish");

    let swapped = wait_for_page(&handle, ACME.0, Duration::from_secs(2)).await;
    assert!(
        serves_page(&swapped, ACME.0).await,
        "the handle must have swapped to the newer snapshot"
    );

    // The captured `old` Arc is untouched by the swap: still answers,
    // still lacks ACME — proves the swap doesn't invalidate outstanding
    // Arcs (ArcSwap/Arc semantics; regression guard).
    assert!(
        serves_page(&old, CUSTOMER_SKILL.0).await,
        "the old Arc must keep answering after the swap"
    );
    assert!(
        !serves_page(&old, ACME.0).await,
        "the old Arc must NOT gain the page published after it was captured"
    );

    refresh.shutdown().await;
}

#[tokio::test]
async fn refresh_failure_keeps_serving_stale() {
    let h = fresh_harness();
    let cfg = lake_config(&h);
    seed(&h, &[CUSTOMER_SKILL]).await;
    let report = publish_lake(&h.indexer, &cfg, None).await.expect("publish");
    let first_adopt = adopt_lake(&cfg, Arc::clone(&h.store), reader_embedder(), TENANT, None)
        .await
        .expect("first adopt")
        .expect("indexer");
    assert_eq!(first_adopt.snapshot_id, report.snapshot_id);

    let handle = IndexerHandle::fixed(first_adopt.indexer);

    // A lake config that fails validation on every poll: the local
    // DATA_PATH points at a directory that doesn't exist (`validate`
    // rejects it before any SQL runs, see `snapshot/lake.rs`). The
    // catalog_dsn is deliberately left pointing at the real catalog so
    // this proves the FAILURE path, not a differently-shaped success.
    let mut broken_cfg = cfg.clone();
    broken_cfg.data_path = h
        .lake_dir
        .path()
        .join("no-such-dir")
        .to_str()
        .unwrap()
        .to_owned();

    let task = RefreshTask::new(
        handle.clone(),
        broken_cfg,
        Arc::clone(&h.store),
        reader_embedder(),
        TENANT,
        Duration::from_millis(50),
        Some(first_adopt.snapshot_id),
    );
    let refresh = task.spawn();

    // Give the loop several ticks to hit the broken poll repeatedly.
    tokio::time::sleep(Duration::from_millis(400)).await;

    // The task must not have panicked / aborted (join would already be
    // finished + panicked if it had) and the handle must still serve
    // the original snapshot.
    assert!(
        serves_page(&handle.current(), CUSTOMER_SKILL.0).await,
        "a poll/adopt failure must never stop the reader from serving the last-good snapshot"
    );
    assert!(!serves_page(&handle.current(), ACME.0).await);

    refresh.shutdown().await;
}
