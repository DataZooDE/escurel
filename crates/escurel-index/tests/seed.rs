//! Integration test for `Indexer::seed_from_dir` — importing an
//! external directory of markdown (the `examples/crm-demo` corpus)
//! into a tenant: write to the canonical LaneStore + index into a
//! real DuckDB. No mocks; real FsStore tempdir + real DuckDB file.

use std::path::PathBuf;
use std::sync::Arc;

use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::read::{Direction, OrderDir};
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
async fn seed_loads_crm_demo_events_and_spine_history() {
    let (indexer, _s, _d) = fresh_indexer();
    indexer.seed_from_dir(&crm_demo_dir()).await.expect("seed");
    let spine = "markdown/instances/engagement__hoffmann-spine.md";

    // events.json populated the inbox + the spine's event history.
    let inbox = indexer.list_inbox(None).await.expect("list_inbox");
    assert!(!inbox.is_empty(), "crm-demo seeds inbox events");
    let history = indexer.list_events(spine, None).await.expect("list_events");
    assert!(
        history.len() >= 5,
        "the spine has its source-event history, got {}",
        history.len(),
    );
    assert!(history.iter().all(|e| e.status == "processed"));

    // history.json gives the spine a real CRDT snapshot timeline:
    // state-at-T reconstructs the contemporaneous contract_value.
    let early = indexer
        .expand(spine, Some("2026-03-14T00:00:00Z"), None)
        .await
        .expect("expand")
        .expect("spine at T");
    assert_eq!(
        early.frontmatter.get("phase").and_then(|v| v.as_str()),
        Some("prospecting"),
        "earliest snapshot is the prospecting state",
    );
    let later = indexer
        .expand(spine, Some("2026-05-15T00:00:00Z"), None)
        .await
        .expect("expand")
        .expect("spine at T2");
    assert_eq!(
        later.frontmatter.get("phase").and_then(|v| v.as_str()),
        Some("delivering"),
    );
}

#[tokio::test]
async fn seed_crm_demo_is_richly_connected() {
    let (indexer, _s, _d) = fresh_indexer();
    indexer.seed_from_dir(&crm_demo_dir()).await.expect("seed");

    // The corpus spans multiple accounts: every skill group is populated,
    // several with multiple instances (the Instances dropdown's grouping).
    async fn count(indexer: &Indexer, skill: &str) -> usize {
        indexer
            .list_instances(skill, Some(OrderDir::Asc), None, None, None, None)
            .await
            .expect("list_instances")
            .len()
    }
    assert!(count(&indexer, "customer").await >= 3, "≥3 customers");
    assert!(count(&indexer, "contact").await >= 6, "≥6 contacts");
    assert!(count(&indexer, "orgunit").await >= 2, "≥2 org units");
    assert!(count(&indexer, "engagement").await >= 3, "≥3 engagements");
    assert!(
        count(&indexer, "opportunity").await >= 3,
        "≥3 opportunities"
    );
    assert!(count(&indexer, "workstream").await >= 4, "≥4 workstreams");

    // The Münchner Pharma spine is a richly-connected hub: ≥7 backlinks
    // (incoming) and ≥5 outgoing — what the links footer renders.
    let spine = "markdown/instances/engagement__hoffmann-spine.md";
    let backlinks = indexer
        .neighbours(spine, Direction::In, None, None, None)
        .await
        .expect("neighbours In");
    assert!(
        backlinks.len() >= 7,
        "spine should have ≥7 backlinks, got {}",
        backlinks.len(),
    );
    let outgoing = indexer
        .neighbours(spine, Direction::Out, None, None, None)
        .await
        .expect("neighbours Out");
    assert!(
        outgoing.len() >= 5,
        "spine should have ≥5 outgoing links, got {}",
        outgoing.len(),
    );

    // A 2nd account's spine carries its own snapshot timeline (version
    // markers on more than one instance).
    let ha_spine = "markdown/instances/engagement__ha-spine.md";
    let snaps = indexer
        .list_snapshots(ha_spine)
        .await
        .expect("list_snapshots");
    assert!(
        snaps.len() >= 3,
        "the Hoffmann spine has a 3-state timeline, got {}",
        snaps.len()
    );

    // The inbox carries several unprocessed candidates across accounts.
    let inbox = indexer.list_inbox(None).await.expect("list_inbox");
    assert!(inbox.len() >= 5, "≥5 inbox events, got {}", inbox.len());
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
