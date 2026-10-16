//! The events table on the attached-Postgres backend must not be read in
//! full by the hot paths (a lab incident: heron's `escurel_events` grew to
//! ~60k never-pruned runner heartbeats and the runner's poll, which lists
//! the inbox and tails two labels, pulled the whole table through the
//! DuckDB postgres scanner on every call; ~120 GiB/day of Cloud SQL egress).
//!
//! These tests measure what Postgres itself reports as rows returned
//! (`pg_stat_user_tables`: rows read by scans and index fetches) around each call, against a REAL
//! Postgres, with a heartbeat backlog the size of the incident's order of
//! magnitude. The scanner reads the backlog several times per call; a call
//! that is pushed down to Postgres touches a handful of rows.
//!
//! Opt-in: behind the `live-ducklake` feature (needs Docker). Run with
//! `cargo test -p escurel-index --features live-ducklake --test suite events_pg_cost_live`.

#![cfg(feature = "live-ducklake")]

use std::sync::Arc;
use std::time::{Duration, Instant};

use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{EventKind, EventListFilter, Indexer, Migrator, NewEvent};
use escurel_storage::{FsStore, LaneStore};
use tempfile::TempDir;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ContainerAsync;
use testcontainers_modules::testcontainers::runners::AsyncRunner;

const TENANT: &str = "acme";
const LABEL: &str = "escurel:runner-status";

struct Replica {
    indexer: Indexer,
    _store_dir: TempDir,
    _db_dir: TempDir,
}

async fn replica(dsn: &str) -> Replica {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Indexer::new(store, embedder, conn, TENANT).unwrap();
    indexer
        .attach_events_pg(dsn)
        .await
        .expect("attach events pg");
    Replica {
        indexer,
        _store_dir: store_dir,
        _db_dir: db_dir,
    }
}

async fn live_postgres() -> (ContainerAsync<Postgres>, String) {
    let pg = Postgres::default().start().await.expect("start postgres");
    let port = pg.get_host_port_ipv4(5432).await.expect("pg port");
    let dsn = format!("host=127.0.0.1 port={port} user=postgres password=postgres dbname=postgres");
    (pg, dsn)
}

/// A plain Postgres client onto the same server: used to seed a backlog
/// natively and to read the server's own row counters. (Not DuckDB's
/// scanner: its catalog probing would itself add rows to every reading.)
///
/// Every operation uses a FRESH connection that is dropped afterwards:
/// Postgres flushes a backend's statistics lazily (a long-lived idle
/// connection can hold seeding counters back by seconds and release them
/// into a later measurement); a closing backend flushes everything.
struct Probe {
    dsn: String,
}

impl Probe {
    async fn new(dsn: &str) -> Self {
        Self {
            dsn: dsn.to_owned(),
        }
    }

    async fn client(&self) -> tokio_postgres::Client {
        let (client, conn) = tokio_postgres::connect(&self.dsn, tokio_postgres::NoTls)
            .await
            .expect("probe connect");
        tokio::spawn(async move {
            let _ = conn.await;
        });
        client
    }

    async fn exec(&self, pg_sql: &str) {
        self.client().await.batch_execute(pg_sql).await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    async fn scalar(&self, pg_sql: &str) -> i64 {
        let c = self.client().await;
        let v = c.query_one(pg_sql, &[]).await.unwrap().get::<_, i64>(0);
        drop(c);
        v
    }

    /// Rows read from the events table by scans and index fetches
    /// (`pg_stat_user_tables`: `seq_tup_read + idx_tup_fetch`), once the
    /// server's lazily flushed statistics have settled (two equal reads,
    /// 1.5 s apart). Reading this counter touches no events row, so the
    /// probe does not disturb what it measures; and it is exactly the
    /// quantity the wire cost follows from.
    async fn rows_read(&self) -> i64 {
        let sql = "SELECT COALESCE(seq_tup_read + idx_tup_fetch, 0)::bigint \
                   FROM pg_stat_user_tables WHERE relname = 'escurel_events'";
        let mut last = self.scalar(sql).await;
        for _ in 0..12 {
            tokio::time::sleep(Duration::from_millis(1500)).await;
            let now = self.scalar(sql).await;
            if now == last {
                return now;
            }
            last = now;
        }
        last
    }

    /// `n` runner-status heartbeats (system events, unassigned, so they sit
    /// in the inbox hidden) and a few real user events, inserted natively.
    async fn seed(&self, heartbeats: i64) {
        self.exec(&format!(
            "INSERT INTO escurel_events \
             (tenant, event_id, at_ts, source, mime, label_skill, status, title, body, kind, seq) \
             SELECT '{TENANT}', 'hb' || g, now() - (g || ' seconds')::interval, 'escurel-runner', \
                    'application/json', '{LABEL}', 'inbox', 'heartbeat', repeat('x', 400), 'system', g \
             FROM generate_series(1, {heartbeats}) g"
        ))
        .await;
        self.exec(&format!(
            "INSERT INTO escurel_events \
             (tenant, event_id, at_ts, source, mime, label_skill, status, title, body, kind, seq) \
             SELECT '{TENANT}', 'user' || g, now() - (g || ' minutes')::interval, 'gmail', \
                    'text/plain', 'email', 'inbox', 'mail ' || g, 'body', 'user', 1000000 + g \
             FROM generate_series(1, 3) g"
        ))
        .await;
        self.exec("ANALYZE escurel_events").await;
    }
}

/// Run `f`, return what it returned and how many events-table rows
/// Postgres read for it.
async fn measured<T, Fut: std::future::Future<Output = T>>(
    probe: &Probe,
    f: impl FnOnce() -> Fut,
) -> (T, i64) {
    let before = probe.rows_read().await;
    let out = f().await;
    let rows = probe.rows_read().await - before;
    eprintln!("measured call read {rows} rows of the events table");
    (out, rows)
}

/// Generous ceiling for a call that is pushed down: a partial-index scan
/// of a handful of rows plus the scanner's catalog probes. The unpushed
/// path is two orders of magnitude above it with the backlog below.
const PUSHED_DOWN_CEILING: i64 = 500;
const BACKLOG: i64 = 5_000;

#[tokio::test]
async fn the_inbox_does_not_read_the_heartbeat_backlog() {
    let (_pg, dsn) = live_postgres().await;
    let r = replica(&dsn).await;
    let probe = Probe::new(&dsn).await;
    probe.seed(BACKLOG).await;
    // A warm-up call, so catalog probing of the first use is not counted.
    r.indexer.list_inbox(Some(1)).await.unwrap();

    let (inbox, rows) = measured(&probe, || async { r.indexer.list_inbox(Some(5)).await }).await;
    let inbox = inbox.expect("list_inbox");
    let titles: Vec<_> = inbox.iter().map(|e| e.title.as_str()).collect();
    assert_eq!(
        titles,
        vec!["mail 1", "mail 2", "mail 3"],
        "newest first, bookkeeping hidden"
    );
    assert!(
        rows < PUSHED_DOWN_CEILING,
        "list_inbox read {rows} rows from Postgres with {BACKLOG} hidden heartbeats in the inbox"
    );
}

#[tokio::test]
async fn paged_inbox_and_label_tails_do_not_read_the_whole_backlog() {
    let (_pg, dsn) = live_postgres().await;
    let r = replica(&dsn).await;
    let probe = Probe::new(&dsn).await;
    probe.seed(BACKLOG).await;
    r.indexer.list_inbox(Some(1)).await.unwrap();

    // Paged inbox, two pages over the three user events, with a cursor.
    let (first, rows) = measured(&probe, || async {
        r.indexer.list_inbox_page(2, None, false).await
    })
    .await;
    let first = first.expect("page 1");
    assert_eq!(first.events.len(), 2);
    assert!(first.next_cursor.is_some());
    assert!(
        rows < PUSHED_DOWN_CEILING,
        "paged inbox read {rows} rows with {BACKLOG} hidden heartbeats"
    );
    let second = r
        .indexer
        .list_inbox_page(2, first.next_cursor.as_deref(), false)
        .await
        .expect("page 2");
    assert_eq!(second.events.len(), 1);
    assert_eq!(second.events[0].title, "mail 3");
    assert!(second.next_cursor.is_none());

    // Including bookkeeping still works (and is the caller's explicit ask).
    let with_system = r
        .indexer
        .list_inbox_page(10, None, true)
        .await
        .expect("with system");
    assert_eq!(with_system.events.len(), 10);

    // The newest runner-status heartbeat: a label tail, newest first, one row.
    let filter = EventListFilter {
        label_skill: Some(LABEL.to_owned()),
        kind: Some(EventKind::System),
        status: Some("inbox".to_owned()),
        ..Default::default()
    };
    let (newest, rows) = measured(&probe, || async {
        r.indexer
            .list_events_filtered_page(&filter, false, 1, None)
            .await
    })
    .await;
    let newest = newest.expect("newest heartbeat");
    assert_eq!(newest.events.len(), 1);
    assert_eq!(newest.events[0].event_id, format!("hb{BACKLOG}"));
    assert!(
        rows < PUSHED_DOWN_CEILING,
        "newest-heartbeat tail read {rows} rows of a {BACKLOG}-row label"
    );

    // Resuming a tail past a cursor pages forward in ingestion order.
    let tail = EventListFilter {
        label_skill: Some(LABEL.to_owned()),
        include_system: true,
        ..Default::default()
    };
    let p1 = r
        .indexer
        .list_events_filtered_page(&tail, true, 3, None)
        .await
        .expect("tail 1");
    let ids: Vec<_> = p1.events.iter().map(|e| e.event_id.as_str()).collect();
    assert_eq!(ids, vec!["hb1", "hb2", "hb3"]);
    let p2 = r
        .indexer
        .list_events_filtered_page(&tail, true, 3, p1.next_cursor.as_deref())
        .await
        .expect("tail 2");
    let ids: Vec<_> = p2.events.iter().map(|e| e.event_id.as_str()).collect();
    assert_eq!(ids, vec!["hb4", "hb5", "hb6"]);
}

#[tokio::test]
async fn capturing_an_event_does_not_scan_the_tenants_sequence_column() {
    let (_pg, dsn) = live_postgres().await;
    let r = replica(&dsn).await;
    let probe = Probe::new(&dsn).await;
    probe.seed(BACKLOG).await;
    r.indexer.list_inbox(Some(1)).await.unwrap();

    let (stored, rows) = measured(&probe, || async {
        r.indexer
            .capture_event(NewEvent {
                source: "gmail".to_owned(),
                mime: "text/plain".to_owned(),
                label_skill: "email".to_owned(),
                title: "fresh".to_owned(),
                body: "b".to_owned(),
                ..Default::default()
            })
            .await
    })
    .await;
    let stored = stored.expect("capture");
    assert_eq!(stored.title, "fresh");
    assert!(
        rows < PUSHED_DOWN_CEILING,
        "capture_event read {rows} rows to find the next sequence number"
    );
    let seq = probe
        .scalar(&format!(
            "SELECT seq::bigint FROM escurel_events WHERE tenant = '{TENANT}' AND title = 'fresh'"
        ))
        .await;
    assert_eq!(
        seq,
        1_000_003 + 1,
        "the next position after the highest seq"
    );
}

#[tokio::test]
async fn pruning_a_big_runner_status_backlog_runs_in_postgres() {
    let (_pg, dsn) = live_postgres().await;
    let r = replica(&dsn).await;
    let probe = Probe::new(&dsn).await;
    probe.seed(2_000).await;
    r.indexer.list_inbox(Some(1)).await.unwrap();

    let (res, rows) = measured(&probe, || async {
        let started = Instant::now();
        let res = r.indexer.prune_label_events(LABEL, 50).await;
        eprintln!("prune call alone: {:?}", started.elapsed());
        res
    })
    .await;
    let took = "see above";
    res.expect("prune");
    eprintln!("prune of a 2000-row backlog ({took}): postgres returned {rows} rows");

    let left = probe.scalar(&format!(
        "SELECT count(*)::bigint FROM escurel_events WHERE tenant = '{TENANT}' AND label_skill = '{LABEL}'"
    )).await;
    assert_eq!(left, 50, "exactly the newest 50 heartbeats are kept");
    let newest = probe.scalar(&format!(
        "SELECT count(*)::bigint FROM escurel_events WHERE tenant = '{TENANT}' AND event_id = 'hb1'"
    )).await;
    assert_eq!(
        newest, 1,
        "the newest (smallest age) heartbeat is among them"
    );
    let user = probe.scalar(&format!(
        "SELECT count(*)::bigint FROM escurel_events WHERE tenant = '{TENANT}' AND label_skill = 'email'"
    )).await;
    assert_eq!(user, 3, "other labels are untouched");
    assert!(
        rows < PUSHED_DOWN_CEILING,
        "the prune pulled {rows} rows through the scanner for a 2000-row backlog"
    );
}
