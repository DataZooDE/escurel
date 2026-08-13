//! Integration tests for `as_of` time-travel across the four read
//! tools (list_instances, expand, neighbours, search). Real DuckDB +
//! real FsStore + ZeroEmbedder, no mocks.
//!
//! The invariant under test: an `as_of` cut hides pages/blocks/edges
//! born *after* the instant, while untimed pages (skills, non-event
//! instances — `at_ts IS NULL`) always remain visible.

use std::sync::Arc;

use bytes::Bytes;
use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Direction, Indexer, Migrator, OrderDir};
use escurel_storage::{FsStore, Key, LaneStore};
use tempfile::TempDir;

const TENANT: &str = "acme";

// Event-typed skill + an untimed (non-event) skill.
const SKILL_DOC: (&str, &str) = (
    "markdown/skills/doc.md",
    "---\ntype: skill\nid: doc\ndescription: A document.\nrequired_frontmatter:\n  - at\n---\n# doc\n",
);
const SKILL_CUSTOMER: (&str, &str) = (
    "markdown/skills/customer.md",
    "---\ntype: skill\nid: customer\ndescription: A buying org.\n---\n# customer\n",
);

// doc::early born 2026-01-01; doc::late born 2026-02-01 and links to early.
const DOC_EARLY: (&str, &str) = (
    "markdown/instances/doc/early.md",
    "---\ntype: instance\nskill: doc\nid: early\nat: 2026-01-01T00:00:00Z\n---\n# Early\n\nThe quarterly zeppelin report.\n",
);
const DOC_LATE: (&str, &str) = (
    "markdown/instances/doc/late.md",
    "---\ntype: instance\nskill: doc\nid: late\nat: 2026-02-01T00:00:00Z\n---\n# Late\n\nFollow-up zeppelin memo: [[doc::early]].\n",
);

// Untimed instance — no `at`; must survive every cut.
const CUSTOMER_ACME: (&str, &str) = (
    "markdown/instances/customer/acme.md",
    "---\ntype: instance\nskill: customer\nid: acme\n---\n# Acme\n",
);

const CUT_MID: &str = "2026-01-15T00:00:00Z"; // between early and late
const CUT_AFTER: &str = "2026-03-01T00:00:00Z"; // after both

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
    seed(
        h,
        &[
            SKILL_DOC,
            SKILL_CUSTOMER,
            DOC_EARLY,
            DOC_LATE,
            CUSTOMER_ACME,
        ],
    )
    .await;
}

// --- list_instances ---------------------------------------------

#[tokio::test]
async fn list_instances_as_of_excludes_later_events() {
    let h = fresh_harness();
    seed_all(&h).await;

    let all = h
        .indexer
        .list_instances("doc", Some(OrderDir::Desc), None, None, None, None)
        .await
        .unwrap();
    assert_eq!(all.len(), 2, "both docs without a cut");

    let cut = h
        .indexer
        .list_instances("doc", Some(OrderDir::Desc), None, None, Some(CUT_MID), None)
        .await
        .unwrap();
    assert_eq!(cut.len(), 1, "late doc is not born yet at the cut");
    assert_eq!(cut[0].at.as_deref(), Some("2026-01-01T00:00:00Z"));
}

#[tokio::test]
async fn list_instances_as_of_keeps_untimed_instances() {
    let h = fresh_harness();
    seed_all(&h).await;

    // The untimed customer survives even a cut before its file existed.
    let cut = h
        .indexer
        .list_instances("customer", None, None, None, Some(CUT_MID), None)
        .await
        .unwrap();
    assert_eq!(cut.len(), 1, "untimed instance must always remain");
    assert_eq!(
        cut[0].frontmatter.get("id").and_then(|v| v.as_str()),
        Some("acme"),
    );
}

// --- expand -----------------------------------------------------

#[tokio::test]
async fn expand_as_of_returns_none_for_unborn_page() {
    let h = fresh_harness();
    seed_all(&h).await;

    let late = h
        .indexer
        .expand(DOC_LATE.0, Some(CUT_MID), None)
        .await
        .unwrap();
    assert!(late.is_none(), "late doc is not born yet at the cut");

    let early = h
        .indexer
        .expand(DOC_EARLY.0, Some(CUT_MID), None)
        .await
        .unwrap();
    assert!(early.is_some(), "early doc predates the cut");

    // After both → late resolves again.
    let late_after = h
        .indexer
        .expand(DOC_LATE.0, Some(CUT_AFTER), None)
        .await
        .unwrap();
    assert!(late_after.is_some());
}

#[tokio::test]
async fn expand_as_of_keeps_untimed_skill_pages() {
    let h = fresh_harness();
    seed_all(&h).await;
    let skill = h
        .indexer
        .expand(SKILL_DOC.0, Some(CUT_MID), None)
        .await
        .unwrap();
    assert!(skill.is_some(), "skill pages are untimed and never vanish");
}

// --- neighbours -------------------------------------------------

#[tokio::test]
async fn neighbours_as_of_hides_edges_from_unborn_sources() {
    let h = fresh_harness();
    seed_all(&h).await;

    // late → early is an inbound edge of `early`, sourced from `late`
    // (born 2026-02-01).
    let without_cut = h
        .indexer
        .neighbours(DOC_EARLY.0, Direction::In, None, None, None)
        .await
        .unwrap();
    assert_eq!(without_cut.len(), 1, "the late→early edge exists");

    let mid = h
        .indexer
        .neighbours(DOC_EARLY.0, Direction::In, None, Some(CUT_MID), None)
        .await
        .unwrap();
    assert!(mid.is_empty(), "source `late` is not born yet at the cut");

    let after = h
        .indexer
        .neighbours(DOC_EARLY.0, Direction::In, None, Some(CUT_AFTER), None)
        .await
        .unwrap();
    assert_eq!(after.len(), 1, "edge reappears once the source is born");
}

// --- search -----------------------------------------------------

#[tokio::test]
async fn search_as_of_excludes_later_blocks() {
    let h = fresh_harness();
    seed_all(&h).await;

    // The ZeroEmbedder's vector half ties across all blocks, so we
    // assert on *membership* rather than exact counts: the `as_of`
    // filter must drop the late doc's block from both search halves.
    let all = h
        .indexer
        .search("zeppelin", 10, None, None, None, None)
        .await
        .unwrap();
    let pages: Vec<String> = all.iter().map(|h| h.page_id.clone()).collect();
    assert!(
        pages.iter().any(|p| p.ends_with("early.md"))
            && pages.iter().any(|p| p.ends_with("late.md")),
        "both docs visible without a cut",
    );

    let cut = h
        .indexer
        .search("zeppelin", 10, None, None, Some(CUT_MID), None)
        .await
        .unwrap();
    let cut_pages: Vec<String> = cut.iter().map(|h| h.page_id.clone()).collect();
    assert!(
        cut_pages.iter().any(|p| p.ends_with("early.md")),
        "early doc predates the cut",
    );
    assert!(
        !cut_pages.iter().any(|p| p.ends_with("late.md")),
        "late doc is not born yet at the cut",
    );
}
