//! A boot on a fresh data directory (a new pod, a wiped volume) scans every lane page and rebuilds the
//! whole index from the lane store. Over Google Drive that is the longest silent phase of a boot: the
//! heron-escurel cold boot logged nothing for 19-21 minutes (rehearsal #982) because only the lake attach
//! and adopt were wrapped in a progress ticker, not this step. This pins that the index open reports it.
//!
//! The lane store here is a real `FsStore` behind a wrapper that pays a fixed delay per read and list,
//! the shape of a slow remote filesystem; the DuckDB index, the migrations and the rebuild are the real
//! ones.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::snapshot::SingleFileStore;
use escurel_server::boot_progress::open_index_with_progress;
use escurel_storage::{FsStore, Key, LaneStore, StoreError, Version};
use tempfile::TempDir;
use url::Url;

/// A lane store whose every read and list takes `delay`.
struct SlowStore {
    inner: FsStore,
    delay: Duration,
}

#[async_trait]
impl LaneStore for SlowStore {
    async fn read(&self, key: &Key) -> Result<Bytes, StoreError> {
        tokio::time::sleep(self.delay).await;
        self.inner.read(key).await
    }
    async fn write(&self, key: &Key, body: Bytes) -> Result<Version, StoreError> {
        self.inner.write(key, body).await
    }
    async fn list(&self, prefix: &Key) -> Result<Vec<Key>, StoreError> {
        tokio::time::sleep(self.delay).await;
        self.inner.list(prefix).await
    }
    async fn delete(&self, key: &Key) -> Result<(), StoreError> {
        self.inner.delete(key).await
    }
    fn url(&self, key: &Key) -> Result<Url, StoreError> {
        self.inner.url(key)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fresh_index_open_over_a_slow_lane_reports_that_it_is_still_running() {
    let lane = TempDir::new().unwrap();
    let data = TempDir::new().unwrap();
    // A skill and a dozen instances, written through the plain store.
    let plain = FsStore::new(lane.path().to_path_buf());
    let put = |path: String, text: String| {
        let plain = &plain;
        async move {
            plain
                .write(&Key::new("acme", path).unwrap(), Bytes::from(text))
                .await
                .unwrap();
        }
    };
    put(
        "skills/note.md".to_owned(),
        "---\nkind: skill\nid: note\ndescription: d\n---\n# note\n".to_owned(),
    )
    .await;
    for n in 0..12 {
        put(
            format!("instances/note/n{n}.md"),
            format!("---\nkind: instance\nskill: note\nid: n{n}\n---\n# Note {n}\n"),
        )
        .await;
    }

    let store = SingleFileStore {
        tenant_dir: data.path().join("tenants").join("acme"),
        rebuild_on_boot: false,
        store: Arc::new(SlowStore {
            inner: FsStore::new(lane.path().to_path_buf()),
            delay: Duration::from_millis(80),
        }),
        embedder: Arc::new(ZeroEmbedder::default()) as Arc<dyn Embedder>,
        tenant: "acme".to_owned(),
        contextualize: Default::default(),
        attach_retrieval: None,
        seed_dir: None,
    };

    let ticks = Arc::new(AtomicU64::new(0));
    let seen = Arc::clone(&ticks);
    let opened = open_index_with_progress(&store, Duration::from_millis(100), move |_| {
        seen.fetch_add(1, Ordering::SeqCst);
    })
    .await
    .expect("the index opens");

    assert!(
        ticks.load(Ordering::SeqCst) >= 3,
        "a fresh open that spent over half a second in lane reads must have reported progress at least \
         three times at a 100 ms interval; it reported {} times",
        ticks.load(Ordering::SeqCst)
    );
    drop(opened);
}
