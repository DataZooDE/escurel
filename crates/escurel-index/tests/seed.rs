//! Integration test for `Indexer::seed_from_dir` — importing an
//! external directory of markdown (the `examples/crm-demo` corpus)
//! into a tenant: write to the canonical LaneStore + index into a
//! real DuckDB. No mocks; real FsStore tempdir + real DuckDB file.

use std::path::PathBuf;
use std::sync::Arc;

use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::read::OrderDir;
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, LaneStore};
use tempfile::TempDir;

const TENANT: &str = "acme";

fn crm_demo_dir() -> PathBuf {
    // crates/escurel-index → repo root → examples/crm-demo
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/crm-demo")
}

fn fresh_indexer() -> (Indexer, TempDir, TempDir) {
    let store_dir = TempDir::new().expect("store tempdir");
    let db_dir = TempDir::new().expect("db tempdir");
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).expect("open duckdb");
    Migrator::up(&conn).expect("migrate");
    let indexer = Indexer::new(store, embedder, conn, TENANT).expect("indexer");
    (indexer, store_dir, db_dir)
}

#[tokio::test]
async fn seed_from_dir_indexes_crm_demo() {
    let (indexer, _s, _d) = fresh_indexer();

    let n = indexer
        .seed_from_dir(&crm_demo_dir())
        .await
        .expect("seed crm-demo");
    assert!(
        n >= 12,
        "expected at least the 7 skills + 5 instances, got {n}"
    );

    // Skills are indexed.
    let skills = indexer.list_skills().await.expect("list_skills");
    let ids: Vec<&str> = skills.iter().map(|s| s.id.as_str()).collect();
    assert!(ids.contains(&"customer"), "skills: {ids:?}");
    assert!(ids.contains(&"opportunity"), "skills: {ids:?}");

    // The Hoffmann customer instance is indexed and listable.
    let customers = indexer
        .list_instances("customer", Some(OrderDir::Asc), None, None, None, None)
        .await
        .expect("list_instances customer");
    assert!(
        customers
            .iter()
            .any(|i| i.page_id.contains("muenchner-pharma")),
        "customers: {:?}",
        customers.iter().map(|i| &i.page_id).collect::<Vec<_>>(),
    );

    // A typed wikilink resolves (contact → customer back-reference exists).
    let resolved = indexer
        .resolve("[[customer::muenchner-pharma]]", None)
        .await
        .expect("resolve");
    assert!(
        resolved.exists(),
        "muenchner-pharma must resolve after seeding"
    );

    // Seeding wrote canonical markdown into the lane → audit is clean.
    let drift = indexer.audit().await.expect("audit");
    assert!(drift.is_clean(), "seed must leave no drift: {drift:?}");
}

#[tokio::test]
async fn seed_from_dir_is_idempotent() {
    let (indexer, _s, _d) = fresh_indexer();
    let dir = crm_demo_dir();
    let first = indexer.seed_from_dir(&dir).await.expect("seed 1");
    let second = indexer.seed_from_dir(&dir).await.expect("seed 2");
    assert_eq!(
        first, second,
        "re-seeding the same corpus seeds the same count"
    );
    // Still exactly one row per page (upsert, not duplicate).
    let drift = indexer.audit().await.expect("audit");
    assert!(drift.is_clean(), "re-seed must stay clean: {drift:?}");
}
