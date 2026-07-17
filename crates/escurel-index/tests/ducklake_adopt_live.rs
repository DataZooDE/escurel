//! Live DuckLake adopt round-trip (DuckLake PR 4): a REAL Postgres
//! catalog (testcontainer) + a REAL MinIO `s3://` DATA_PATH
//! (testcontainer), no mocks — the production shape ADR-0009 locks.
//! Publish from a writer indexer, poll, adopt into a fresh in-memory
//! reader, and query it over the real wire.
//!
//! Opt-in: gated behind the `live-ducklake` feature (needs Docker),
//! like `ducklake_publish_live.rs`. Run with `cargo test -p
//! escurel-index --features live-ducklake --test ducklake_adopt_live`.

#![cfg(feature = "live-ducklake")]

use std::sync::Arc;

use duckdb::Connection;
use escurel_embed::{Embedder, HashEmbedder};
use escurel_index::snapshot::{
    LakeConfig, ObjectStoreSecret, adopt_lake, latest_lake_snapshot_id, publish_lake,
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

const CUSTOMER_SKILL: (&str, &str) = (
    "markdown/skills/customer.md",
    "---\n\
     type: skill\n\
     id: customer\n\
     description: a customer\n\
     ---\n\
     # customer\n",
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

/// All-German-stopword body — FTS drops every token, so only the dense
/// arm can rank this page (see `ducklake_adopt.rs` for the rationale).
const STOPWORT: (&str, &str) = (
    "markdown/instances/customer/stopwort.md",
    "---\n\
     type: instance\n\
     skill: customer\n\
     id: stopwort\n\
     ---\n\
     # Der Die Das\n\
     \n\
     und oder aber als also bei von der die das\n",
);

struct LiveLake {
    store: Arc<dyn LaneStore>,
    indexer: Arc<Indexer>,
    cfg: LakeConfig,
    _pg: ContainerAsync<Postgres>,
    _minio: ContainerAsync<MinIO>,
    _store_dir: TempDir,
    _db_dir: TempDir,
}

/// Boot Postgres (catalog) + MinIO (data path), create the bucket with
/// the REAL S3Store, and build a seeded-ready writer Indexer.
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
    let embedder: Arc<dyn Embedder> = Arc::new(HashEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Arc::new(Indexer::new(Arc::clone(&store), embedder, conn, TENANT).unwrap());
    LiveLake {
        store,
        indexer,
        cfg,
        _pg: pg,
        _minio: minio,
        _store_dir: store_dir,
        _db_dir: db_dir,
    }
}

#[tokio::test]
async fn adopt_builds_queryable_indexer_pg_minio() {
    let lake = live_lake().await;
    for (path, body) in [CUSTOMER_SKILL, ACME, STOPWORT] {
        lake.indexer.update_page(path, body).await.unwrap();
    }

    let report = publish_lake(&lake.indexer, &lake.cfg, None)
        .await
        .expect("publish to PG catalog + MinIO");
    assert!(!report.skipped);
    assert_eq!(report.pages, 3);

    // The change poll (fresh scout connection each time) sees it.
    let latest = latest_lake_snapshot_id(&lake.cfg).await.expect("poll");
    assert_eq!(latest, Some(report.snapshot_id));

    // Full adopt over the real wire: PG catalog + Parquet from MinIO.
    let reader_embedder: Arc<dyn Embedder> = Arc::new(HashEmbedder::default());
    let adopted = adopt_lake(
        &lake.cfg,
        Arc::clone(&lake.store),
        reader_embedder,
        TENANT,
        None,
    )
    .await
    .expect("adopt")
    .expect("first adopt returns an indexer");
    assert_eq!(adopted.snapshot_id, report.snapshot_id);
    let reader = &adopted.indexer;

    // Vector arm (exact-body all-stopword query — FTS returns nothing).
    let stopwort_body = lake
        .indexer
        .expand(STOPWORT.0, None, None)
        .await
        .unwrap()
        .expect("stopwort page on the writer")
        .blocks[0]
        .content
        .clone();
    let hits = reader
        .search(&stopwort_body, 3, None, None, None, None)
        .await
        .unwrap();
    assert_eq!(
        hits.first().map(|x| x.page_id.as_str()),
        Some(STOPWORT.0),
        "the all-stopword query must hit via the dense arm: {:?}",
        hits.iter().map(|x| x.page_id.clone()).collect::<Vec<_>>()
    );

    // FTS arm + resolve.
    let hits = reader
        .search("manufacturing", 4, None, None, None, None)
        .await
        .unwrap();
    assert!(hits.iter().any(|x| x.page_id == ACME.0));
    assert!(
        reader
            .resolve("[[customer::acme-corp]]", None)
            .await
            .unwrap()
            .exists()
    );

    // Unchanged snapshot → no re-adopt.
    let noop = adopt_lake(
        &lake.cfg,
        Arc::clone(&lake.store),
        Arc::new(HashEmbedder::default()),
        TENANT,
        Some(adopted.snapshot_id),
    )
    .await
    .expect("noop adopt");
    assert!(noop.is_none());
}
