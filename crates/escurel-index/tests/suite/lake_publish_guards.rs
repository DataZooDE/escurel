//! A lake must stay consistent with the lane across `migrate_kind`, and a partial index must never
//! overwrite it. Real DuckDB + real DuckLake (DuckDB-file catalog, local DATA_PATH), no mocks.
//!
//! The lab-cutover rehearsal of 2026-10-14 found three ways to lose the lake's truth:
//!
//! 1. `migrate-kind --apply` left the lake stale (`type:` pages); the next writer boot adopted that
//!    stale corpus over the freshly migrated lane and served `type:` frontmatter silently;
//! 2. a `rebuild` whose client gave up left the served index partial, and a `publish_snapshot` then
//!    published 91 of ~183 pages over the populated lake;
//! 3. nothing refused a corpus far smaller than the lake it overwrites (only an EMPTY one).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use duckdb::Connection;
use escurel_embed::{Embedder, HashEmbedder};
use escurel_index::snapshot::{
    LakeConfig, ObjectStoreSecret, PublishOptions, SnapshotError, adopt_lake_for_writer,
    attach_lake, publish_lake, publish_lake_with,
};
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, Key, LaneStore, Result as StoreResult, Version};
use tempfile::TempDir;
use url::Url;

const TENANT: &str = "acme";

fn skill_page() -> (&'static str, String) {
    (
        "markdown/skills/customer.md",
        "---\nkind: skill\nid: customer\ndescription: a customer\n---\n# customer\n\nA customer.\n"
            .to_owned(),
    )
}

fn instance_page(i: usize) -> (String, String) {
    (
        format!("markdown/instances/customer/c{i:02}.md"),
        format!(
            "---\nkind: instance\nskill: customer\nid: c{i:02}\n---\n# C{i}\n\nCustomer number {i}.\n"
        ),
    )
}

/// A lane store that delegates to the real filesystem store but pauses every read, so a test can
/// act while a rebuild is half-way (latency only: every byte is real).
struct SlowReads {
    inner: FsStore,
    delay: Duration,
}

#[async_trait]
impl LaneStore for SlowReads {
    async fn read(&self, key: &Key) -> StoreResult<Bytes> {
        tokio::time::sleep(self.delay).await;
        self.inner.read(key).await
    }
    async fn write(&self, key: &Key, body: Bytes) -> StoreResult<Version> {
        self.inner.write(key, body).await
    }
    async fn list(&self, prefix: &Key) -> StoreResult<Vec<Key>> {
        self.inner.list(prefix).await
    }
    async fn delete(&self, key: &Key) -> StoreResult<()> {
        self.inner.delete(key).await
    }
    fn url(&self, key: &Key) -> StoreResult<Url> {
        self.inner.url(key)
    }
}

struct Env {
    store: Arc<dyn LaneStore>,
    indexer: Arc<Indexer>,
    cfg: LakeConfig,
    _dirs: Vec<TempDir>,
}

fn indexer_over(store: Arc<dyn LaneStore>, db_dir: &TempDir) -> Arc<Indexer> {
    let embedder: Arc<dyn Embedder> = Arc::new(HashEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    Arc::new(Indexer::new(store, embedder, conn, TENANT).unwrap())
}

async fn env_with(instances: usize, delay: Option<Duration>) -> Env {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let lake_dir = TempDir::new().unwrap();
    std::fs::create_dir_all(lake_dir.path().join("data")).unwrap();
    let fs = FsStore::new(store_dir.path().to_path_buf());
    let store: Arc<dyn LaneStore> = match delay {
        Some(delay) => Arc::new(SlowReads { inner: fs, delay }),
        None => Arc::new(fs),
    };
    let indexer = indexer_over(Arc::clone(&store), &db_dir);
    let (sp, sb) = skill_page();
    indexer.update_page(sp, &sb).await.unwrap();
    for i in 0..instances {
        let (p, b) = instance_page(i);
        indexer.update_page(&p, &b).await.unwrap();
    }
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
    Env {
        store,
        indexer,
        cfg,
        _dirs: vec![store_dir, db_dir, lake_dir],
    }
}

fn lake_pages(cfg: &LakeConfig) -> i64 {
    let conn = Connection::open_in_memory().unwrap();
    attach_lake(&conn, cfg, true).unwrap();
    conn.query_row("SELECT count(*) FROM lake.pages", [], |r| r.get(0))
        .unwrap()
}

fn lake_legacy_pages(cfg: &LakeConfig) -> i64 {
    let conn = Connection::open_in_memory().unwrap();
    attach_lake(&conn, cfg, true).unwrap();
    conn.query_row(
        "SELECT count(*) FROM lake.pages WHERE json_extract(frontmatter, '$.type') IS NOT NULL",
        [],
        |r| r.get(0),
    )
    .unwrap()
}

#[tokio::test]
async fn a_corpus_far_smaller_than_the_lake_is_refused_unless_the_shrink_is_explicit() {
    let e = env_with(14, None).await; // 15 pages
    publish_lake(&e.indexer, &e.cfg, None)
        .await
        .expect("first publish");
    assert_eq!(lake_pages(&e.cfg), 15);

    // A second writer over the same lake whose index only holds 4 pages (a partial rebuild).
    let db2 = TempDir::new().unwrap();
    let partial = indexer_over(Arc::clone(&e.store), &db2);
    let (sp, sb) = skill_page();
    partial.update_page(sp, &sb).await.unwrap();
    for i in 0..3 {
        let (p, b) = instance_page(i);
        partial.update_page(&p, &b).await.unwrap();
    }

    let err = publish_lake(&partial, &e.cfg, None)
        .await
        .expect_err("a 4-page corpus must not overwrite a 15-page lake");
    assert!(
        matches!(
            err,
            SnapshotError::RefusedPartialPublish {
                local_pages: 4,
                lake_pages: 15
            }
        ),
        "got: {err}"
    );
    assert_eq!(
        lake_pages(&e.cfg),
        15,
        "a refused publish must leave the lake untouched"
    );

    publish_lake_with(
        &partial,
        &e.cfg,
        None,
        PublishOptions { allow_shrink: true },
    )
    .await
    .expect("a deliberate shrink goes through");
    assert_eq!(lake_pages(&e.cfg), 4);
}

#[tokio::test]
async fn a_rebuild_that_did_not_finish_blocks_publishing_until_a_rebuild_completes() {
    let e = env_with(14, Some(Duration::from_millis(100))).await;
    publish_lake(&e.indexer, &e.cfg, None)
        .await
        .expect("first publish");

    // Start a rebuild and abandon it half-way (what a client that gave up used to do to the server).
    let ix = Arc::clone(&e.indexer);
    let run = tokio::spawn(async move { ix.rebuild().await });
    // Wait until the rebuild has truncated the index (it flags itself partial from there on), then
    // abandon it a page or two into the re-index.
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while !e.indexer.rebuild_incomplete() {
        assert!(
            std::time::Instant::now() < deadline,
            "the rebuild never reached its truncate"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(250)).await;
    run.abort();
    let _ = run.await;
    assert!(
        e.indexer.rebuild_incomplete(),
        "an aborted rebuild leaves the index flagged partial"
    );

    let err = publish_lake(&e.indexer, &e.cfg, None)
        .await
        .expect_err("a partial index must not be published");
    assert!(
        matches!(err, SnapshotError::RefusedIncompleteIndex),
        "got: {err}"
    );
    assert_eq!(lake_pages(&e.cfg), 15, "the lake keeps its real corpus");

    e.indexer.rebuild().await.expect("a completed rebuild");
    assert!(!e.indexer.rebuild_incomplete());
    publish_lake(&e.indexer, &e.cfg, None)
        .await
        .expect("publishes again");
}

/// The rehearsal sequence: the lane was migrated (`kind:`) but the lake still holds `type:` pages from
/// an older writer. Booting a writer over it adopts the stale corpus; that must quarantine the tenant
/// (never serve it silently), and `migrate_kind --apply` must lift it by re-deriving the index from the
/// lane, after which a publish leaves the lake with no legacy page.
#[tokio::test]
async fn a_stale_lake_adopted_over_a_migrated_lane_quarantines_and_apply_makes_it_consistent() {
    let e = env_with(5, None).await; // lane + index hold `kind:` pages
    publish_lake(&e.indexer, &e.cfg, None)
        .await
        .expect("publish");
    // Make the LAKE stale: the shape an older writer published.
    {
        let conn = Connection::open_in_memory().unwrap();
        attach_lake(&conn, &e.cfg, false).unwrap();
        conn.execute_batch(
            "UPDATE lake.pages SET frontmatter = json_object('type', json_extract_string(frontmatter, '$.kind'), \
             'id', json_extract_string(frontmatter, '$.id')) WHERE json_extract(frontmatter, '$.kind') IS NOT NULL;",
        )
        .unwrap();
    }
    assert_eq!(
        lake_legacy_pages(&e.cfg),
        6,
        "precondition: the lake is stale"
    );

    // A fresh writer over the SAME lane boots and adopts the lake.
    let db2 = TempDir::new().unwrap();
    let writer = indexer_over(Arc::clone(&e.store), &db2);
    writer.attach_lake(&e.cfg).await.unwrap();
    let embedder = HashEmbedder::default();
    adopt_lake_for_writer(&writer, &embedder)
        .await
        .expect("adopt");
    assert!(
        writer.quarantine_stale_adopted_lake().await.unwrap(),
        "an adopted lake whose pages carry the removed `type:` key must quarantine the tenant"
    );
    assert!(writer.legacy_quarantine().is_some());
    // A quarantined tenant never publishes (and so never spreads the stale corpus).
    assert!(publish_lake(&writer, &e.cfg, None).await.unwrap().skipped);

    // The apply re-derives the index from the (clean) lane and lifts the quarantine ...
    let report = writer.migrate_kind(true).await.expect("apply");
    assert!(!report.tenant_quarantined);
    // ... and a publish now leaves the lake consistent.
    let published = publish_lake(&writer, &e.cfg, None).await.expect("publish");
    assert!(!published.skipped);
    assert_eq!(
        lake_legacy_pages(&e.cfg),
        0,
        "no `type:` page may remain in the lake"
    );
    assert_eq!(lake_pages(&e.cfg), 6);
}

/// Damage the lake CATALOG (not the data): the DuckDB-file catalog still opens and the lake still
/// attaches, but the rows of its tables cannot be listed (a catalog database restored without one of its
/// metadata tables). This is the failure shape of a catalog that answers the attach and then errors on a read.
fn damage_lake_catalog(cfg: &LakeConfig) {
    let conn = Connection::open(&cfg.catalog_dsn).unwrap();
    conn.execute_batch("ALTER TABLE ducklake_data_file RENAME TO ducklake_data_file_gone;")
        .unwrap();
}

#[tokio::test]
async fn a_publish_whose_lake_page_count_cannot_be_read_is_refused_not_treated_as_an_empty_lake() {
    let e = env_with(14, None).await; // 15 pages
    publish_lake(&e.indexer, &e.cfg, None)
        .await
        .expect("first publish");
    damage_lake_catalog(&e.cfg);

    // A partial index (4 of 15 pages) is exactly what the partial-publish guard exists to stop; with the
    // lake unreadable the guard has to fail CLOSED, not read the failure as "0 lake pages".
    let db2 = TempDir::new().unwrap();
    let partial = indexer_over(Arc::clone(&e.store), &db2);
    let (sp, sb) = skill_page();
    partial.update_page(sp, &sb).await.unwrap();
    for i in 0..3 {
        let (p, b) = instance_page(i);
        partial.update_page(&p, &b).await.unwrap();
    }
    let err = publish_lake(&partial, &e.cfg, None)
        .await
        .expect_err("an unreadable lake must refuse the publish, not overwrite it");
    assert!(
        matches!(err, SnapshotError::LakePagesUnreadable { .. }),
        "the guard must refuse BEFORE the overwrite, naming that the lake could not be read: {err}"
    );
}

#[tokio::test]
async fn an_empty_corpus_over_a_lake_that_has_no_pages_table_yet_still_publishes() {
    // A brand-new lake has no `pages` table: that is "empty", not "unreadable".
    let e = env_with(0, None).await;
    publish_lake(&e.indexer, &e.cfg, None)
        .await
        .expect("the first publish into a fresh lake");
    assert_eq!(lake_pages(&e.cfg), 1);
}
