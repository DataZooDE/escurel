//! A page written after the last FTS build reaches the BM25 arm (#868).
//!
//! The `fts` extension only rebuilds the whole index (`refresh_fts`), which
//! boot, rebuild and the pack tools call. Nothing called it after an ordinary
//! write, so everything written during a gateway's lifetime was invisible to
//! the lexical arm until the next restart. `refresh_fts_if_stale` refreshes when
//! (and only when) the index changed since the last build; the server calls it
//! on a timer. Real DuckDB, real FsStore.

use std::sync::Arc;

use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, LaneStore};
use tempfile::TempDir;

/// A token no embedder relates to anything: only BM25 can find it.
const TOKEN: &str = "zyxquorbit";

fn harness() -> (Indexer, TempDir, TempDir) {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let idx = Indexer::new(store, embedder, conn, "acme").unwrap();
    (idx, store_dir, db_dir)
}

/// The fused score of the fresh page. Reciprocal-rank fusion makes the number
/// of arms visible: a page only one arm returns scores at most 1/61 (≈ 0.0164),
/// one both arms return scores ≈ 2/61. With a zero embedder the vector arm
/// returns every page, so "found" alone would not show what BM25 sees.
async fn fresh_score(idx: &Indexer) -> f64 {
    idx.search(TOKEN, 5, None, None, None, None)
        .await
        .unwrap()
        .iter()
        .find(|h| h.page_id == "markdown/instances/note/fresh.md")
        .map_or(0.0, |h| h.score)
}

const ONE_ARM: f64 = 1.0 / 61.0 + 1e-9;

#[tokio::test]
async fn a_page_written_after_the_last_build_is_lexically_found_after_a_stale_refresh() {
    let (idx, _s, _d) = harness();
    idx.update_page(
        "markdown/skills/note.md",
        "---\nkind: skill\nid: note\ndescription: A note.\n---\n# note\n",
    )
    .await
    .unwrap();
    // The boot build.
    idx.refresh_fts().await.unwrap();
    assert!(
        !idx.refresh_fts_if_stale().await.unwrap(),
        "nothing changed since the build"
    );

    // Written while serving.
    idx.update_page(
        "markdown/instances/note/fresh.md",
        &format!(
            "---\nkind: instance\nskill: note\nid: fresh\n---\n# fresh\nThe {TOKEN} shipment.\n"
        ),
    )
    .await
    .unwrap();
    let before = fresh_score(&idx).await;
    assert!(
        before <= ONE_ARM,
        "precondition: only the vector arm sees the new page (score {before})"
    );

    assert!(
        idx.refresh_fts_if_stale().await.unwrap(),
        "a write since the build makes it stale"
    );
    let after = fresh_score(&idx).await;
    assert!(
        after > ONE_ARM,
        "after the refresh BM25 sees it too (score {before} -> {after})"
    );
    assert!(
        !idx.refresh_fts_if_stale().await.unwrap(),
        "and an idle tick does no work"
    );
}
