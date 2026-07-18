//! Offline DuckLake publish round-trip (DuckLake PR 3) — no Docker.
//!
//! Uses the DuckDB-file catalog form (`ATTACH 'ducklake:<path>'`, spike
//! note 2026-07-17) with a local-directory DATA_PATH, so the whole
//! lake lives under tempdirs. Real DuckDB, real ducklake extension,
//! real Parquet files on disk — no mocks. The live Postgres-catalog +
//! MinIO leg is `ducklake_publish.rs`'s sibling
//! (`ducklake_publish_live.rs`, feature `live-ducklake`).

use std::sync::Arc;

use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::snapshot::{
    LakeConfig, ObjectStoreSecret, attach_sql, gc_lake_snapshots, install_load_sql, publish_lake,
    secret_sql,
};
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, LaneStore};
use tempfile::TempDir;

const TENANT: &str = "acme";

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

Enterprise tier, manufacturing.
";

struct Harness {
    indexer: Arc<Indexer>,
    _store_dir: TempDir,
    _db_dir: TempDir,
    /// Holds the lake: `catalog.ducklake` (DuckDB-file catalog) +
    /// `data/` (local DATA_PATH).
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
    let indexer = Arc::new(Indexer::new(store, embedder, conn, TENANT).unwrap());
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

/// Fresh, second DuckDB connection attaching the lake READ_ONLY —
/// proving the published state is readable without the writer.
fn reader_conn(cfg: &LakeConfig) -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(&install_load_sql(cfg)).unwrap();
    if let Some(sql) = secret_sql(cfg).unwrap() {
        conn.execute_batch(&sql).unwrap();
    }
    conn.execute_batch(&attach_sql(cfg, true).unwrap()).unwrap();
    conn
}

fn lake_snapshot_count(cfg: &LakeConfig) -> i64 {
    let conn = reader_conn(cfg);
    conn.query_row("SELECT count(*) FROM ducklake_snapshots('lake')", [], |r| {
        r.get(0)
    })
    .unwrap()
}

#[tokio::test]
async fn ducklake_file_catalog_local_data_round_trip() {
    let h = fresh_harness();
    let cfg = lake_config(&h);

    h.indexer
        .update_page("markdown/skills/customer.md", CUSTOMER_SKILL)
        .await
        .unwrap();
    h.indexer
        .update_page("markdown/instances/customer/acme-corp.md", ACME_INSTANCE)
        .await
        .unwrap();
    assert!(
        h.indexer.mutation_epoch() >= 2,
        "two update_page calls must bump the dirty counter twice"
    );

    let report = publish_lake(&h.indexer, &cfg, None).await.expect("publish");
    assert!(!report.skipped);
    assert_eq!(report.pages, 2);
    assert_eq!(report.blocks, 2);
    assert_eq!(report.epoch, h.indexer.mutation_epoch());
    assert!(report.snapshot_id >= 1, "got {}", report.snapshot_id);

    // A SECOND, fresh connection reads the lake READ_ONLY.
    let reader = reader_conn(&cfg);
    let pages: i64 = reader
        .query_row("SELECT count(*) FROM lake.pages", [], |r| r.get(0))
        .unwrap();
    assert_eq!(pages, 2);
    let blocks: i64 = reader
        .query_row("SELECT count(*) FROM lake.blocks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(blocks, 2);

    // The FLOAT[768] → FLOAT[] cast round-trips: list type, 768 elements.
    let (ty, len): (String, i64) = reader
        .query_row(
            "SELECT typeof(dense_vec), len(dense_vec) FROM lake.blocks LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        ty, "FLOAT[]",
        "lake must store the list type, not FLOAT[768]"
    );
    assert_eq!(len, 768, "all 768 elements survive the Parquet round-trip");

    // Single-row manifest with the embedding-space + schema pins.
    let (schema_version, model_id, dim, escurel_version, m_pages, m_blocks, epoch): (
        i64,
        String,
        i64,
        String,
        i64,
        i64,
        i64,
    ) = reader
        .query_row(
            "SELECT schema_version, model_id, dim, escurel_version, \
                    pages, blocks, published_epoch \
             FROM lake.escurel_manifest",
            [],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(schema_version, i64::from(Migrator::SCHEMA_VERSION));
    assert_eq!(model_id, "zero");
    assert_eq!(dim, 768);
    assert_eq!(escurel_version, env!("CARGO_PKG_VERSION"));
    assert_eq!(m_pages, 2);
    assert_eq!(m_blocks, 2);
    assert_eq!(epoch as u64, report.epoch);

    // The manifest is single-row (an upsert, not an append).
    let manifest_rows: i64 = reader
        .query_row("SELECT count(*) FROM lake.escurel_manifest", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(manifest_rows, 1);

    // The credential registry NEVER reaches the lake.
    let cred_tables: i64 = reader
        .query_row(
            "SELECT count(*) FROM information_schema.tables \
             WHERE table_catalog = 'lake' AND table_name = 'external_credentials'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        cred_tables, 0,
        "external_credentials must never be published"
    );
}

#[tokio::test]
async fn publish_skips_when_clean() {
    let h = fresh_harness();
    let cfg = lake_config(&h);
    h.indexer
        .update_page("markdown/skills/customer.md", CUSTOMER_SKILL)
        .await
        .unwrap();

    let first = publish_lake(&h.indexer, &cfg, None).await.expect("publish");
    assert!(!first.skipped);
    let snapshots_after_first = lake_snapshot_count(&cfg);

    // Nothing changed since `first.epoch` → the second publish is a no-op.
    let second = publish_lake(&h.indexer, &cfg, Some(first.epoch))
        .await
        .expect("clean publish");
    assert!(second.skipped, "clean publish must be skipped: {second:?}");
    assert_eq!(second.epoch, first.epoch);
    assert_eq!(
        lake_snapshot_count(&cfg),
        snapshots_after_first,
        "a skipped publish must not create a DuckLake snapshot"
    );

    // A new write dirties the epoch → the next publish runs again.
    h.indexer
        .update_page("markdown/instances/customer/acme-corp.md", ACME_INSTANCE)
        .await
        .unwrap();
    let third = publish_lake(&h.indexer, &cfg, Some(first.epoch))
        .await
        .expect("dirty publish");
    assert!(!third.skipped);
    assert_eq!(third.pages, 2);
    assert!(
        lake_snapshot_count(&cfg) > snapshots_after_first,
        "a dirty publish must create a new snapshot"
    );
}

/// `gc_lake_snapshots` prunes down to (at most) `keep` snapshots and
/// never touches the current one — publish 5 distinct snapshots (the
/// initial ATTACH/CREATE snapshots plus one per `update_page`+publish),
/// then GC with `keep = 2` and assert the count settles to exactly the
/// retention target (DuckLake never expires the newest snapshot, so
/// this is the exact, not just upper-bound, count — verified
/// interactively first, see
/// docs/notes/discovered/2026-07-18-ducklake-snapshot-gc.md).
#[tokio::test]
async fn gc_prunes_to_the_keep_count() {
    let h = fresh_harness();
    let cfg = lake_config(&h);

    let mut last_epoch = None;
    for i in 0..5 {
        h.indexer
            .update_page(
                &format!("markdown/instances/customer/c{i}.md"),
                ACME_INSTANCE,
            )
            .await
            .unwrap();
        let report = publish_lake(&h.indexer, &cfg, last_epoch)
            .await
            .expect("publish");
        last_epoch = Some(report.epoch);
    }
    let before = lake_snapshot_count(&cfg);
    assert!(
        before > 2,
        "need more than `keep` snapshots to prove GC ran: {before}"
    );

    let pruned = gc_lake_snapshots(&h.indexer, &cfg, 2).await.expect("gc");
    assert!(pruned > 0, "gc must report at least one pruned snapshot");
    assert_eq!(
        lake_snapshot_count(&cfg),
        2,
        "gc must settle the snapshot count at exactly `keep`"
    );
}

/// `keep = 0` disables GC — a no-op, not a "prune everything" footgun.
#[tokio::test]
async fn gc_keep_zero_disables_gc() {
    let h = fresh_harness();
    let cfg = lake_config(&h);
    h.indexer
        .update_page("markdown/skills/customer.md", CUSTOMER_SKILL)
        .await
        .unwrap();
    publish_lake(&h.indexer, &cfg, None).await.expect("publish");
    let before = lake_snapshot_count(&cfg);

    let pruned = gc_lake_snapshots(&h.indexer, &cfg, 0)
        .await
        .expect("gc no-op");
    assert_eq!(pruned, 0);
    assert_eq!(lake_snapshot_count(&cfg), before);
}

/// Fewer published snapshots than `keep` — `ducklake_expire_snapshots`
/// with a `NULL` `older_than` crashes the extension (discovered note),
/// so `gc_lake_snapshots` must short-circuit before ever calling it.
#[tokio::test]
async fn gc_noop_when_fewer_snapshots_than_keep() {
    let h = fresh_harness();
    let cfg = lake_config(&h);
    h.indexer
        .update_page("markdown/skills/customer.md", CUSTOMER_SKILL)
        .await
        .unwrap();
    publish_lake(&h.indexer, &cfg, None).await.expect("publish");
    let before = lake_snapshot_count(&cfg);

    let pruned = gc_lake_snapshots(&h.indexer, &cfg, 1000)
        .await
        .expect("gc must not crash when keep exceeds the snapshot count");
    assert_eq!(pruned, 0);
    assert_eq!(lake_snapshot_count(&cfg), before);
}
