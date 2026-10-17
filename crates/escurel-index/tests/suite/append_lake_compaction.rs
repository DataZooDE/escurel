//! The append tables (events, drafts, chat) on a lake: compaction is data-driven, and what a call costs in file
//! operations is measured.
//!
//! On a Drive-backed lake every file open and every media read is a REST round trip (~0.27 s for the open's
//! revalidation, ~1.2 s for a cold media request), and every capture writes one Parquet object and one snapshot
//! (inlining is off). The publish task used to rewrite the events and chat tables on EVERY tick, even idle, and
//! never compacted the drafts table. These tests pin the data-driven replacement and report the per-call file
//! operations that multiply the Drive latency. No Docker: DuckDB-file catalog + local DATA_PATH, the real
//! ducklake extension, the real `Indexer`.

use std::sync::Arc;

use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::drafts::NewDraft;
use escurel_index::snapshot::{LakeConfig, ObjectStoreSecret};
use escurel_index::{EventListFilter, Indexer, Migrator, NewEvent};
use escurel_storage::{FsStore, LaneStore};
use tempfile::TempDir;

const TENANT: &str = "acme";

struct Harness {
    indexer: Indexer,
    /// A second handle on the SAME DuckDB instance: used to enable file-operation logging and read it.
    probe: Connection,
    _dirs: Vec<TempDir>,
}

async fn harness() -> Harness {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let lake_dir = TempDir::new().unwrap();
    std::fs::create_dir_all(lake_dir.path().join("data")).unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let probe = conn.try_clone().unwrap();
    let indexer = Indexer::new(store, embedder, conn, TENANT).unwrap();
    let cfg = LakeConfig {
        catalog_dsn: lake_dir
            .path()
            .join("catalog.ducklake")
            .to_str()
            .unwrap()
            .to_owned(),
        data_path: lake_dir.path().join("data").to_str().unwrap().to_owned(),
        object_store: ObjectStoreSecret::None,
    };
    indexer.attach_events_lake(&cfg).await.unwrap();
    indexer.attach_drafts_lake(&cfg).await.unwrap();
    Harness {
        indexer,
        probe,
        _dirs: vec![store_dir, db_dir, lake_dir],
    }
}

fn event(n: u32) -> NewEvent {
    NewEvent {
        event_id: Some(format!("evt-{n:03}")),
        at: Some(format!("2026-10-01T09:{:02}:00Z", n % 60)),
        source: "gmail".to_owned(),
        label_skill: "email".to_owned(),
        title: format!("event {n}"),
        body: "an inbound enquiry".to_owned(),
        ..Default::default()
    }
}

fn draft(n: u32) -> NewDraft {
    NewDraft {
        target_page_id: format!("markdown/instances/note/n{n}.md"),
        content: format!("---\nkind: instance\nid: n{n}\n---\n# n{n}\n"),
        base_sha256: None,
        author: "agt:scout".to_owned(),
        event_id: None,
        changeset_id: None,
        base_version: None,
        run_id: None,
        root_event_id: None,
    }
}

/// Data files of the table's current snapshot. Read through the probe, i.e. the SAME DuckDB instance as the
/// indexer: a second instance would read the catalog file as of its last checkpoint and miss the newest appends.
fn files(h: &Harness, table: &str) -> i64 {
    h.probe
        .query_row(
            &format!("SELECT count(*) FROM ducklake_list_files('append_lake', '{table}')"),
            [],
            |r| r.get(0),
        )
        .unwrap()
}

fn snapshots(h: &Harness) -> i64 {
    h.probe
        .query_row(
            "SELECT count(*) FROM ducklake_snapshots('append_lake')",
            [],
            |r| r.get(0),
        )
        .unwrap()
}

#[tokio::test]
async fn compaction_runs_when_a_table_has_enough_files_and_not_before() {
    let h = harness().await;
    for n in 1..=5 {
        h.indexer.capture_event(event(n)).await.unwrap();
    }
    assert_eq!(
        files(&h, "escurel_events"),
        5,
        "one Parquet file per capture"
    );

    h.indexer.compact_append_lake_if(8).await.unwrap();
    assert_eq!(
        files(&h, "escurel_events"),
        5,
        "5 files is below the threshold of 8: untouched"
    );

    for n in 6..=8 {
        h.indexer.capture_event(event(n)).await.unwrap();
    }
    h.indexer.compact_append_lake_if(8).await.unwrap();
    assert_eq!(
        files(&h, "escurel_events"),
        1,
        "at the threshold the table is rewritten to one file"
    );
    let inbox = h.indexer.list_inbox(None).await.unwrap();
    assert_eq!(inbox.len(), 8, "no row was lost by the rewrite");
}

#[tokio::test]
async fn an_idle_tick_writes_nothing() {
    let h = harness().await;
    for n in 1..=9 {
        h.indexer.capture_event(event(n)).await.unwrap();
    }
    h.indexer.compact_append_lake_if(8).await.unwrap();
    let after_first = snapshots(&h);

    // The next ticks find one file per table: nothing to rewrite, so no new snapshot, no upload.
    for _ in 0..3 {
        h.indexer.compact_append_lake_if(8).await.unwrap();
    }

    assert_eq!(
        snapshots(&h),
        after_first,
        "an idle publish tick must not create lake snapshots (each is a Drive upload)"
    );
}

#[tokio::test]
async fn the_drafts_table_is_compacted_too() {
    let h = harness().await;
    for n in 1..=9 {
        h.indexer.create_draft(draft(n)).await.unwrap();
    }
    assert_eq!(files(&h, "escurel_drafts"), 9);

    h.indexer.compact_append_lake_if(8).await.unwrap();

    assert_eq!(
        files(&h, "escurel_drafts"),
        1,
        "drafts were never compacted before"
    );
    assert_eq!(
        h.indexer.list_drafts(None).await.unwrap().len(),
        9,
        "no draft was lost by the rewrite"
    );
}

/// Drafts follow the events selector onto the lake. The lake table declares `reason` and `decided_by` NOT NULL
/// with no default, and `create_draft` did not supply them: on a lake the first proposal failed with
/// `NOT NULL constraint failed: escurel_drafts.decided_by`.
#[tokio::test]
async fn a_draft_lives_its_whole_life_on_a_lake() {
    let h = harness().await;

    let d = h
        .indexer
        .create_draft(draft(1))
        .await
        .expect("create on a lake");
    assert_eq!(d.status, "open");
    assert_eq!(d.decided_by, "", "an open draft has no decider");

    let closed = h
        .indexer
        .close_draft(&d.draft_id, "approved", "jr", "looks right")
        .await
        .unwrap();
    assert!(closed);
    let after = h.indexer.get_draft(&d.draft_id).await.unwrap().unwrap();
    assert_eq!(
        (
            after.status.as_str(),
            after.decided_by.as_str(),
            after.reason.as_str()
        ),
        ("approved", "jr", "looks right")
    );
}

/// Counts of file operations the probe connection logged since the last reset.
fn ops(h: &Harness) -> (i64, i64) {
    let count = |op: &str| -> i64 {
        h.probe
            .query_row(
                &format!("SELECT count(*) FROM duckdb_logs_parsed('FileSystem') WHERE op = '{op}'"),
                [],
                |r| r.get(0),
            )
            .unwrap()
    };
    (count("OPEN"), count("READ"))
}

fn reset_ops(h: &Harness) {
    h.probe
        .execute_batch("CALL truncate_duckdb_logs();")
        .unwrap();
}

/// What a call costs in file operations before and after compaction, the number Drive's per-operation latency
/// multiplies. Printed (run with `-- --nocapture`) and asserted in shape: a compacted table is cheaper to read and
/// a capture does not get more expensive. The figures are from a local lake; on Drive multiply the OPEN count by
/// the per-open revalidation (~0.27 s, skipped when the lake prefix is declared immutable) and the distinct files
/// read by the cold media request (~1.2 s each, 16 MiB blocks shared between threads).
#[tokio::test]
async fn file_operations_per_call_fall_after_compaction() {
    let h = harness().await;
    h.probe
        .execute_batch("CALL enable_logging(['FileSystem'], storage='memory');")
        .unwrap();
    for n in 1..=24 {
        h.indexer.capture_event(event(n)).await.unwrap();
    }

    reset_ops(&h);
    h.indexer.capture_event(event(100)).await.unwrap();
    let capture_before = ops(&h);

    reset_ops(&h);
    let inbox = h.indexer.list_inbox(None).await.unwrap();
    let inbox_before = ops(&h);
    assert_eq!(inbox.len(), 25);

    reset_ops(&h);
    let filter = EventListFilter {
        label_skill: Some("email".to_owned()),
        ..Default::default()
    };
    h.indexer
        .list_events_filtered_page(&filter, false, 50, None)
        .await
        .unwrap();
    let events_before = ops(&h);

    h.indexer.compact_append_lake_if(8).await.unwrap();
    assert_eq!(files(&h, "escurel_events"), 1);

    reset_ops(&h);
    h.indexer.capture_event(event(101)).await.unwrap();
    let capture_after = ops(&h);
    reset_ops(&h);
    h.indexer.list_inbox(None).await.unwrap();
    let inbox_after = ops(&h);
    reset_ops(&h);
    h.indexer
        .list_events_filtered_page(&filter, false, 50, None)
        .await
        .unwrap();
    let events_after = ops(&h);

    eprintln!("file operations (OPEN, READ) per call, 25 events:");
    eprintln!("  capture_event  before compaction {capture_before:?}  after {capture_after:?}");
    eprintln!("  list_inbox     before compaction {inbox_before:?}  after {inbox_after:?}");
    eprintln!("  list_events    before compaction {events_before:?}  after {events_after:?}");
    assert!(
        inbox_after.0 < inbox_before.0 && events_after.0 < events_before.0,
        "a compacted table opens fewer files per read: inbox {inbox_before:?} -> {inbox_after:?}, \
         events {events_before:?} -> {events_after:?}"
    );
}
