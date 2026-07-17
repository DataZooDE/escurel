//! Capstone E2E for the offline batch loader: build an artifact, then transfer
//! it into a fresh live escurel data dir. Real DuckDB + FsStore + attach, no
//! mocks. The decisive assertions: transferred `blocks.dense_vec` is
//! byte-identical to the loader's (no re-embed), the merged instances are
//! search-hittable (HNSW recreate + FTS refresh ran), re-transfer is idempotent
//! (skip), and a manifest model mismatch aborts before any rows move.

use std::path::Path;
use std::sync::Arc;

use escurel_embed::{Embedder, HashEmbedder};
use escurel_index::Indexer;
use escurel_index::backend::{Extractor, PlainTextExtractor};
use escurel_index::indexer::OnCollision;
use escurel_index::schema::Migrator;
use escurel_loader::{LoaderBuilder, LoaderError, transfer};
use escurel_storage::{FsStore, LaneStore};
use tempfile::TempDir;

/// (page_id, ordinal, dense_vec[1]) for every block — the embedding fingerprint.
fn vec_heads(db: &Path) -> Vec<(String, i64, f32)> {
    let conn = duckdb::Connection::open(db).unwrap();
    Migrator::load_extensions(&conn).unwrap();
    Migrator::enable_hnsw_persistence(&conn).unwrap();
    let mut stmt = conn
        .prepare("SELECT page_id, ordinal, dense_vec[1] FROM blocks ORDER BY page_id, ordinal")
        .unwrap();
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, f32>(2)?,
            ))
        })
        .unwrap();
    rows.map(|r| r.unwrap()).collect()
}

async fn build_loader(dir: &Path) {
    let src = TempDir::new().unwrap();
    std::fs::write(src.path().join("a.txt"), "alpha widgets report").unwrap();
    std::fs::write(src.path().join("b.txt"), "beta gadgets summary").unwrap();
    let extractor: Arc<dyn Extractor> = Arc::new(PlainTextExtractor);
    let embedder: Arc<dyn Embedder> = Arc::new(HashEmbedder::default());
    let report = LoaderBuilder::new(dir, "attachment", extractor, embedder)
        .build(src.path())
        .await
        .expect("loader build");
    assert_eq!(report.manifest.model_id, "hash");
    assert_eq!(report.manifest.doc_count, 2);
}

#[tokio::test]
async fn transfer_carries_embeddings_verbatim_and_is_idempotent() {
    let loader_dir = TempDir::new().unwrap();
    build_loader(loader_dir.path()).await;
    let loader_db = loader_dir.path().join("escurel.duckdb");
    let loader_heads = vec_heads(&loader_db);
    assert!(!loader_heads.is_empty());
    // HashEmbedder produces non-zero vectors — so "byte-identical" is a real
    // signal (a re-embed in the transfer would NOT reproduce these).
    assert!(
        loader_heads.iter().any(|(_, _, h)| *h != 0.0),
        "loader vectors non-zero"
    );

    // Transfer into a fresh, empty live data dir.
    let live = TempDir::new().unwrap();
    let report = transfer(
        loader_dir.path(),
        live.path(),
        "acme",
        "hash",
        OnCollision::Skip,
    )
    .await
    .expect("transfer");
    assert_eq!(report.merge.source_pages, 2);
    assert_eq!(
        report.merge.collisions, 0,
        "fresh tenant — nothing collides"
    );
    assert_eq!(report.files.blobs, 2);
    assert_eq!(report.files.overlays, 2);

    // Vectors copied byte-for-byte — the no-re-embed guarantee.
    let live_db = live
        .path()
        .join("tenants")
        .join("acme")
        .join("escurel.duckdb");
    assert_eq!(
        vec_heads(&live_db),
        loader_heads,
        "dense_vec transferred verbatim"
    );

    // The merged instances are search-hittable (HNSW recreate + FTS refresh).
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(live.path().to_path_buf()));
    let conn = duckdb::Connection::open(&live_db).unwrap();
    Migrator::load_extensions(&conn).unwrap();
    Migrator::enable_hnsw_persistence(&conn).unwrap();
    let embedder: Arc<dyn Embedder> = Arc::new(HashEmbedder::default());
    let idx = Indexer::new(store, embedder, conn, "acme").unwrap();
    let hits = idx
        .search("widgets", 10, None, None, None, None)
        .await
        .unwrap();
    assert!(
        !hits.is_empty(),
        "transferred instance is searchable: {hits:?}"
    );

    // Idempotent re-transfer (skip): same source, all collide, counts stable.
    let again = transfer(
        loader_dir.path(),
        live.path(),
        "acme",
        "hash",
        OnCollision::Skip,
    )
    .await
    .expect("re-transfer");
    assert_eq!(again.merge.collisions, 2, "all source pages now collide");
    assert_eq!(
        vec_heads(&live_db),
        loader_heads,
        "no duplicates, vectors unchanged"
    );
}

#[tokio::test]
async fn transfer_error_policy_aborts_on_collision_without_mutating() {
    let loader_dir = TempDir::new().unwrap();
    build_loader(loader_dir.path()).await;
    let live = TempDir::new().unwrap();

    // Seed the tenant.
    transfer(
        loader_dir.path(),
        live.path(),
        "acme",
        "hash",
        OnCollision::Skip,
    )
    .await
    .expect("seed transfer");
    let live_db = live
        .path()
        .join("tenants")
        .join("acme")
        .join("escurel.duckdb");
    let before = vec_heads(&live_db);

    // A second transfer with on_collision=error must abort (every page collides)
    // and report that nothing was copied — the row state is unchanged.
    let err = transfer(
        loader_dir.path(),
        live.path(),
        "acme",
        "hash",
        OnCollision::Error,
    )
    .await
    .expect_err("error policy must abort on collision");
    assert!(format!("{err}").contains("nothing was copied"), "got {err}");
    assert_eq!(
        vec_heads(&live_db),
        before,
        "row state unchanged after aborted error transfer"
    );
}

#[tokio::test]
async fn transfer_aborts_on_embedder_model_mismatch() {
    let loader_dir = TempDir::new().unwrap();
    build_loader(loader_dir.path()).await;

    let live = TempDir::new().unwrap();
    // Live tenant expects a DIFFERENT embedding space than the artifact's "hash".
    let err = transfer(
        loader_dir.path(),
        live.path(),
        "acme",
        "gemini-embedding-001",
        OnCollision::Skip,
    )
    .await
    .expect_err("mismatch must abort");
    assert!(matches!(err, LoaderError::Incompatible(_)), "got {err:?}");
    assert!(format!("{err}").contains("model mismatch"));

    // Nothing was merged: the live DB has no instance rows (abort before attach).
    let live_db = live
        .path()
        .join("tenants")
        .join("acme")
        .join("escurel.duckdb");
    if live_db.exists() {
        assert!(vec_heads(&live_db).is_empty(), "no rows merged on abort");
    }
}
