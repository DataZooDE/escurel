//! Live DuckLake publish round-trip (DuckLake PR 3): a REAL Postgres
//! catalog (testcontainer) + a REAL MinIO `s3://` DATA_PATH
//! (testcontainer), no mocks. This is the production shape ADR-0009
//! locks (Postgres-catalog DuckLake, object-store Parquet).
//!
//! Opt-in: gated behind the `live-ducklake` feature (needs Docker),
//! mirroring `live-postgres` / the escurel-storage `s3` MinIO tests. Run
//! with `cargo test -p escurel-index --features live-ducklake --test
//! ducklake_publish_live`.

#![cfg(feature = "live-ducklake")]

use std::sync::Arc;

use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::snapshot::{
    LakeConfig, ObjectStoreSecret, attach_sql, install_load_sql, publish_lake, secret_sql,
};
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, LaneStore, S3Store, S3StoreConfig};
use tempfile::TempDir;
use testcontainers_modules::minio::MinIO;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ContainerAsync;
use testcontainers_modules::testcontainers::runners::AsyncRunner;

const TENANT: &str = "acme";
const BUCKET: &str = "escurel-lake";

const CUSTOMER_SKILL: &str = "\
---
type: skill
id: customer
description: a customer
---
# customer
";

const ACME_INSTANCE: &str = "\
---
type: instance
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
    _minio: ContainerAsync<MinIO>,
    _store_dir: TempDir,
    _db_dir: TempDir,
}

/// Boot Postgres (catalog) + MinIO (data path), create the bucket with
/// the REAL S3Store, and build a seeded-ready Indexer.
async fn live_lake() -> LiveLake {
    let pg = Postgres::default().start().await.expect("start postgres");
    let pg_port = pg.get_host_port_ipv4(5432).await.expect("pg port");
    let dsn =
        format!("host=127.0.0.1 port={pg_port} user=postgres password=postgres dbname=postgres");

    let minio = MinIO::default().start().await.expect("start minio");
    let s3_port = minio.get_host_port_ipv4(9000).await.expect("minio port");
    let s3 = S3Store::new(S3StoreConfig {
        bucket: BUCKET.to_owned(),
        prefix: "unused".to_owned(),
        endpoint_url: format!("http://127.0.0.1:{s3_port}"),
        region: "us-east-1".to_owned(),
        access_key_id: "minioadmin".to_owned(),
        secret_access_key: "minioadmin".to_owned(),
    })
    .await
    .expect("build S3Store");
    s3.ensure_bucket().await.expect("create bucket");

    let cfg = LakeConfig {
        catalog_dsn: dsn,
        data_path: format!("s3://{BUCKET}/data/"),
        object_store: ObjectStoreSecret::S3 {
            endpoint: format!("127.0.0.1:{s3_port}"),
            access_key_id: "minioadmin".to_owned(),
            secret_access_key: "minioadmin".to_owned(),
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
        _minio: minio,
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
async fn publish_creates_lake_tables_and_manifest_pg_minio() {
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
        .expect("publish to PG catalog + MinIO");
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

    // MinIO now physically holds Parquet objects under the data prefix
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
        "publish must write Parquet data files to MinIO"
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
