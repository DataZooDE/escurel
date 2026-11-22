//! An event read must not hold the tenant's connection while it runs (#858).
//!
//! In-crate because the probe must take `Indexer::conn` itself (crate-private):
//! that lock is what every other tool queues on.
//!
//! In the lab the events table is a DuckLake table (catalog on Cloud SQL,
//! Parquet on S3), so one `list_events` / `list_inbox` takes ~1.4 s. The async
//! runner polls every 5 s, and while its read ran it held the one connection,
//! so `search`, `expand` and `query_instance` waited behind it. A large local
//! table stands in for the slow lake: the read takes long enough that a lock
//! held for its whole duration is unmistakable.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_storage::{FsStore, LaneStore};

use crate::{Indexer, Migrator};

const ROWS: i64 = 400_000;
/// Far below the read's own duration (hundreds of ms for `ROWS`), far above a
/// clone of the connection plus scheduling noise.
const MAX_WAIT: Duration = Duration::from_millis(150);

struct Fixture {
    idx: Arc<Indexer>,
    _dirs: Vec<tempfile::TempDir>,
}

async fn fixture() -> Fixture {
    let store_dir = tempfile::TempDir::new().unwrap();
    let db_dir = tempfile::TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = duckdb::Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let idx = Arc::new(Indexer::new(store, embedder, conn, "acme").unwrap());
    {
        let conn = idx.conn.lock().await;
        // Half in the inbox, half processed on one instance.
        conn.execute_batch(&format!(
            "INSERT INTO events (event_id, at_ts, source, mime, label_skill, instance_page_id, \
                                 status, title, body, kind, seq) \
             SELECT printf('ev-%08d', i), TIMESTAMP '2026-01-01' + INTERVAL (i) SECOND, 'probe', \
                    'text/plain', 'meeting', \
                    CASE WHEN i % 2 = 0 THEN 'markdown/instances/meeting/q3.md' END, \
                    CASE WHEN i % 2 = 0 THEN 'processed' ELSE 'inbox' END, \
                    'title ' || i, repeat('x', 200), 'user', i \
             FROM range({ROWS}) t(i)"
        ))
        .unwrap();
    }
    Fixture {
        idx,
        _dirs: vec![store_dir, db_dir],
    }
}

/// Take and release the connection in a loop while `stop` is false; return the
/// longest single wait for it.
fn probe(idx: &Arc<Indexer>, stop: &Arc<AtomicBool>) -> tokio::task::JoinHandle<Duration> {
    let (idx, stop) = (Arc::clone(idx), Arc::clone(stop));
    tokio::spawn(async move {
        let mut worst = Duration::ZERO;
        while !stop.load(Ordering::SeqCst) {
            let t = Instant::now();
            drop(idx.conn.lock().await);
            worst = worst.max(t.elapsed());
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        worst
    })
}

async fn worst_wait_during<F, T>(idx: &Arc<Indexer>, read: F) -> (Duration, Duration, T)
where
    F: std::future::Future<Output = T>,
{
    let stop = Arc::new(AtomicBool::new(false));
    let handle = probe(idx, &stop);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let t = Instant::now();
    let out = read.await;
    let took = t.elapsed();
    stop.store(true, Ordering::SeqCst);
    (handle.await.unwrap(), took, out)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reading_the_inbox_does_not_hold_the_connection() {
    let f = fixture().await;
    let (worst, took, inbox) = worst_wait_during(&f.idx, f.idx.list_inbox(None)).await;
    assert_eq!(inbox.unwrap().len() as i64, ROWS / 2);
    assert!(
        worst < MAX_WAIT,
        "another caller waited {worst:?} for the connection during a {took:?} inbox read"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reading_an_instances_events_does_not_hold_the_connection() {
    let f = fixture().await;
    let (worst, took, events) = worst_wait_during(
        &f.idx,
        f.idx.list_events("markdown/instances/meeting/q3.md", None),
    )
    .await;
    assert_eq!(events.unwrap().len() as i64, ROWS / 2);
    assert!(
        worst < MAX_WAIT,
        "another caller waited {worst:?} for the connection during a {took:?} event read"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn paging_events_does_not_hold_the_connection() {
    let f = fixture().await;
    let (worst, took, page) = worst_wait_during(
        &f.idx,
        f.idx
            .list_events_page("markdown/instances/meeting/q3.md", 10_000, None, false),
    )
    .await;
    // A page is capped at 10 000 rows, so this read is shorter than the
    // others: under the lock the probe waits about as long as the read took.
    assert_eq!(page.unwrap().events.len(), 10_000);
    assert!(
        worst < MAX_WAIT && worst * 3 < took,
        "another caller waited {worst:?} for the connection during a {took:?} paged read"
    );
}

/// What a caller just wrote is visible to its next read, although the read
/// runs on its own connection: the write committed before the lock was let go.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_read_sees_the_event_captured_just_before_it() {
    let f = fixture().await;
    let ev = f
        .idx
        .capture_event(crate::NewEvent {
            event_id: Some("fresh-1".to_owned()),
            at: Some("2030-01-01T00:00:00Z".to_owned()),
            source: "probe".to_owned(),
            mime: "text/plain".to_owned(),
            label_skill: "meeting".to_owned(),
            instance_page_id: None,
            title: "fresh".to_owned(),
            body: "fresh".to_owned(),
            provenance: None,
            kind: Default::default(),
            root_event_id: None,
            run_id: None,
        })
        .await
        .unwrap();
    let inbox = f.idx.list_inbox(Some(1)).await.unwrap();
    assert_eq!(inbox[0].event_id, ev.event_id);
}
