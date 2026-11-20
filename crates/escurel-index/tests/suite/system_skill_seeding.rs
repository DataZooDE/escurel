//! System skills are seeded once and namespaced (v2026.11.21).
//!
//! 1. **Exactly one copy.** On a Google Drive lane a name can be held by two files, and every later read
//!    of it fails ("is ambiguous: 2 files share that name"). heron-escurel got two identical
//!    `markdown/skills/agent.md` three seconds apart when v2026.11.17 seeded it: the seed wrote the page
//!    and then wrote it again through the ordinary write path. The seed now decides against the LANE and
//!    creates with `create_if_absent`. These tests count what a real `FsStore` is asked to write:
//!    repeated boots on fresh indexes, and two indexers seeding the same lane at once, produce one copy.
//! 2. **Namespaced ids.** escurel's skills are `escurel-agent`, `escurel-runtime`, `escurel-chat-channel`.
//!    A tenant's own `agent` skill and its instances are ordinary pages.
//! 3. **Migration.** A legacy `markdown/skills/agent.md` that escurel wrote (byte-equal to a shipped
//!    version) moves to `escurel-agent` with its instances; a tenant-authored one is never touched.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bytes::Bytes;
use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, Key, LaneStore, Result as StoreResult, StoreError, Version};
use tempfile::TempDir;
use url::Url;

const TENANT: &str = "acme";
const SYSTEM_PAGES: [&str; 3] = [
    "markdown/skills/escurel-agent.md",
    "markdown/skills/escurel-runtime.md",
    "markdown/skills/escurel-chat-channel.md",
];
const LEGACY_AGENT: &str = include_str!("../fixtures/legacy_agent_skill_v2026.11.20.md");

/// A real `FsStore` that records, per key, every write and every successful create. Nothing is faked.
struct CountingStore {
    inner: FsStore,
    stores: Mutex<HashMap<String, usize>>,
}

impl CountingStore {
    fn count(&self, key: &Key) {
        *self
            .stores
            .lock()
            .unwrap()
            .entry(key.path().to_owned())
            .or_default() += 1;
    }
    fn stored(&self, page_id: &str) -> usize {
        self.stores
            .lock()
            .unwrap()
            .get(page_id)
            .copied()
            .unwrap_or(0)
    }
}

#[async_trait]
impl LaneStore for CountingStore {
    async fn read(&self, key: &Key) -> StoreResult<Bytes> {
        self.inner.read(key).await
    }
    async fn write(&self, key: &Key, body: Bytes) -> StoreResult<Version> {
        self.count(key);
        self.inner.write(key, body).await
    }
    async fn create_if_absent(&self, key: &Key, body: Bytes) -> StoreResult<bool> {
        let created = self.inner.create_if_absent(key, body).await?;
        if created {
            self.count(key);
        }
        Ok(created)
    }
    async fn list(&self, prefix: &Key) -> StoreResult<Vec<Key>> {
        self.inner.list(prefix).await
    }
    async fn delete(&self, key: &Key) -> StoreResult<()> {
        self.inner.delete(key).await
    }
    fn url(&self, key: &Key) -> StoreResult<Url> {
        self.inner.url(key)
    }
}

struct Lane {
    store: Arc<CountingStore>,
    _dir: TempDir,
}

fn lane() -> Lane {
    let dir = TempDir::new().unwrap();
    Lane {
        store: Arc::new(CountingStore {
            inner: FsStore::new(dir.path().to_path_buf()),
            stores: Mutex::new(HashMap::new()),
        }),
        _dir: dir,
    }
}

/// One gateway: its own fresh DuckDB index over the shared lane.
fn gateway(lane: &Lane, dbs: &TempDir, name: &str) -> Indexer {
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(dbs.path().join(format!("{name}.duckdb"))).unwrap();
    Migrator::up(&conn).unwrap();
    Indexer::new(
        Arc::clone(&lane.store) as Arc<dyn LaneStore>,
        embedder,
        conn,
        TENANT,
    )
    .unwrap()
}

async fn put(lane: &Lane, page_id: &str, content: &str) {
    lane.store
        .inner
        .write(
            &Key::new(TENANT, page_id.to_owned()).unwrap(),
            Bytes::from(content.to_owned()),
        )
        .await
        .unwrap();
}

async fn read(lane: &Lane, page_id: &str) -> Option<String> {
    match lane
        .store
        .inner
        .read(&Key::new(TENANT, page_id.to_owned()).unwrap())
        .await
    {
        Ok(b) => Some(String::from_utf8(b.to_vec()).unwrap()),
        Err(StoreError::NotFound(_)) => None,
        Err(e) => panic!("{e}"),
    }
}

/// How many keys the lane lists under exactly this name.
async fn listed(lane: &Lane, page_id: &str) -> usize {
    lane.store
        .inner
        .list(&Key::new(TENANT, "markdown/".to_owned()).unwrap())
        .await
        .unwrap()
        .iter()
        .filter(|k| k.path() == page_id)
        .count()
}

#[tokio::test]
async fn repeated_boots_on_fresh_indexes_store_each_system_skill_once() {
    let lane = lane();
    let dbs = TempDir::new().unwrap();
    for boot in ["first", "second", "third"] {
        gateway(&lane, &dbs, boot)
            .ensure_system_skills()
            .await
            .unwrap();
    }
    for page in SYSTEM_PAGES {
        assert_eq!(
            lane.store.stored(page),
            1,
            "{page} was stored more than once"
        );
        assert_eq!(listed(&lane, page).await, 1, "{page}");
    }
    // The meta-skill too.
    assert_eq!(lane.store.stored("markdown/skills/escurel.md"), 1);
}

#[tokio::test]
async fn two_gateways_seeding_the_same_lane_at_once_store_one_copy() {
    for _ in 0..5 {
        let lane = lane();
        let dbs = TempDir::new().unwrap();
        let (a, b) = (gateway(&lane, &dbs, "a"), gateway(&lane, &dbs, "b"));
        let (ra, rb) = tokio::join!(a.ensure_system_skills(), b.ensure_system_skills());
        ra.unwrap();
        rb.unwrap();
        for page in SYSTEM_PAGES {
            assert_eq!(lane.store.stored(page), 1, "{page}");
        }
        // Both gateways serve it.
        for ix in [&a, &b] {
            assert!(
                ix.read_page_markdown(SYSTEM_PAGES[0])
                    .await
                    .unwrap()
                    .is_some()
            );
        }
    }
}

#[tokio::test]
async fn a_tenants_own_agent_skill_is_left_alone_and_escurels_moves_aside() {
    let lane = lane();
    let dbs = TempDir::new().unwrap();
    let portfolio =
        "---\nkind: skill\nid: agent\ndescription: A portfolio AI agent.\n---\n# agent\n";
    let instance = "---\nkind: instance\nskill: agent\nid: order-agent\nname: Order agent\n---\n# Order agent\n";
    put(&lane, "markdown/skills/agent.md", portfolio).await;
    put(&lane, "markdown/instances/agent/order-agent.md", instance).await;
    let ix = gateway(&lane, &dbs, "g");
    ix.ensure_system_skills().await.unwrap();
    assert_eq!(
        read(&lane, "markdown/skills/agent.md").await.as_deref(),
        Some(portfolio)
    );
    assert_eq!(
        read(&lane, "markdown/instances/agent/order-agent.md")
            .await
            .as_deref(),
        Some(instance)
    );
    assert!(
        read(&lane, "markdown/skills/escurel-agent.md")
            .await
            .is_some()
    );
    assert!(!escurel_index::system_skills::is_admin_only_path(
        "markdown/instances/agent/order-agent.md"
    ));
}

#[tokio::test]
async fn escurels_legacy_agent_skill_moves_to_its_namespaced_id_with_its_profiles() {
    let lane = lane();
    let dbs = TempDir::new().unwrap();
    let profile = "---\nkind: instance\nskill: agent\nid: scribe\nharness: echo\n\
                   skills: [\"[[skill::note]]\"]\n---\n# scribe\n";
    put(&lane, "markdown/skills/agent.md", LEGACY_AGENT).await;
    put(&lane, "markdown/instances/agent/scribe.md", profile).await;
    let ix = gateway(&lane, &dbs, "g");
    ix.ensure_system_skills().await.unwrap();
    assert_eq!(read(&lane, "markdown/skills/agent.md").await, None);
    assert_eq!(
        read(&lane, "markdown/instances/agent/scribe.md").await,
        None
    );
    let moved = read(&lane, "markdown/instances/escurel-agent/scribe.md")
        .await
        .expect("the profile moved");
    assert!(moved.contains("skill: escurel-agent\n"), "{moved}");
    assert!(
        moved.contains("harness: echo\n"),
        "the rest is kept: {moved}"
    );
    let page = ix
        .expand("markdown/instances/escurel-agent/scribe.md", None, None)
        .await
        .unwrap()
        .expect("indexed");
    assert_eq!(page.page.skill, "escurel-agent");
    assert!(
        ix.expand("markdown/skills/agent.md", None, None)
            .await
            .unwrap()
            .is_none()
    );
    // A second boot changes nothing.
    gateway(&lane, &dbs, "g2")
        .ensure_system_skills()
        .await
        .unwrap();
    assert_eq!(
        read(&lane, "markdown/instances/escurel-agent/scribe.md")
            .await
            .as_deref(),
        Some(moved.as_str())
    );
    assert_eq!(lane.store.stored("markdown/skills/escurel-agent.md"), 1);
}
