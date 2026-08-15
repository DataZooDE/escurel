//! `has_shared_events` / `has_shared_chat` must be true for BOTH shared
//! backends — attached Postgres AND the lake (no Docker: DuckDB-file
//! catalog + local DATA_PATH, real ducklake extension).
//!
//! The bug this pins: `has_shared_events()` matched only
//! `EventsBackend::AttachedPostgres`, but `attach_events_lake()` sets
//! `EventsBackend::AttachedLake` — and `escurel-server`'s reader
//! dispatch gate (`SHARED_SURFACE_GATES`) consults this probe, so a
//! DuckLake-events reader wrongly rejected `capture_event` /
//! `assign_event` / `list_events` / `list_inbox` as
//! unsupported_on_replica. `has_shared_chat` already matched both
//! variants; events must mirror it.

use std::sync::Arc;

use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::snapshot::{LakeConfig, ObjectStoreSecret};
use escurel_index::{Indexer, Migrator, NewEvent};
use escurel_storage::{FsStore, LaneStore};
use tempfile::TempDir;

const TENANT: &str = "acme";

struct Harness {
    indexer: Indexer,
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
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Indexer::new(store, embedder, conn, TENANT).unwrap();
    Harness {
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

#[tokio::test]
async fn lake_attached_events_probe_reports_shared() {
    let h = fresh_harness();
    assert!(
        !h.indexer.has_shared_events(),
        "local backend is not shared"
    );

    h.indexer
        .attach_events_lake(&lake_config(&h))
        .await
        .expect("attach events lake");

    assert!(
        h.indexer.has_shared_events(),
        "a lake-attached events backend IS a shared backend — the server \
         reader gate keys the event tools off this probe"
    );

    // And the surface actually works end-to-end over the lake table.
    let captured = h
        .indexer
        .capture_event(NewEvent {
            source: "gmail".to_owned(),
            label_skill: "email".to_owned(),
            title: "over the lake".to_owned(),
            body: "b".to_owned(),
            ..Default::default()
        })
        .await
        .expect("capture over the lake");
    let inbox = h.indexer.list_inbox(None).await.expect("list_inbox");
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].event_id, captured.event_id);
}

#[tokio::test]
async fn lake_attached_chat_probe_reports_shared() {
    // Regression guard for the sibling probe: `has_shared_chat` already
    // matched both variants — keep it that way.
    let h = fresh_harness();
    h.indexer
        .attach_chat_lake(&lake_config(&h))
        .await
        .expect("attach chat lake");
    assert!(h.indexer.has_shared_chat());
}
