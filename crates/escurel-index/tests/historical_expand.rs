//! M7-PR3: `expand(as_of=T)` reconstructs an instance's frontmatter+body
//! *as it was* at T by replaying the CRDT snapshot at-or-before T. Real
//! DuckDB + real Loro snapshots + FsStore + ZeroEmbedder, no mocks.

use std::sync::Arc;

use bytes::Bytes;
use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, Key, LaneStore};
use tempfile::TempDir;

const TENANT: &str = "acme";
const SPINE: &str = "markdown/instances/engagement/spine.md";

fn spine_md(contract_value: &str, phase: &str) -> String {
    format!(
        "---\ntype: instance\nskill: engagement\nid: hoffmann-spine\nat: 2026-03-01T00:00:00Z\n\
         contract_value: \"{contract_value}\"\nphase: {phase}\n---\n# Spine\n\nThe lifecycle spine.\n"
    )
}

struct Harness {
    store: Arc<dyn LaneStore>,
    indexer: Indexer,
    _store_dir: TempDir,
    _db_dir: TempDir,
}

fn fresh_harness() -> Harness {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let duckdb_path = db_dir.path().join("escurel.duckdb");
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(&duckdb_path).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Indexer::new(Arc::clone(&store), embedder, conn, TENANT).unwrap();
    Harness {
        store,
        indexer,
        _store_dir: store_dir,
        _db_dir: db_dir,
    }
}

async fn write_page(h: &Harness, path: &str, body: &str) {
    let key = Key::new(TENANT, path.to_owned()).unwrap();
    h.store
        .write(&key, Bytes::from(body.to_owned()))
        .await
        .unwrap();
    h.indexer.update_page(path, body).await.unwrap();
}

fn cv(p: &escurel_index::ExpandedPage) -> String {
    p.frontmatter
        .get("contract_value")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_owned()
}

#[tokio::test]
async fn expand_as_of_reconstructs_field_values_from_snapshots() {
    let h = fresh_harness();

    // Current state (the live `pages` row): the final figure.
    write_page(&h, SPINE, &spine_md("620k", "scaling")).await;

    // Seed a real CRDT snapshot history: the spine's state evolving as
    // events landed over the timeline.
    h.indexer
        .seed_snapshot_history(
            SPINE,
            &[
                ("2026-03-10T00:00:00Z", &spine_md("350k", "qualifying")),
                ("2026-04-10T00:00:00Z", &spine_md("420k", "delivering")),
                ("2026-05-10T00:00:00Z", &spine_md("620k", "scaling")),
            ],
        )
        .await
        .unwrap();

    // No cut → the current live state.
    let now = h.indexer.expand(SPINE, None, None).await.unwrap().unwrap();
    assert_eq!(cv(&now), "620k");

    // As-of between the first two snapshots → the first snapshot's value.
    let early = h
        .indexer
        .expand(SPINE, Some("2026-03-20T00:00:00Z"), None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        cv(&early),
        "350k",
        "state-at-T replays the snapshot at-or-before T"
    );
    assert_eq!(
        early.frontmatter.get("phase").and_then(|v| v.as_str()),
        Some("qualifying"),
    );

    // As-of after the second snapshot, before the third.
    let mid = h
        .indexer
        .expand(SPINE, Some("2026-04-20T00:00:00Z"), None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cv(&mid), "420k");

    // As-of after the last snapshot → the last snapshot's value.
    let late = h
        .indexer
        .expand(SPINE, Some("2026-06-01T00:00:00Z"), None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cv(&late), "620k");
}

#[tokio::test]
async fn expand_as_of_without_snapshots_keeps_birth_filter_behaviour() {
    let h = fresh_harness();
    // A normal page with NO snapshot history: as_of falls through to the
    // existing birth-time filter (born 2026-03-01).
    write_page(&h, SPINE, &spine_md("350k", "qualifying")).await;

    // Before birth → not yet born → None.
    assert!(
        h.indexer
            .expand(SPINE, Some("2026-02-01T00:00:00Z"), None)
            .await
            .unwrap()
            .is_none(),
    );
    // After birth → current state.
    let p = h
        .indexer
        .expand(SPINE, Some("2026-04-01T00:00:00Z"), None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cv(&p), "350k");
}

#[tokio::test]
async fn list_snapshots_returns_taken_at_points_oldest_first() {
    let h = fresh_harness();
    write_page(&h, SPINE, &spine_md("620k", "scaling")).await;

    // No history yet → empty.
    assert!(h.indexer.list_snapshots(SPINE).await.unwrap().is_empty());

    h.indexer
        .seed_snapshot_history(
            SPINE,
            &[
                ("2026-03-10T00:00:00Z", &spine_md("350k", "qualifying")),
                ("2026-04-10T00:00:00Z", &spine_md("420k", "delivering")),
                ("2026-05-10T00:00:00Z", &spine_md("620k", "scaling")),
            ],
        )
        .await
        .unwrap();

    // The discrete state-over-time points `expand(as_of=T)` can replay,
    // oldest first.
    let snaps = h.indexer.list_snapshots(SPINE).await.unwrap();
    assert_eq!(
        snaps,
        vec![
            "2026-03-10T00:00:00Z".to_owned(),
            "2026-04-10T00:00:00Z".to_owned(),
            "2026-05-10T00:00:00Z".to_owned(),
        ],
    );

    // A page with no history → empty (not an error).
    assert!(
        h.indexer
            .list_snapshots("markdown/instances/engagement/other.md")
            .await
            .unwrap()
            .is_empty()
    );
}
