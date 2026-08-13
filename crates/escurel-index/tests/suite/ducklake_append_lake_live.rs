//! Chat + events on a LAKE-backed table (Parquet on the object store)
//! rather than the catalog's Postgres, against real containers.
//!
//! What these pin, beyond "it works": the properties the Postgres variant
//! got from the schema and the lake cannot — cross-replica read-your-writes
//! with a READ_ONLY corpus attach alongside, `capture_event` idempotency
//! without a PRIMARY KEY, and that compaction actually collapses the
//! one-file-per-append growth.
//!
//! `cargo test -p escurel-index --features live-ducklake --test ducklake_append_lake_live`

#![cfg(feature = "live-ducklake")]

use std::sync::Arc;

use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::snapshot::{
    LakeConfig, ObjectStoreSecret, gc_lake_snapshots, install_load_sql, secret_sql,
};
use escurel_index::{AppendChatMessage, Indexer, ListChatMessages, Migrator, NewEvent, OrderDir};
use escurel_storage::{FsStore, LaneStore, S3Store, S3StoreConfig};
use tempfile::TempDir;
use testcontainers_modules::minio::MinIO;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ContainerAsync;
use testcontainers_modules::testcontainers::runners::AsyncRunner;

const TENANT: &str = "acme";
const BUCKET: &str = "escurel-append";
const GROUP: &str = "grp-1";

struct Live {
    cfg: LakeConfig,
    _pg: ContainerAsync<Postgres>,
    _minio: ContainerAsync<MinIO>,
    _dirs: Vec<TempDir>,
}

async fn live() -> Live {
    let pg = Postgres::default().start().await.expect("start postgres");
    let pg_port = pg.get_host_port_ipv4(5432).await.expect("pg port");
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

    Live {
        cfg: LakeConfig {
            catalog_dsn: format!(
                "host=127.0.0.1 port={pg_port} user=postgres password=postgres dbname=postgres"
            ),
            data_path: format!("s3://{BUCKET}/data/"),
            object_store: ObjectStoreSecret::S3 {
                endpoint: format!("127.0.0.1:{s3_port}"),
                access_key_id: "minioadmin".to_owned(),
                secret_access_key: "minioadmin".to_owned(),
                region: "us-east-1".to_owned(),
                use_ssl: false,
            },
        },
        _pg: pg,
        _minio: minio,
        _dirs: Vec::new(),
    }
}

/// An independent "replica": its own store dir, its own DuckDB file, its
/// own connection — exactly what a second server process would have.
fn replica(live: &mut Live) -> Arc<Indexer> {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    live._dirs.push(store_dir);
    live._dirs.push(db_dir);
    Arc::new(Indexer::new(store, embedder, conn, TENANT).unwrap())
}

fn msg(content: &str) -> AppendChatMessage<'_> {
    AppendChatMessage {
        chat_group_id: GROUP,
        role: "user",
        content,
        author: None,
        ts: None,
        metadata: None,
        msg_id: None,
        embed: false,
    }
}

fn list() -> ListChatMessages<'static> {
    ListChatMessages {
        chat_group_id: GROUP,
        since: None,
        until: None,
        limit: 100,
        cursor: None,
        direction: OrderDir::Asc,
    }
}

/// Count Parquet objects on the data path directly. `ducklake_table_info`
/// returns one row per TABLE, not per file, and would report 1 no matter
/// how many exist — the trap that made the first compaction spike read as
/// a working no-change.
fn parquet_files(cfg: &LakeConfig) -> i64 {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(&install_load_sql(cfg)).unwrap();
    if let Some(sql) = secret_sql(cfg).unwrap() {
        conn.execute_batch(&sql).unwrap();
    }
    conn.query_row(
        &format!("SELECT count(*) FROM glob('s3://{BUCKET}/data/**/*.parquet')"),
        [],
        |r| r.get::<_, i64>(0),
    )
    .unwrap_or(-1)
}

#[tokio::test]
async fn chat_on_the_lake_round_trips_across_replicas() {
    let mut live = live().await;
    let a = replica(&mut live);
    let b = replica(&mut live);
    a.attach_chat_lake(&live.cfg).await.unwrap();
    b.attach_chat_lake(&live.cfg).await.unwrap();

    a.append_chat_message(msg("from a")).await.unwrap();
    b.append_chat_message(msg("from b")).await.unwrap();

    // The property the Postgres variant is asserted on: a SEPARATE replica
    // sees the row immediately, with no publish/adopt cycle in between.
    let seen = b.list_chat_messages(list()).await.unwrap();
    let contents: Vec<&str> = seen.messages.iter().map(|m| m.content.as_str()).collect();
    assert!(
        contents.contains(&"from a") && contents.contains(&"from b"),
        "both replicas' appends must be visible to either; got {contents:?}",
    );

    // Ordering is (ts, msg_id) with ULID tiebreakers — unchanged by the
    // backend swap.
    let ts: Vec<&str> = seen.messages.iter().map(|m| m.ts.as_str()).collect();
    let mut sorted = ts.clone();
    sorted.sort_unstable();
    assert_eq!(ts, sorted, "history must come back time-ordered");
}

#[tokio::test]
async fn capture_event_stays_idempotent_without_a_primary_key() {
    let mut live = live().await;
    let a = replica(&mut live);
    a.attach_events_lake(&live.cfg).await.unwrap();

    let ev = NewEvent {
        event_id: Some("01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned()),
        source: "gmail".to_owned(),
        mime: "text/plain".to_owned(),
        label_skill: "email".to_owned(),
        title: "first".to_owned(),
        body: "one".to_owned(),
        ..Default::default()
    };
    let first = a.capture_event(ev.clone()).await.unwrap();
    let second = a
        .capture_event(NewEvent {
            title: "second".to_owned(),
            ..ev
        })
        .await
        .unwrap();

    // DuckLake enforces no PRIMARY KEY, so this is carried by the INSERT's
    // anti-join instead. First-writer-wins must still hold.
    assert_eq!(first.event_id, second.event_id);
    assert_eq!(
        second.title, "first",
        "a re-capture must return the FIRST stored event, not the second input",
    );
    let inbox = a.list_inbox(None).await.unwrap();
    assert_eq!(inbox.len(), 1, "the duplicate must not create a second row");
}

#[tokio::test]
async fn compaction_collapses_one_file_per_append() {
    let mut live = live().await;
    let a = replica(&mut live);
    a.attach_chat_lake(&live.cfg).await.unwrap();

    const N: usize = 40;
    for i in 0..N {
        a.append_chat_message(msg(&format!("m{i}"))).await.unwrap();
    }

    let before = parquet_files(&live.cfg);
    assert!(
        before >= i64::try_from(N).unwrap(),
        "each append should have written its own Parquet file, got {before} for {N} appends",
    );

    // Compaction alone does NOT free anything: `CREATE OR REPLACE` writes
    // the consolidated file, but the superseded ones stay referenced by
    // older snapshots. Expiry + cleanup is what actually removes them —
    // which is why the publish task runs the pair, and why this test does
    // too rather than asserting on compaction in isolation.
    a.compact_append_lake().await.unwrap();
    let after_compact = parquet_files(&live.cfg);
    gc_lake_snapshots(&a, &live.cfg, 1).await.unwrap();
    let after = parquet_files(&live.cfg);
    println!("files: {before} -> {after_compact} (compact) -> {after} (gc)");

    let msgs = a.list_chat_messages(list()).await.unwrap();
    assert_eq!(msgs.messages.len(), N, "compaction must preserve every row");
    assert!(
        after < before,
        "compaction + GC must reduce the Parquet file count \
         ({before} -> {after_compact} after compact -> {after} after gc)",
    );
}
