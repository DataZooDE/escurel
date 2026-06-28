//! Integration tests for Matryoshka two-pass vector search (issue #218).
//!
//! Real DuckDB + real FsStore + `HashEmbedder`. The correctness property
//! pinned here: with a coarse shortlist large enough to cover the whole
//! corpus, the two-pass result (coarse prefix shortlist → exact full-dim
//! rescore) is **identical** to single-pass search — proving the rescoring
//! produces the true full-dimension ranking. (How small `coarse_candidates`
//! can shrink before recall drops is a model-quality question for the eval
//! harness, not a deterministic unit test.)

use std::sync::Arc;

use bytes::Bytes;
use duckdb::Connection;
use escurel_embed::{Embedder, HashEmbedder};
use escurel_index::{Indexer, Migrator, RetrievalConfig, SearchHit};
use escurel_storage::{FsStore, Key, LaneStore};
use tempfile::TempDir;

const TENANT: &str = "acme";

struct Harness {
    store: Arc<dyn LaneStore>,
    indexer: Indexer,
    _store_dir: TempDir,
    _db_dir: TempDir,
}

fn harness(retrieval: Option<RetrievalConfig>) -> Harness {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(HashEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let mut indexer = Indexer::new(Arc::clone(&store), embedder, conn, TENANT).unwrap();
    if let Some(r) = retrieval {
        indexer = indexer.with_retrieval(r);
    }
    Harness {
        store,
        indexer,
        _store_dir: store_dir,
        _db_dir: db_dir,
    }
}

async fn seed(h: &Harness, pages: &[(String, String)]) {
    for (path, body) in pages {
        let key = Key::new(TENANT, path.clone()).unwrap();
        h.store
            .write(&key, Bytes::copy_from_slice(body.as_bytes()))
            .await
            .unwrap();
        h.indexer.update_page(path, body).await.unwrap();
    }
    h.indexer.refresh_fts().await.unwrap();
}

fn skill() -> (String, String) {
    (
        "markdown/skills/note.md".to_owned(),
        "---\ntype: skill\nid: note\ndescription: notes\n---\n# note\n".to_owned(),
    )
}

fn note(id: &str, body: &str) -> (String, String) {
    (
        format!("markdown/instances/note/{id}.md"),
        format!("---\ntype: instance\nskill: note\nid: {id}\n---\n# {id}\n\n{body}\n"),
    )
}

fn ids(hits: &[SearchHit]) -> Vec<String> {
    hits.iter().map(|h| h.page_id.clone()).collect()
}

/// Six distinct notes; their bodies hash to well-separated unit vectors.
fn corpus() -> Vec<(String, String)> {
    vec![
        note(
            "alpha",
            "quarterly revenue planning across the northern region",
        ),
        note("beta", "the zebra crossing budget was approved last spring"),
        note(
            "gamma",
            "logistics throughput improved after the depot move",
        ),
        note("delta", "customer churn analysis for the enterprise tier"),
        note(
            "epsilon",
            "marketing spend allocation by acquisition channel",
        ),
        note(
            "zeta",
            "engineering headcount plan for the next fiscal year",
        ),
    ]
}

#[tokio::test]
async fn two_pass_matches_single_pass_when_shortlist_covers_corpus() {
    let mut pages = vec![skill()];
    pages.extend(corpus());

    let single = harness(None);
    seed(&single, &pages).await;

    // coarse_dim 128, coarse_candidates 1000 ≫ corpus → shortlist = everything.
    let twopass = harness(Some(RetrievalConfig::default().with_two_pass(128, 1000)));
    seed(&twopass, &pages).await;

    for q in ["planning", "logistics depot", "customer churn", "zebra"] {
        let s = single
            .indexer
            .search(q, 6, None, Some("note"), None, None)
            .await
            .unwrap();
        let t = twopass
            .indexer
            .search(q, 6, None, Some("note"), None, None)
            .await
            .unwrap();
        assert_eq!(
            ids(&s),
            ids(&t),
            "two-pass with a corpus-covering shortlist must equal single-pass for {q:?}",
        );
    }
}

#[tokio::test]
async fn two_pass_disabled_by_default() {
    let h = harness(None);
    assert!(!h.indexer.rerank_enabled());
    // Default config: two_pass off. A coarse_dim == full dim also reads as off.
    assert!(!RetrievalConfig::default().with_two_pass(768, 10).two_pass());
    assert!(RetrievalConfig::default().with_two_pass(128, 10).two_pass());
}
