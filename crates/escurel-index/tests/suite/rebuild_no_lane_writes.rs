//! A rebuild reads the lane and writes only the index.
//!
//! `Indexer::rebuild` re-derives `pages`/`blocks`/`links` from the markdown the lane already holds.
//! It used to do that through `update_page_as`, which writes the lane FIRST. So every cold boot on a
//! fresh data directory wrote each page's identical markdown back to the lane. On a Google Drive lane
//! that is one Drive write per page (~1.5 s each: heron-escurel, 223 pages, a large share of a 20 minute
//! boot), and one transient Drive error was fatal: "building indexer: lane store error: io error:
//! duckvfs write: temporary Drive API failure".
//!
//! The lane is the source of truth and a rebuild must treat it as read-only. These tests count the
//! writes a real `FsStore` receives, and compare the index a rebuild produces with the one the write
//! path produced.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use bytes::Bytes;
use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, Key, LaneStore, Result as StoreResult, Version};
use tempfile::TempDir;
use url::Url;

const TENANT: &str = "acme";

/// A real `FsStore` that counts the writes it receives. Nothing is faked: every call is forwarded.
struct CountingStore {
    inner: FsStore,
    writes: AtomicUsize,
    deletes: AtomicUsize,
}

#[async_trait]
impl LaneStore for CountingStore {
    async fn read(&self, key: &Key) -> StoreResult<Bytes> {
        self.inner.read(key).await
    }
    async fn write(&self, key: &Key, body: Bytes) -> StoreResult<Version> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        self.inner.write(key, body).await
    }
    async fn list(&self, prefix: &Key) -> StoreResult<Vec<Key>> {
        self.inner.list(prefix).await
    }
    async fn delete(&self, key: &Key) -> StoreResult<()> {
        self.deletes.fetch_add(1, Ordering::SeqCst);
        self.inner.delete(key).await
    }
    fn url(&self, key: &Key) -> StoreResult<Url> {
        self.inner.url(key)
    }
}

struct Harness {
    store: Arc<CountingStore>,
    indexer: Indexer,
    db_path: std::path::PathBuf,
    _store_dir: TempDir,
    _db_dir: TempDir,
}

fn new_indexer(store: &Arc<CountingStore>, db_path: &std::path::Path) -> Indexer {
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_path).unwrap();
    Migrator::up(&conn).unwrap();
    Indexer::new(
        Arc::clone(store) as Arc<dyn LaneStore>,
        embedder,
        conn,
        TENANT,
    )
    .unwrap()
}

fn harness() -> Harness {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let store = Arc::new(CountingStore {
        inner: FsStore::new(store_dir.path().to_path_buf()),
        writes: AtomicUsize::new(0),
        deletes: AtomicUsize::new(0),
    });
    let db_path = db_dir.path().join("escurel.duckdb");
    let indexer = new_indexer(&store, &db_path);
    Harness {
        store,
        indexer,
        db_path,
        _store_dir: store_dir,
        _db_dir: db_dir,
    }
}

const SKILL: (&str, &str) = (
    "markdown/skills/note.md",
    "---\nkind: skill\nid: note\n---\n# Note\n\nA note.\n",
);
const A: (&str, &str) = (
    "markdown/instances/note/alpha.md",
    "---\nkind: instance\nskill: note\nid: alpha\nabout: \"[[note::beta]]\"\n---\n# Alpha\n\nSee [[note::beta]].\n",
);
const B: (&str, &str) = (
    "markdown/instances/note/beta.md",
    "---\nkind: instance\nskill: note\nid: beta\n---\n# Beta\n\nBack to [[note::alpha]].\n",
);
const ARCHIVED: (&str, &str) = (
    "markdown/instances/note/gone.md",
    "---\nkind: instance\nskill: note\nid: gone\narchived: true\n---\n# Gone\n",
);

/// The index, read back as plain data, for comparing two derivations of the same lane.
fn snapshot(db_path: &std::path::Path) -> (Vec<String>, Vec<String>, Vec<String>) {
    let conn = Connection::open(db_path).unwrap();
    let rows = |sql: &str| -> Vec<String> {
        let mut stmt = conn.prepare(sql).unwrap();
        let mut v: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        v.sort();
        v
    };
    (
        rows(
            "SELECT page_id || '|' || coalesce(slug,'') || '|' || skill || '|' || page_type \
             || '|' || body_hash FROM pages",
        ),
        rows("SELECT page_id || '|' || anchor || '|' || body FROM blocks"),
        rows(
            "SELECT src_page || '|' || coalesce(src_field,'') || '|' || dst_page || '|' || link_skill \
             FROM links",
        ),
    )
}

async fn seed(h: &Harness) {
    for (id, body) in [SKILL, A, B] {
        h.indexer.update_page(id, body).await.expect("seed page");
    }
    // An archived page is kept in the lane but out of the index; write it straight to the lane.
    let key = Key::new(TENANT, ARCHIVED.0.to_owned()).unwrap();
    h.store
        .write(&key, Bytes::from(ARCHIVED.1))
        .await
        .expect("archived page");
}

/// RED before the fix: a rebuild wrote every page back to the lane.
#[tokio::test]
async fn rebuild_performs_no_lane_writes() {
    let h = harness();
    seed(&h).await;
    let before = h.store.writes.load(Ordering::SeqCst);
    assert!(before >= 4, "the seed writes the lane ({before})");

    h.store.writes.store(0, Ordering::SeqCst);
    h.indexer.rebuild().await.expect("rebuild");
    assert_eq!(
        h.store.writes.load(Ordering::SeqCst),
        0,
        "rebuild is read-only on the lane (it used to rewrite every page: one Drive write each)"
    );
    assert_eq!(h.store.deletes.load(Ordering::SeqCst), 0);
}

/// The index a rebuild derives from the lane equals the one the write path built, so dropping the
/// lane write changed nothing the index depends on. Uses a FRESH database on the same lane: exactly a
/// cold boot on a new data directory.
#[tokio::test]
async fn rebuild_on_a_fresh_database_yields_the_same_index() {
    let h = harness();
    seed(&h).await;
    let written = snapshot(&h.db_path);
    assert_eq!(written.0.len(), 3, "the archived page is not indexed");

    let fresh_dir = TempDir::new().unwrap();
    let fresh_db = fresh_dir.path().join("fresh.duckdb");
    let fresh = new_indexer(&h.store, &fresh_db);
    h.store.writes.store(0, Ordering::SeqCst);
    fresh.rebuild().await.expect("cold rebuild");

    assert_eq!(
        h.store.writes.load(Ordering::SeqCst),
        0,
        "a cold rebuild writes nothing to the lane"
    );
    assert_eq!(snapshot(&fresh_db), written, "same pages, blocks and links");
}

/// What the rebuild must keep: a page that fails the mandatory-meta-skill guard is still refused by
/// the write path (the guard sits in the shared code, not in the lane write).
#[tokio::test]
async fn update_page_still_writes_the_lane() {
    let h = harness();
    h.indexer.update_page(A.0, A.1).await.expect("write");
    assert_eq!(h.store.writes.load(Ordering::SeqCst), 1);
    let key = Key::new(TENANT, A.0.to_owned()).unwrap();
    assert_eq!(
        h.store.read(&key).await.unwrap().as_ref(),
        A.1.as_bytes(),
        "the canonical markdown is in the lane"
    );
}
