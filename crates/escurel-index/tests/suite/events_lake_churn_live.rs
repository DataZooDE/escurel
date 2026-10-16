//! Runner heartbeats on the LAKE-backed events table (`ESCUREL_EVENTS_BACKEND=
//! ducklake`, the lab's dz-escurel): every `capture_event` writes a Parquet
//! file and a snapshot, and the runner-status prune that follows it deletes a
//! row and writes another snapshot. DuckLake reads `ducklake_file_column_stats`
//! on every scan, so what the periodic compaction + GC leaves behind decides
//! what every later read costs (the lab saw ~106 full scans a minute of a
//! table that was ~93 % rows of files long retired).
//!
//! Pins that the pair the publish task runs (`compact_append_lake` +
//! `gc_lake_snapshots`) leaves the catalog bounded: snapshots at the retention
//! count, no statistics rows of files that no longer exist.
//!
//! Opt-in: `live-ducklake` feature (Docker). Run with
//! `cargo test -p escurel-index --features live-ducklake --test suite events_lake_churn_live`.

#![cfg(feature = "live-ducklake")]

use std::sync::Arc;

use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::snapshot::{LakeConfig, ObjectStoreSecret, gc_lake_snapshots};
use escurel_index::{EventKind, Indexer, Migrator, NewEvent};
use escurel_storage::testing::{self, S3Server};
use escurel_storage::{FsStore, LaneStore};
use tempfile::TempDir;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::AsyncRunner;

const TENANT: &str = "acme";
const BUCKET: &str = "escurel-churn";
const LABEL: &str = "escurel:runner-status";
const KEEP_ROWS: usize = 50;
const RETAIN_SNAPSHOTS: u32 = 10;

async fn catalog_count(client: &tokio_postgres::Client, sql: &str) -> i64 {
    client.query_one(sql, &[]).await.unwrap().get::<_, i64>(0)
}

/// Everything a lake-backed events test keeps alive.
struct Env {
    ix: Indexer,
    cfg: LakeConfig,
    client: tokio_postgres::Client,
    _pg: testcontainers_modules::testcontainers::ContainerAsync<Postgres>,
    _s3_server: S3Server,
    _dirs: (TempDir, TempDir),
}

async fn lake_env() -> Env {
    let pg = Postgres::default().start().await.expect("start postgres");
    let pg_port = pg.get_host_port_ipv4(5432).await.expect("pg port");
    let s3_server = S3Server::start().await;
    let _s3 = s3_server.store(BUCKET, "unused").await;
    let dsn =
        format!("host=127.0.0.1 port={pg_port} user=postgres password=postgres dbname=postgres");
    let cfg = LakeConfig {
        catalog_dsn: dsn.clone(),
        data_path: format!("s3://{BUCKET}/data/"),
        object_store: ObjectStoreSecret::S3 {
            endpoint: s3_server.endpoint_host_port(),
            access_key_id: testing::ACCESS_KEY.to_owned(),
            secret_access_key: testing::SECRET_KEY.to_owned(),
            region: "us-east-1".to_owned(),
            use_ssl: false,
        },
    };

    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let ix = Indexer::new(store, embedder, conn, TENANT).unwrap();
    ix.attach_events_lake(&cfg)
        .await
        .expect("attach events lake");

    let (client, c) = tokio_postgres::connect(&dsn, tokio_postgres::NoTls)
        .await
        .expect("catalog connect");
    tokio::spawn(async move {
        let _ = c.await;
    });
    Env {
        ix,
        cfg,
        client,
        _pg: pg,
        _s3_server: s3_server,
        _dirs: (store_dir, db_dir),
    }
}

#[tokio::test]
async fn heartbeat_churn_leaves_a_bounded_catalog_after_compaction_and_gc() {
    let env = lake_env().await;
    let (ix, cfg, client) = (&env.ix, &env.cfg, &env.client);

    // 120 heartbeats, each followed by the gateway's prune to the newest 50.
    for i in 0..120 {
        ix.capture_event(NewEvent {
            source: "escurel-runner".to_owned(),
            mime: "application/json".to_owned(),
            label_skill: LABEL.to_owned(),
            title: "heartbeat".to_owned(),
            body: format!("{{\"n\":{i}}}"),
            kind: EventKind::System,
            ..Default::default()
        })
        .await
        .expect("capture");
        ix.prune_label_events(LABEL, KEEP_ROWS)
            .await
            .expect("prune");
    }
    let rows = ix
        .list_events_filtered_page(
            &escurel_index::EventListFilter {
                label_skill: Some(LABEL.to_owned()),
                include_system: true,
                ..Default::default()
            },
            true,
            1000,
            None,
        )
        .await
        .expect("list")
        .events
        .len();
    assert_eq!(rows, KEEP_ROWS, "the prune keeps the newest {KEEP_ROWS}");

    let snapshots_before = catalog_count(client, "SELECT count(*) FROM ducklake_snapshot").await;
    assert!(
        snapshots_before > 150,
        "120 captures + ~70 prunes are ~190 snapshots; got {snapshots_before}"
    );

    // What the periodic publish task runs after every cycle.
    ix.compact_append_lake().await.expect("compact");
    gc_lake_snapshots(ix, cfg, RETAIN_SNAPSHOTS)
        .await
        .expect("gc");

    let snapshots = catalog_count(client, "SELECT count(*) FROM ducklake_snapshot").await;
    let files = catalog_count(client, "SELECT count(*) FROM ducklake_data_file").await;
    let stats = catalog_count(client, "SELECT count(*) FROM ducklake_file_column_stats").await;
    let orphan_stats = catalog_count(
        client,
        "SELECT count(*) FROM ducklake_file_column_stats s \
         WHERE NOT EXISTS (SELECT 1 FROM ducklake_data_file f WHERE f.data_file_id = s.data_file_id)",
    )
    .await;
    eprintln!(
        "snapshots {snapshots_before} -> {snapshots}; data files {files}; column-stats rows {stats} (orphans {orphan_stats})"
    );
    assert!(
        snapshots <= i64::from(RETAIN_SNAPSHOTS) + 2,
        "snapshots stay at the retention count; got {snapshots}"
    );
    assert_eq!(
        orphan_stats, 0,
        "no column-statistics rows of files that no longer exist"
    );
    assert!(
        stats <= files * 40,
        "statistics rows are bounded by (live files x columns); {stats} rows for {files} files"
    );
}

#[tokio::test]
async fn status_record_overwrites_leave_one_row_and_a_bounded_catalog() {
    let env = lake_env().await;
    let (ix, cfg, client) = (&env.ix, &env.cfg, &env.client);
    let prov = serde_json::json!({ "runner_status": { "runner_id": "r1", "tenant": TENANT } });

    // The legacy stream is on the lake too: 15 heartbeats of the runner.
    for i in 0..15 {
        ix.capture_event(NewEvent {
            source: "escurel-runner".to_owned(),
            mime: "application/json".to_owned(),
            label_skill: LABEL.to_owned(),
            title: "heartbeat".to_owned(),
            body: format!("{{\"old\":{i}}}"),
            provenance: Some(prov.clone()),
            kind: EventKind::System,
            ..Default::default()
        })
        .await
        .expect("capture");
    }
    // 120 overwrites, no prune needed.
    for i in 0..120 {
        let r = ix
            .upsert_runner_status("r1", None, "heartbeat", &format!("{{\"n\":{i}}}"), &prov)
            .await
            .expect("upsert");
        assert_eq!(r.created, i == 0, "report {i}");
    }
    let rows = ix
        .list_events_filtered_page(
            &escurel_index::EventListFilter {
                label_skill: Some(LABEL.to_owned()),
                include_system: true,
                ..Default::default()
            },
            false,
            1000,
            None,
        )
        .await
        .expect("list")
        .events;
    assert_eq!(
        rows.len(),
        1,
        "one record, the legacy rows are gone: {rows:?}"
    );
    assert_eq!(rows[0].event_id, "runner-status:r1");
    assert_eq!(rows[0].body, "{\"n\":119}");

    let snapshots_before = catalog_count(client, "SELECT count(*) FROM ducklake_snapshot").await;
    ix.compact_append_lake().await.expect("compact");
    gc_lake_snapshots(ix, cfg, RETAIN_SNAPSHOTS)
        .await
        .expect("gc");
    let snapshots = catalog_count(client, "SELECT count(*) FROM ducklake_snapshot").await;
    let files = catalog_count(client, "SELECT count(*) FROM ducklake_data_file").await;
    let stats = catalog_count(client, "SELECT count(*) FROM ducklake_file_column_stats").await;
    let orphan_stats = catalog_count(
        client,
        "SELECT count(*) FROM ducklake_file_column_stats s \
         WHERE NOT EXISTS (SELECT 1 FROM ducklake_data_file f WHERE f.data_file_id = s.data_file_id)",
    )
    .await;
    eprintln!(
        "snapshots {snapshots_before} -> {snapshots}; data files {files}; column-stats rows {stats} (orphans {orphan_stats})"
    );
    assert!(
        snapshots <= i64::from(RETAIN_SNAPSHOTS) + 2,
        "snapshots stay at the retention count; got {snapshots}"
    );
    assert_eq!(orphan_stats, 0);
    assert!(stats <= files * 40, "{stats} stat rows for {files} files");
    let after = ix
        .get_event("runner-status:r1")
        .await
        .expect("get")
        .expect("record survives compaction + gc");
    assert_eq!(after.body, "{\"n\":119}");
}
