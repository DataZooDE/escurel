//! Offline DuckLake adopt round-trip (DuckLake PR 4) — no Docker.
//!
//! The reader half of the lake: `latest_lake_snapshot_id` (the change
//! poll) + `adopt_lake` (fail-closed compat check, bulk-load into a
//! fresh in-memory DB, HNSW + FTS rebuild). Uses the DuckDB-file
//! catalog + local-directory DATA_PATH like `ducklake_publish.rs`, so
//! the whole lake lives under tempdirs. Real DuckDB, real ducklake
//! extension, real Parquet — no mocks. The live Postgres + MinIO leg is
//! `ducklake_adopt_live.rs` (feature `live-ducklake`).
//!
//! Embedder: `HashEmbedder` (SHA-256 of the whole text → vector), so
//! "same text ⇒ same vector" holds exactly. The vector-arm assertion
//! seeds a page whose body is entirely German FTS stopwords: querying
//! that exact body produces zero FTS matches (every query token is a
//! stopword), so the hit can ONLY come from the dense arm — which
//! finds it at cosine distance 0 iff the vectors survived the
//! FLOAT[] → FLOAT[768] lake round-trip intact.

use std::sync::Arc;

use duckdb::Connection;
use escurel_embed::{Embedder, HashEmbedder};
use escurel_index::pack::PackSubscription;
use escurel_index::snapshot::{
    LakeConfig, ObjectStoreSecret, SnapshotError, adopt_lake, attach_lake, latest_lake_snapshot_id,
    publish_lake,
};
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, LaneStore};
use tempfile::TempDir;

const TENANT: &str = "acme";

const CUSTOMER_SKILL: (&str, &str) = (
    "markdown/skills/customer.md",
    "---\n\
     type: skill\n\
     id: customer\n\
     description: a customer\n\
     ---\n\
     # customer\n\
     \n\
     A customer is the unit of revenue.\n",
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

/// Body made ENTIRELY of German FTS stopwords (see `GERMAN_STOPWORDS`
/// in `search.rs`): der/die/das/und/oder/aber/als/also/bei/von. FTS
/// drops every token, so only the vector arm can rank this page.
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

struct Harness {
    store: Arc<dyn LaneStore>,
    indexer: Arc<Indexer>,
    _store_dir: TempDir,
    _db_dir: TempDir,
    lake_dir: TempDir,
}

fn fresh_harness() -> Harness {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let lake_dir = TempDir::new().unwrap();
    std::fs::create_dir_all(lake_dir.path().join("data")).unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(HashEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Arc::new(Indexer::new(Arc::clone(&store), embedder, conn, TENANT).unwrap());
    Harness {
        store,
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

fn reader_embedder() -> Arc<dyn Embedder> {
    Arc::new(HashEmbedder::default())
}

async fn seed(h: &Harness, pages: &[(&str, &'static str)]) {
    for (path, body) in pages {
        h.indexer.update_page(path, body).await.unwrap();
    }
}

#[tokio::test]
async fn adopt_builds_queryable_indexer_offline() {
    let h = fresh_harness();
    let cfg = lake_config(&h);
    seed(&h, &[CUSTOMER_SKILL, ACME, STOPWORT]).await;

    // Registry rows ride along with the corpus.
    h.indexer
        .add_group_member("team-acme", "alice@example.com", Some("admin"))
        .await
        .unwrap();
    h.indexer
        .record_pack_subscription(&PackSubscription {
            pack_id: "crm-core".to_owned(),
            version: 3,
            vertical: "crm".to_owned(),
            publisher: "datazoo".to_owned(),
            content_hash: "abc123".to_owned(),
            signature: String::new(),
        })
        .await
        .unwrap();

    let report = publish_lake(&h.indexer, &cfg, None).await.expect("publish");
    assert!(!report.skipped);

    // The change poll sees the published snapshot.
    let latest = latest_lake_snapshot_id(&cfg).await.expect("poll");
    assert_eq!(latest, Some(report.snapshot_id));

    // Adopt into a fresh in-memory indexer.
    let adopted = adopt_lake(&cfg, Arc::clone(&h.store), reader_embedder(), TENANT, None)
        .await
        .expect("adopt")
        .expect("a first adopt must return an indexer");
    assert_eq!(adopted.snapshot_id, report.snapshot_id);
    let reader = &adopted.indexer;

    // --- Vector arm: query with the stopword page's EXACT block body.
    // Every query token is a German FTS stopword → the FTS arm returns
    // nothing; only the dense arm can produce this hit, and it ranks
    // first iff the adopted vector equals HashEmbedder(body) — i.e. the
    // FLOAT[] → FLOAT[768] round-trip preserved the embedding.
    let stopwort_body = h
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

    // --- FTS arm: proves refresh_fts ran over the bulk-loaded blocks.
    let hits = reader
        .search("manufacturing", 4, None, None, None, None)
        .await
        .unwrap();
    assert!(
        hits.iter().any(|x| x.page_id == ACME.0),
        "FTS must rank the manufacturing page: {:?}",
        hits.iter().map(|x| x.page_id.clone()).collect::<Vec<_>>()
    );

    // --- resolve + expand work on the adopted index.
    let r = reader
        .resolve("[[customer::acme-corp]]", None)
        .await
        .unwrap();
    assert!(r.exists(), "wikilink must resolve on the adopted index");
    let expanded = reader
        .expand(ACME.0, None, None)
        .await
        .unwrap()
        .expect("acme page must expand");
    assert!(expanded.body.contains("manufacturing"));

    // --- Registry tables came along.
    let members = reader.list_group_members("team-acme").await.unwrap();
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].subject, "alice@example.com");
    let subs = reader.list_pack_subscriptions().await.unwrap();
    assert_eq!(subs.len(), 1);
    assert_eq!(subs[0].pack_id, "crm-core");
    assert_eq!(subs[0].version, 3);
}

#[tokio::test]
async fn adopt_noop_when_snapshot_unchanged() {
    let h = fresh_harness();
    let cfg = lake_config(&h);
    seed(&h, &[CUSTOMER_SKILL]).await;
    let report = publish_lake(&h.indexer, &cfg, None).await.expect("publish");

    let first = adopt_lake(&cfg, Arc::clone(&h.store), reader_embedder(), TENANT, None)
        .await
        .expect("first adopt")
        .expect("first adopt returns an indexer");
    assert_eq!(first.snapshot_id, report.snapshot_id);

    // Same snapshot already being served → nothing to adopt.
    let second = adopt_lake(
        &cfg,
        Arc::clone(&h.store),
        reader_embedder(),
        TENANT,
        Some(first.snapshot_id),
    )
    .await
    .expect("second adopt");
    assert!(
        second.is_none(),
        "adopt with current == latest must be a no-op"
    );

    // A new publish advances the snapshot → the next adopt picks it up.
    h.indexer.update_page(ACME.0, ACME.1).await.unwrap();
    let report2 = publish_lake(&h.indexer, &cfg, Some(report.epoch))
        .await
        .expect("second publish");
    let third = adopt_lake(
        &cfg,
        Arc::clone(&h.store),
        reader_embedder(),
        TENANT,
        Some(first.snapshot_id),
    )
    .await
    .expect("third adopt")
    .expect("a newer snapshot must be adopted");
    assert_eq!(third.snapshot_id, report2.snapshot_id);
    assert!(third.snapshot_id > first.snapshot_id);
}

#[tokio::test]
async fn poll_and_adopt_none_on_unpublished_lake() {
    // A lake that exists (catalog bootstrapped) but was never published
    // has no `escurel_manifest` — poll and adopt both report "nothing".
    let h = fresh_harness();
    let cfg = lake_config(&h);
    {
        // Bootstrap the catalog out-of-band (writer-side attach), no data.
        let conn = Connection::open_in_memory().unwrap();
        attach_lake(&conn, &cfg, false).unwrap();
    }
    assert_eq!(latest_lake_snapshot_id(&cfg).await.expect("poll"), None);
    let adopted = adopt_lake(&cfg, Arc::clone(&h.store), reader_embedder(), TENANT, None)
        .await
        .expect("adopt on an unpublished lake");
    assert!(adopted.is_none());
}

#[tokio::test]
async fn adopt_rejects_schema_or_model_mismatch() {
    let h = fresh_harness();
    let cfg = lake_config(&h);
    seed(&h, &[CUSTOMER_SKILL, ACME]).await;
    publish_lake(&h.indexer, &cfg, None).await.expect("publish");

    let corrupt = |sql: &str| {
        let conn = Connection::open_in_memory().unwrap();
        attach_lake(&conn, &cfg, false).unwrap();
        conn.execute_batch(sql).unwrap();
    };

    // Foreign embedding space → fail closed, no partial adopt.
    corrupt("UPDATE lake.escurel_manifest SET model_id = 'other-model';");
    let err = adopt_lake(&cfg, Arc::clone(&h.store), reader_embedder(), TENANT, None)
        .await
        .expect_err("a foreign model_id must be rejected");
    assert!(
        matches!(&err, SnapshotError::LakeIncompatible(msg) if msg.contains("model")),
        "want LakeIncompatible about the model, got: {err}"
    );

    // Foreign schema version → fail closed too.
    corrupt("UPDATE lake.escurel_manifest SET model_id = 'hash', schema_version = 999;");
    let err = adopt_lake(&cfg, Arc::clone(&h.store), reader_embedder(), TENANT, None)
        .await
        .expect_err("a foreign schema_version must be rejected");
    assert!(
        matches!(&err, SnapshotError::LakeIncompatible(msg) if msg.contains("schema")),
        "want LakeIncompatible about the schema, got: {err}"
    );

    // Restore the manifest → the same lake adopts cleanly (the gate is
    // the manifest, not a poisoned reader).
    corrupt(&format!(
        "UPDATE lake.escurel_manifest SET schema_version = {};",
        Migrator::SCHEMA_VERSION
    ));
    let adopted = adopt_lake(&cfg, Arc::clone(&h.store), reader_embedder(), TENANT, None)
        .await
        .expect("restored manifest must adopt")
        .expect("indexer");
    assert!(
        adopted
            .indexer
            .resolve("[[customer::acme-corp]]", None)
            .await
            .unwrap()
            .exists()
    );
}
