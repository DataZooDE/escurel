//! Live DuckLake publish round-trip (DuckLake PR 3): a REAL Postgres
//! catalog (testcontainer) + a REAL the S3 test server `s3://` DATA_PATH
//! (testcontainer), no mocks. This is the production shape ADR-0009
//! locks (Postgres-catalog DuckLake, object-store Parquet).
//!
//! Opt-in: gated behind the `live-ducklake` feature (needs Docker),
//! mirroring `live-postgres` / the escurel-storage `s3` the S3 test server tests. Run
//! with `cargo test -p escurel-index --features live-ducklake --test
//! ducklake_publish_live`.

#![cfg(feature = "live-ducklake")]

use std::sync::Arc;

use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::backend::{SqlConnector, SqlViewBackend, SqlViewBinding};
use escurel_index::snapshot::{
    LakeConfig, ObjectStoreSecret, attach_sql, install_load_sql, publish_lake, secret_sql,
};
use escurel_index::{Indexer, Migrator};
use escurel_storage::testing::{self, S3Server};
use escurel_storage::{FsStore, LaneStore};
use tempfile::TempDir;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ContainerAsync;
use testcontainers_modules::testcontainers::runners::AsyncRunner;

const TENANT: &str = "acme";
const BUCKET: &str = "escurel-lake";

const CUSTOMER_SKILL: &str = "\
---
kind: skill
id: customer
description: a customer
---
# customer
";

const ACME_INSTANCE: &str = "\
---
kind: instance
skill: customer
id: acme-corp
---
# Acme Corp

Enterprise tier.
";

struct LiveLake {
    indexer: Arc<Indexer>,
    cfg: LakeConfig,
    _pg: ContainerAsync<Postgres>,
    _s3: S3Server,
    _store_dir: TempDir,
    _db_dir: TempDir,
}

/// Boot Postgres (catalog) + the S3 test server (data path), create the bucket with
/// the REAL S3Store, and build a seeded-ready Indexer.
async fn live_lake() -> LiveLake {
    let pg = Postgres::default().start().await.expect("start postgres");
    let pg_port = pg.get_host_port_ipv4(5432).await.expect("pg port");
    let dsn =
        format!("host=127.0.0.1 port={pg_port} user=postgres password=postgres dbname=postgres");

    let s3_server = S3Server::start().await;
    let _s3 = s3_server.store(BUCKET, "unused").await;

    let cfg = LakeConfig {
        catalog_dsn: dsn,
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
    let indexer = Arc::new(Indexer::new(store, embedder, conn, TENANT).unwrap());
    LiveLake {
        indexer,
        cfg,
        _pg: pg,
        _s3: s3_server,
        _store_dir: store_dir,
        _db_dir: db_dir,
    }
}

/// Fresh, second connection attaching the live lake READ_ONLY.
fn reader_conn(cfg: &LakeConfig) -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(&install_load_sql(cfg)).unwrap();
    if let Some(sql) = secret_sql(cfg).unwrap() {
        conn.execute_batch(&sql).unwrap();
    }
    conn.execute_batch(&attach_sql(cfg, true).unwrap()).unwrap();
    conn
}

#[tokio::test]
async fn parquet_directory_reads_s3_with_the_lake_secret() {
    let lake = live_lake().await;
    // Write a real object through httpfs. The directory connector uses the same
    // S3 secret installed when the indexer attaches the lake.
    let writer = Connection::open_in_memory().unwrap();
    writer.execute_batch(&install_load_sql(&lake.cfg)).unwrap();
    writer
        .execute_batch(&secret_sql(&lake.cfg).unwrap().unwrap())
        .unwrap();
    writer
        .execute_batch(&format!(
            "COPY (SELECT 'Acme' AS name) TO 's3://{BUCKET}/demo/rows.parquet' (FORMAT parquet)"
        ))
        .unwrap();
    lake.indexer.attach_lake(&lake.cfg).await.unwrap();
    let binding = SqlViewBinding {
        connector: SqlConnector::ParquetDir,
        attach: None,
        relation: format!("s3://{BUCKET}/demo/"),
        filter: None,
        project: Default::default(),
        search_text: Vec::new(),
    };
    let backend = SqlViewBackend::new(Arc::clone(&lake.indexer));
    let materialized = backend
        .create_instance("customer", &binding, "remote", "# Remote")
        .await
        .unwrap();
    let rows = backend.project_rows(&materialized.view, 10).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["name"], "Acme");
}

#[tokio::test]
async fn publish_creates_lake_tables_and_manifest_pg_s3() {
    let lake = live_lake().await;
    lake.indexer
        .update_page("markdown/skills/customer.md", CUSTOMER_SKILL)
        .await
        .unwrap();
    lake.indexer
        .update_page("markdown/instances/customer/acme-corp.md", ACME_INSTANCE)
        .await
        .unwrap();

    let report = publish_lake(&lake.indexer, &lake.cfg, None)
        .await
        .expect("publish to PG catalog + the S3 test server");
    assert!(!report.skipped);
    assert_eq!(report.pages, 2);
    assert_eq!(report.blocks, 2);

    // A fresh READ_ONLY reader sees the published content.
    let reader = reader_conn(&lake.cfg);
    let pages: i64 = reader
        .query_row("SELECT count(*) FROM lake.pages", [], |r| r.get(0))
        .unwrap();
    assert_eq!(pages, 2);
    let (ty, len): (String, i64) = reader
        .query_row(
            "SELECT typeof(dense_vec), len(dense_vec) FROM lake.blocks LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(ty, "FLOAT[]");
    assert_eq!(len, 768);
    let (schema_version, model_id, epoch): (i64, String, i64) = reader
        .query_row(
            "SELECT schema_version, model_id, published_epoch FROM lake.escurel_manifest",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(schema_version, i64::from(Migrator::SCHEMA_VERSION));
    assert_eq!(model_id, "zero");
    assert_eq!(epoch as u64, report.epoch);

    // The credential registry never reached the lake.
    let cred_tables: i64 = reader
        .query_row(
            "SELECT count(*) FROM information_schema.tables \
             WHERE table_catalog = 'lake' AND table_name = 'external_credentials'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(cred_tables, 0);

    // the S3 test server now physically holds Parquet objects under the data prefix
    // (probed through httpfs on the same reader — real object listing).
    let parquet_objects: i64 = reader
        .query_row(
            &format!("SELECT count(*) FROM glob('s3://{BUCKET}/data/**/*.parquet')"),
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        parquet_objects > 0,
        "publish must write Parquet data files to the S3 test server"
    );
}

#[tokio::test]
async fn publish_is_one_snapshot_atomic() {
    let lake = live_lake().await;
    lake.indexer
        .update_page("markdown/skills/customer.md", CUSTOMER_SKILL)
        .await
        .unwrap();
    let first = publish_lake(&lake.indexer, &lake.cfg, None)
        .await
        .expect("first publish");

    // Dirty the index, publish again: the whole multi-table copy commits
    // as EXACTLY ONE DuckLake snapshot (readers see all-or-nothing).
    lake.indexer
        .update_page("markdown/instances/customer/acme-corp.md", ACME_INSTANCE)
        .await
        .unwrap();
    let second = publish_lake(&lake.indexer, &lake.cfg, Some(first.epoch))
        .await
        .expect("second publish");
    assert!(!second.skipped);
    assert_eq!(
        second.snapshot_id,
        first.snapshot_id + 1,
        "one publish transaction must advance the snapshot id by exactly 1"
    );

    // The catalog agrees (fresh READ_ONLY connection, live Postgres).
    let reader = reader_conn(&lake.cfg);
    let max_snapshot: i64 = reader
        .query_row(
            "SELECT max(snapshot_id) FROM ducklake_snapshots('lake')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(max_snapshot, second.snapshot_id);
}
