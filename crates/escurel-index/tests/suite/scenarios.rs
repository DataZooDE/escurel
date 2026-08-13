//! Integration tests for scenario overlays (A/B/C what-if branches).
//! Real DuckDB + real FsStore + ZeroEmbedder, no mocks.
//!
//! Invariant: `scenario = None` sees only the shared base; `Some("B")`
//! sees base ∪ the B overlay, where a B page overrides its base twin
//! (same slug) and B-only pages appear. Overlays never tombstone base.

use std::sync::Arc;

use bytes::Bytes;
use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Direction, Indexer, Migrator};
use escurel_storage::{FsStore, Key, LaneStore};
use tempfile::TempDir;

const TENANT: &str = "acme";

const SKILL_ENGAGEMENT: (&str, &str) = (
    "markdown/skills/engagement.md",
    "---\ntype: skill\nid: engagement\ndescription: A delivery engagement.\n---\n# engagement\n",
);

// Base spine: contract_value €350k, no scenario (shared base).
const ENG_BASE: (&str, &str) = (
    "markdown/instances/engagement/spine.md",
    "---\ntype: instance\nskill: engagement\nid: hoffmann-spine\ncontract_value: \"350k\"\n---\n# Spine (base)\n",
);
// Scenario-B overlay of the SAME slug: contract_value €500k.
const ENG_B: (&str, &str) = (
    "markdown/instances/engagement/spine.b.md",
    "---\ntype: instance\nskill: engagement\nid: hoffmann-spine\nscenario: B\ncontract_value: \"500k\"\n---\n# Spine (scenario B)\n",
);
// B-only instance (no base twin). Its body carries a unique search
// term and an outbound link to the base spine, so it exercises the
// scenario filter on search + neighbours too.
const ENG_B_ONLY: (&str, &str) = (
    "markdown/instances/engagement/expansion.b.md",
    "---\ntype: instance\nskill: engagement\nid: expansion\nscenario: B\ncontract_value: \"120k\"\n---\n# Expansion (B-only)\n\nThe zeppelin expansion case for [[engagement::hoffmann-spine]].\n",
);

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

async fn seed(h: &Harness, pages: &[(&str, &'static str)]) {
    for (path, body) in pages {
        let key = Key::new(TENANT, path.to_owned()).unwrap();
        h.store
            .write(&key, Bytes::from_static(body.as_bytes()))
            .await
            .unwrap();
        h.indexer.update_page(path, body).await.unwrap();
    }
}

async fn seed_all(h: &Harness) {
    seed(h, &[SKILL_ENGAGEMENT, ENG_BASE, ENG_B, ENG_B_ONLY]).await;
}

fn cv(i: &escurel_index::InstanceInfo) -> Option<String> {
    i.frontmatter
        .get("contract_value")
        .and_then(|v| v.as_str())
        .map(str::to_owned)
}

#[tokio::test]
async fn list_instances_base_view_excludes_overlays() {
    let h = fresh_harness();
    seed_all(&h).await;

    let base = h
        .indexer
        .list_instances("engagement", None, None, None, None, None)
        .await
        .unwrap();
    // Only the base spine; neither the B overlay nor the B-only show.
    assert_eq!(base.len(), 1, "base view is base-only");
    assert_eq!(cv(&base[0]).as_deref(), Some("350k"));
}

#[tokio::test]
async fn list_instances_scenario_b_overrides_and_adds() {
    let h = fresh_harness();
    seed_all(&h).await;

    let b = h
        .indexer
        .list_instances("engagement", None, None, None, None, Some("B"))
        .await
        .unwrap();
    // spine (overlay wins → 500k, not duplicated) + the B-only expansion.
    assert_eq!(b.len(), 2, "base ∪ overlay, deduped per slug");
    let spine = b
        .iter()
        .find(|i| i.frontmatter.get("id").and_then(|v| v.as_str()) == Some("hoffmann-spine"))
        .expect("spine present");
    assert_eq!(cv(spine).as_deref(), Some("500k"), "overlay overrides base");
    assert!(
        b.iter()
            .any(|i| i.frontmatter.get("id").and_then(|v| v.as_str()) == Some("expansion")),
        "B-only instance appears",
    );
}

#[tokio::test]
async fn resolve_picks_overlay_over_base() {
    let h = fresh_harness();
    seed_all(&h).await;

    let base = h
        .indexer
        .resolve("[[engagement::hoffmann-spine]]", None)
        .await
        .unwrap();
    assert_eq!(
        base.page.expect("base resolves").page_id,
        "markdown/instances/engagement/spine.md",
    );

    let overlay = h
        .indexer
        .resolve("[[engagement::hoffmann-spine]]", Some("B"))
        .await
        .unwrap();
    assert_eq!(
        overlay.page.expect("B resolves").page_id,
        "markdown/instances/engagement/spine.b.md",
        "scenario B prefers the overlay page",
    );
}

#[tokio::test]
async fn resolve_base_does_not_see_b_only() {
    let h = fresh_harness();
    seed_all(&h).await;
    let base = h
        .indexer
        .resolve("[[engagement::expansion]]", None)
        .await
        .unwrap();
    assert!(base.page.is_none(), "B-only instance is invisible in base");

    let b = h
        .indexer
        .resolve("[[engagement::expansion]]", Some("B"))
        .await
        .unwrap();
    assert!(b.page.is_some(), "visible under scenario B");
}

// --- expand / neighbours / search scenario gating -----------------

#[tokio::test]
async fn expand_base_hides_overlay_pages() {
    let h = fresh_harness();
    seed_all(&h).await;

    // The B-only page is invisible in base view, visible under B.
    assert!(
        h.indexer
            .expand(ENG_B_ONLY.0, None, None)
            .await
            .unwrap()
            .is_none(),
        "B-only page hidden in base",
    );
    assert!(
        h.indexer
            .expand(ENG_B_ONLY.0, None, Some("B"))
            .await
            .unwrap()
            .is_some(),
        "B-only page visible under scenario B",
    );
    // Base pages stay visible in base view.
    assert!(
        h.indexer
            .expand(ENG_BASE.0, None, None)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn neighbours_filters_edges_by_source_scenario() {
    let h = fresh_harness();
    seed_all(&h).await;

    // The expansion (B) page links to the base spine. That inbound edge
    // is sourced from a B page, so it's hidden in base view…
    let base = h
        .indexer
        .neighbours(ENG_BASE.0, Direction::In, None, None, None)
        .await
        .unwrap();
    assert!(
        !base.iter().any(|e| e.src_page == ENG_B_ONLY.0),
        "overlay-sourced edge hidden in base",
    );

    // …and present under scenario B.
    let b = h
        .indexer
        .neighbours(ENG_BASE.0, Direction::In, None, None, Some("B"))
        .await
        .unwrap();
    assert!(
        b.iter().any(|e| e.src_page == ENG_B_ONLY.0),
        "overlay-sourced edge visible under scenario B",
    );
}

#[tokio::test]
async fn search_filters_blocks_by_scenario() {
    let h = fresh_harness();
    seed_all(&h).await;
    h.indexer.refresh_fts().await.unwrap();

    // "zeppelin" lives only in the B-only page's body.
    let base = h
        .indexer
        .search("zeppelin", 10, None, None, None, None)
        .await
        .unwrap();
    assert!(
        !base.iter().any(|hit| hit.page_id == ENG_B_ONLY.0),
        "B-only block excluded from base search",
    );

    let b = h
        .indexer
        .search("zeppelin", 10, None, None, None, Some("B"))
        .await
        .unwrap();
    assert!(
        b.iter().any(|hit| hit.page_id == ENG_B_ONLY.0),
        "B-only block included under scenario B",
    );
}
