//! A rebuild must never truncate a populated index against a listing it
//! could not prove complete.
//!
//! `rebuild` reads the lane store, drops `pages`/`blocks`/`links`, and
//! re-indexes what it read. That is correct when the listing is the truth.
//! It is catastrophic when the listing is empty because the store was
//! unreachable: the index is emptied, and — until this lands — the orphan
//! blob reclaim that used to run at the end of `rebuild` then deleted every
//! canonical blob the emptied index no longer referenced.
//!
//! `LaneStore::list` now errors rather than returning an empty vec when it
//! cannot prove completeness, so the unreachable case no longer reaches
//! here at all. This is the second line of defence, for the case the first
//! cannot see: a store that is reachable and answers, truthfully, with
//! nothing — while the index holds pages that came from somewhere.

use std::sync::Arc;

use bytes::Bytes;
use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, Key, LaneStore};
use tempfile::TempDir;

const TENANT: &str = "acme";

struct Harness {
    store: Arc<dyn LaneStore>,
    indexer: Indexer,
    db_path: std::path::PathBuf,
    _store_dir: TempDir,
    _db_dir: TempDir,
}

impl Harness {
    /// Count rows directly, the way `index_roundtrip` does — the point of
    /// these tests is what is ON DISK after a refused rebuild.
    fn count_pages(&self) -> i64 {
        let conn = Connection::open(&self.db_path).expect("reopen");
        conn.query_row("SELECT count(*) FROM pages", [], |row| row.get(0))
            .expect("count pages")
    }
}

fn fresh_harness() -> Harness {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let db_path = db_dir.path().join("escurel.duckdb");
    let conn = Connection::open(&db_path).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Indexer::new(Arc::clone(&store), embedder, conn, TENANT).unwrap();
    Harness {
        store,
        indexer,
        db_path,
        _store_dir: store_dir,
        _db_dir: db_dir,
    }
}

const PAGE: (&str, &str) = (
    "markdown/instances/note/alpha.md",
    "---\ntype: instance\nskill: note\nid: alpha\n---\n# Alpha\n\nBody.\n",
);

/// An empty corpus must not be allowed to erase a populated index.
#[tokio::test]
async fn rebuild_refuses_to_empty_a_populated_index() {
    let h = fresh_harness();
    h.indexer
        .update_page(PAGE.0, PAGE.1)
        .await
        .expect("seed one page");
    assert_eq!(h.count_pages(), 1);

    // The lane goes empty while the index still holds the page — the shape
    // a half-readable store produces once `list` itself stops lying.
    let key = Key::new(TENANT.to_owned(), PAGE.0.to_owned()).unwrap();
    h.store.delete(&key).await.expect("remove the lane copy");

    let err = h
        .indexer
        .rebuild()
        .await
        .expect_err("an empty listing must not truncate a populated index");
    let msg = err.to_string();
    assert!(
        msg.contains("empty") || msg.contains("refus"),
        "the refusal must say what it refused and why: {msg}"
    );
    assert_eq!(
        h.count_pages(),
        1,
        "the index must be untouched by a refused rebuild"
    );
}

/// The control: a genuinely empty corpus and a genuinely empty index is a
/// legitimate rebuild, and must still succeed.
#[tokio::test]
async fn rebuild_of_an_empty_corpus_into_an_empty_index_is_allowed() {
    let h = fresh_harness();
    h.store
        .write(
            // OUTSIDE `markdown/`, so it provisions the tenant without
            // appearing in the corpus listing the rebuild reads.
            &Key::new(TENANT.to_owned(), "blobs/.keep".to_owned()).unwrap(),
            Bytes::from_static(b""),
        )
        .await
        .expect("provision the tenant");
    h.indexer
        .rebuild()
        .await
        .expect("an empty corpus rebuilding an empty index is not a mistake");
    assert_eq!(h.count_pages(), 0);
}
