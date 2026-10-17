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
/// The lab's incident: one runner's heartbeats, ~58k rows.
const RUNNER_BACKLOG: i64 = 58_000;

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

#[tokio::test]
async fn purging_a_label_on_postgres_runs_in_postgres_and_stays_in_its_tenant() {
    let (_pg, dsn) = live_postgres().await;
    let r = replica(&dsn).await;
    let probe = Probe::new(&dsn).await;
    probe.seed(2_000).await;
    // The same label under ANOTHER tenant that shares the physical table.
    probe
        .exec(&format!(
            "INSERT INTO escurel_events (tenant, event_id, at_ts, label_skill, status, kind, seq) \
             SELECT 'other', 'o' || g, now() - (g || ' seconds')::interval, '{LABEL}', 'inbox', 'system', g \
             FROM generate_series(1, 100) g"
        ))
        .await;
    r.indexer.list_inbox(Some(1)).await.unwrap();
    let spec = escurel_index::PurgeSpec::new(LABEL, None, Some(50), false).unwrap();

    let (dry, rows) = measured(&probe, || async {
        r.indexer.purge_events(&spec, false).await
    })
    .await;
    let dry = dry.expect("dry run");
    assert_eq!(
        (dry.total, dry.matched, dry.kept, dry.deleted),
        (2_000, 1_950, 50, 0)
    );
    assert_eq!(
        probe
            .scalar(&format!(
                "SELECT count(*)::bigint FROM escurel_events WHERE label_skill = '{LABEL}'"
            ))
            .await,
        2_100,
        "a dry run deletes nothing"
    );
    assert!(
        rows < 2_000,
        "the dry run's counts ran in Postgres, not through the scanner; read {rows}"
    );

    let done = r.indexer.purge_events(&spec, true).await.expect("apply");
    assert_eq!((done.matched, done.deleted), (1_950, 1_950));
    let mine = probe
        .scalar(&format!(
            "SELECT count(*)::bigint FROM escurel_events WHERE label_skill = '{LABEL}' AND tenant = '{TENANT}'"
        ))
        .await;
    let other = probe
        .scalar(&format!(
            "SELECT count(*)::bigint FROM escurel_events WHERE label_skill = '{LABEL}' AND tenant = 'other'"
        ))
        .await;
    assert_eq!(
        (mine, other),
        (50, 100),
        "only this tenant's rows, and the newest 50 survive"
    );
    let trail = probe
        .scalar(&format!(
            "SELECT count(*)::bigint FROM escurel_events WHERE label_skill = 'escurel:event-purge' AND tenant = '{TENANT}'"
        ))
        .await;
    assert_eq!(trail, 1, "the applied purge left one audit event");
}

/// `n` heartbeats of one runner, written the way the stream design did
/// (`provenance.runner_status.runner_id`), natively.
async fn seed_runner_heartbeats(probe: &Probe, runner: &str, n: i64, tenant: &str, base: i64) {
    probe
        .exec(&format!(
            "INSERT INTO escurel_events \
             (tenant, event_id, at_ts, source, mime, label_skill, status, title, body, provenance, kind, seq) \
             SELECT '{tenant}', '{runner}-hb' || g, now() - (g || ' seconds')::interval, 'escurel-runner', \
                    'application/json', '{LABEL}', 'inbox', 'heartbeat', repeat('x', 400), \
                    '{{\"runner_status\":{{\"runner_id\":\"{runner}\",\"tenant\":\"{tenant}\"}}}}', 'system', {base} + g \
             FROM generate_series(1, {n}) g"
        ))
        .await;
}

fn runner_prov(runner: &str) -> serde_json::Value {
    serde_json::json!({ "runner_status": { "runner_id": runner, "tenant": TENANT } })
}

#[tokio::test]
async fn a_runners_first_status_report_purges_its_backlog_in_postgres_then_stays_one_row() {
    let (_pg, dsn) = live_postgres().await;
    let r = replica(&dsn).await;
    let probe = Probe::new(&dsn).await;
    probe.seed(0).await;
    // The runner's own 58,000-row backlog; another runner's rows; the same
    // runner id under another tenant sharing the physical table.
    seed_runner_heartbeats(&probe, "r1", RUNNER_BACKLOG, TENANT, 10).await;
    seed_runner_heartbeats(&probe, "r2", BACKLOG, TENANT, 100_000).await;
    seed_runner_heartbeats(&probe, "r1", 30, "other", 0).await;
    probe.exec("ANALYZE escurel_events").await;
    r.indexer.list_inbox(Some(1)).await.unwrap();

    let count = |where_sql: String| {
        let probe = &probe;
        async move {
            probe
                .scalar(&format!(
                    "SELECT count(*)::bigint FROM escurel_events WHERE {where_sql}"
                ))
                .await
        }
    };

    let started = Instant::now();
    let first = r
        .indexer
        .upsert_runner_status(
            "r1",
            None,
            "started",
            "{\"state\":\"started\"}",
            &runner_prov("r1"),
        )
        .await
        .expect("first report");
    eprintln!(
        "first report (creates the record, purges a {RUNNER_BACKLOG}-row backlog): {:?}",
        started.elapsed()
    );
    assert!(first.created);
    assert_eq!(first.event.event_id, "runner-status:r1");
    assert_eq!(
        count(format!("tenant = '{TENANT}' AND label_skill = '{LABEL}'")).await,
        1 + BACKLOG,
        "r1's backlog is gone, its record and r2's rows stay"
    );
    assert_eq!(
        count("tenant = 'other' AND label_skill = 'escurel:runner-status'".to_owned()).await,
        30,
        "the same runner id under another tenant is untouched"
    );

    // Steady state: every further report is one UPDATE; the table does not grow
    // and Postgres reads a handful of rows for it. Two warm-up reports: the attached connection charges the backlog delete's
    // counters to the first statements that follow it.
    for _ in 0..2 {
        r.indexer
            .upsert_runner_status("r1", None, "heartbeat", "{}", &runner_prov("r1"))
            .await
            .expect("warm-up report");
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
    for i in 0..5 {
        let (r2, rows) = measured(&probe, || async {
            r.indexer
                .upsert_runner_status(
                    "r1",
                    None,
                    "heartbeat",
                    &format!("{{\"n\":{i}}}"),
                    &runner_prov("r1"),
                )
                .await
        })
        .await;
        let r2 = r2.expect("report");
        assert!(!r2.created);
        assert_eq!(r2.event.body, format!("{{\"n\":{i}}}"));
        assert!(
            rows < PUSHED_DOWN_CEILING,
            "report {i} read {rows} rows of the events table"
        );
    }
    assert_eq!(
        count(format!("tenant = '{TENANT}' AND label_skill = '{LABEL}'")).await,
        1 + BACKLOG,
        "the reports add no row"
    );

    // The newest-first label tail (what the workbench and the readiness probe read)
    // finds the record first.
    let tail = EventListFilter {
        label_skill: Some(LABEL.to_owned()),
        include_system: true,
        ..Default::default()
    };
    let newest = r
        .indexer
        .list_events_filtered_page(&tail, false, 1, None)
        .await
        .expect("newest");
    assert_eq!(newest.events[0].event_id, "runner-status:r1");
    assert_eq!(newest.events[0].title, "heartbeat");
}

/// One reserved-id status record, written natively `age_s` seconds ago.
async fn seed_status_record(
    probe: &Probe,
    tenant: &str,
    runner: &str,
    age_s: i64,
    interval_s: u64,
) {
    probe
        .exec(&format!(
            "INSERT INTO escurel_events \
             (tenant, event_id, at_ts, source, mime, label_skill, status, title, body, provenance, kind, seq) \
             VALUES ('{tenant}', 'runner-status:{runner}', now() - interval '{age_s} seconds', \
                     'escurel-runner', 'application/json', '{LABEL}', 'inbox', 'heartbeat', \
                     '{{\"status_interval_s\":{interval_s}}}', \
                     '{{\"runner_status\":{{\"runner_id\":\"{runner}\",\"tenant\":\"{tenant}\"}}}}', \
                     'system', (SELECT COALESCE(max(seq), 0) + 1 FROM escurel_events))"
        ))
        .await;
}

#[tokio::test]
async fn a_report_collects_a_stopped_runners_record_in_postgres_and_only_in_its_tenant() {
    let (_pg, dsn) = live_postgres().await;
    let r = replica(&dsn).await;
    let probe = Probe::new(&dsn).await;
    probe.seed(0).await;
    // Old pods of two redeploys (3 h and 2 h silent), a sibling that reported
    // 10 min ago, a slow runner (30 min interval) silent for 4 h (window 5 h),
    // and a stopped runner of ANOTHER tenant sharing the physical table.
    seed_status_record(&probe, TENANT, "pod-old-1", 3 * 3600, 300).await;
    seed_status_record(&probe, TENANT, "pod-old-2", 2 * 3600, 300).await;
    seed_status_record(&probe, TENANT, "sibling", 600, 300).await;
    seed_status_record(&probe, TENANT, "slow", 4 * 3600, 1800).await;
    seed_status_record(&probe, "other", "pod-old-1", 3 * 3600, 300).await;
    probe.exec("ANALYZE escurel_events").await;

    let ids = || {
        let probe = &probe;
        async move {
            let c = probe.client().await;
            let rows = c
                .query(
                    &format!(
                        "SELECT tenant || '/' || event_id FROM escurel_events \
                         WHERE label_skill = '{LABEL}' ORDER BY 1"
                    ),
                    &[],
                )
                .await
                .unwrap();
            rows.iter()
                .map(|r| r.get::<_, String>(0))
                .collect::<Vec<_>>()
        }
    };

    let (res, rows) = measured(&probe, || async {
        r.indexer
            .upsert_runner_status(
                "pod-new",
                None,
                "started",
                "{\"status_interval_s\":300}",
                &runner_prov("pod-new"),
            )
            .await
    })
    .await;
    res.expect("report");
    assert_eq!(
        ids().await,
        [
            "acme/runner-status:pod-new",
            "acme/runner-status:sibling",
            "acme/runner-status:slow",
            "other/runner-status:pod-old-1",
        ],
        "both stopped pods of this tenant are gone; the sibling, the slow runner inside \
         its own window and the other tenant's record stay"
    );
    assert!(
        rows < PUSHED_DOWN_CEILING,
        "the report plus the collection read {rows} rows of the events table"
    );
}
