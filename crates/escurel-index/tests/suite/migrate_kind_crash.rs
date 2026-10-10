//! `migrate_kind --apply` must survive being interrupted. The lane has no transaction and the
//! index is rebuilt only at the END, so a failure or a kill -9 part-way used to leave a lane with
//! no legacy pages: the next boot found nothing to quarantine, ran no rebuild, and served an index
//! that was never derived from the migrated lane, silently.
//!
//! Real DuckDB, a real file store wrapped to fail on the Nth write (a store fault, not a stub: every
//! other call hits the real files), and a "reboot" = a new `Indexer` over the same lane.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use bytes::Bytes;
use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, Key, LaneStore, StoreError, Version};
use tempfile::TempDir;
use url::Url;

const TENANT: &str = "acme";

/// A real `FsStore` that starts failing every write after `allow` successful ones.
struct FaultStore {
    inner: FsStore,
    allow: AtomicUsize,
}

#[async_trait]
impl LaneStore for FaultStore {
    async fn read(&self, key: &Key) -> escurel_storage::Result<Bytes> {
        self.inner.read(key).await
    }
    async fn write(&self, key: &Key, body: Bytes) -> escurel_storage::Result<Version> {
        // A compare_exchange loop, not `fetch_update`: newer rustc deprecates that name (`try_update`),
        // and the older pinned toolchain has no `try_update`.
        let granted = loop {
            let n = self.allow.load(Ordering::SeqCst);
            if n == 0 {
                break false;
            }
            if self
                .allow
                .compare_exchange(n, n - 1, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                break true;
            }
        };
        if !granted {
            return Err(StoreError::Io(std::io::Error::other(
                "injected store fault",
            )));
        }
        self.inner.write(key, body).await
    }
    async fn list(&self, prefix: &Key) -> escurel_storage::Result<Vec<Key>> {
        self.inner.list(prefix).await
    }
    async fn delete(&self, key: &Key) -> escurel_storage::Result<()> {
        self.inner.delete(key).await
    }
    fn url(&self, key: &Key) -> escurel_storage::Result<Url> {
        self.inner.url(key)
    }
}

struct Rig {
    store: Arc<FaultStore>,
    dir: TempDir,
}

fn rig() -> Rig {
    let dir = TempDir::new().unwrap();
    Rig {
        store: Arc::new(FaultStore {
            inner: FsStore::new(dir.path().join("lane")),
            allow: AtomicUsize::new(usize::MAX),
        }),
        dir,
    }
}

fn indexer(rig: &Rig, db: &str) -> Indexer {
    let store: Arc<dyn LaneStore> = rig.store.clone();
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(rig.dir.path().join(db)).unwrap();
    Migrator::up(&conn).unwrap();
    Indexer::new(store, embedder, conn, TENANT).unwrap()
}

fn key(path: &str) -> Key {
    Key::new(TENANT, path.to_owned()).unwrap()
}

async fn put(rig: &Rig, path: &str, md: &str) {
    rig.store
        .inner
        .write(&key(path), Bytes::from(md.to_owned()))
        .await
        .unwrap();
}

fn legacy_page(id: usize) -> (String, String) {
    (
        format!("markdown/instances/note/n{id}.md"),
        format!("---\ntype: instance\nskill: note\nid: n{id}\n---\n# n{id}\n"),
    )
}

const SKILL: &str = "markdown/skills/note.md";
const SKILL_LEGACY: &str = "---\ntype: skill\nid: note\n---\n# note\n";

#[tokio::test]
async fn a_migration_interrupted_after_the_first_page_does_not_come_back_serving() {
    let rig = rig();
    put(&rig, SKILL, SKILL_LEGACY).await;
    for i in 0..3 {
        let (p, md) = legacy_page(i);
        put(&rig, &p, &md).await;
    }
    let first_boot = indexer(&rig, "a.duckdb");
    assert!(first_boot.quarantine_legacy_kind_pages().await.unwrap());

    // The store dies after the FIRST page was rewritten (the marker is write #1 when it exists).
    rig.store.allow.store(2, Ordering::SeqCst);
    first_boot
        .migrate_kind(true)
        .await
        .expect_err("the injected store fault must surface");
    rig.store.allow.store(usize::MAX, Ordering::SeqCst);

    // Reboot: a new indexer over the same lane. At least one page IS migrated now, but others are
    // not, and the index was never rebuilt: the tenant must NOT come back as healthy.
    let rebooted = indexer(&rig, "b.duckdb");
    assert!(
        rebooted.quarantine_legacy_kind_pages().await.unwrap(),
        "an interrupted migration must keep the tenant quarantined across a reboot"
    );

    // And re-running it finishes the job and lifts the quarantine for good.
    let report = rebooted.migrate_kind(true).await.unwrap();
    assert!(!report.tenant_quarantined, "{report:?}");
    let after = indexer(&rig, "c.duckdb");
    assert!(
        !after.quarantine_legacy_kind_pages().await.unwrap(),
        "a completed migration clears its marker"
    );
}

#[tokio::test]
async fn an_interruption_after_every_page_was_rewritten_still_quarantines_on_reboot() {
    // The nastiest case: the lane is FULLY migrated (nothing legacy left to scan for) but the index
    // rebuild never ran, so the boot scan alone would find nothing.
    let rig = rig();
    put(&rig, SKILL, SKILL_LEGACY).await;
    let (p, md) = legacy_page(0);
    put(&rig, &p, &md).await;
    let first_boot = indexer(&rig, "a.duckdb");
    assert!(first_boot.quarantine_legacy_kind_pages().await.unwrap());

    // Marker + both pages = 3 writes allowed; the 4th (anything after the pages) fails.
    rig.store.allow.store(3, Ordering::SeqCst);
    let _ = first_boot.migrate_kind(true).await;
    rig.store.allow.store(usize::MAX, Ordering::SeqCst);

    for path in [SKILL, p.as_str()] {
        let body = rig.store.inner.read(&key(path)).await.unwrap();
        assert!(
            std::str::from_utf8(&body).unwrap().contains("kind:"),
            "{path} was rewritten before the interruption"
        );
    }
    let rebooted = indexer(&rig, "b.duckdb");
    assert!(
        rebooted.quarantine_legacy_kind_pages().await.unwrap(),
        "a fully rewritten lane with no rebuilt index must still be quarantined"
    );
}

#[tokio::test]
async fn a_page_the_rebuild_cannot_parse_stops_the_rebuild_before_it_truncates_the_index() {
    // A page with broken YAML does not parse. The rebuild used to find out AFTER it had truncated the
    // index, leaving it partly rebuilt. (A BOM or CRLF page DOES parse now: see escurel-md.)
    let rig = rig();
    let idx = indexer(&rig, "a.duckdb");
    idx.update_page(SKILL, "---\nkind: skill\nid: note\n---\n# note\n")
        .await
        .unwrap();
    idx.update_page(
        "markdown/instances/note/ok.md",
        "---\nkind: instance\nskill: note\nid: ok\n---\n# ok\n",
    )
    .await
    .unwrap();
    let before = idx
        .list_instances("note", None, Some(100), None, None, None)
        .await
        .unwrap()
        .len();
    assert_eq!(before, 1);

    put(
        &rig,
        "markdown/instances/note/bom.md",
        "---\nkind: instance\nskill: [unclosed\nid: bom\n---\n# bom\n",
    )
    .await;

    let err = idx
        .rebuild()
        .await
        .expect_err("an unparsable page refuses the rebuild");
    let msg = err.to_string();
    assert!(msg.contains("bom.md"), "the page is named: {msg}");

    let after = idx
        .list_instances("note", None, Some(100), None, None, None)
        .await
        .unwrap()
        .len();
    assert_eq!(
        after, before,
        "a refused rebuild leaves the index exactly as it was"
    );
}

#[tokio::test]
async fn bom_and_crlf_legacy_pages_migrate_and_an_unparsable_one_keeps_the_tenant_quarantined() {
    let rig = rig();
    let (p, md) = legacy_page(0);
    put(&rig, SKILL, SKILL_LEGACY).await;
    put(&rig, &p, &md).await;
    put(
        &rig,
        "markdown/instances/note/bom.md",
        "\u{feff}---\ntype: instance\nskill: note\nid: bom\n---\n# bom\n",
    )
    .await;
    put(
        &rig,
        "markdown/instances/note/crlf.md",
        "---\r\ntype: instance\r\nskill: note\r\nid: crlf\r\n---\r\n# crlf\r\n",
    )
    .await;
    put(
        &rig,
        "markdown/instances/note/broken.md",
        "---\ntype: instance\nskill: [unclosed\nid: broken\n---\n# broken\n",
    )
    .await;
    let boot = indexer(&rig, "a.duckdb");
    assert!(boot.quarantine_legacy_kind_pages().await.unwrap());

    // BOM and CRLF pages are ordinary files: the dry run migrates them. Only the page that cannot be
    // parsed stops the apply at the rebuild (named), and the tenant is NOT lifted.
    let dry = boot.migrate_kind(false).await.unwrap();
    for name in ["bom.md", "crlf.md"] {
        assert!(
            dry.pages_to_migrate.iter().any(|p| p.ends_with(name)),
            "{dry:?}"
        );
    }
    let err = boot
        .migrate_kind(true)
        .await
        .expect_err("the rebuild must refuse");
    assert!(err.to_string().contains("broken.md"), "{err}");

    let rebooted = indexer(&rig, "b.duckdb");
    assert!(
        rebooted.quarantine_legacy_kind_pages().await.unwrap(),
        "still quarantined: the marker stays until the rebuild succeeds"
    );
}

/// `FsStore` publishes a page by writing `<page>.md.tmp` and renaming it. A kill -9 between the two
/// leaves the temp file behind. The literal-SIGKILL test in `escurel-server` found that such an orphan
/// was listed as if it were a page: the migration read it (it vanished when the SIBLING page's rewrite
/// renamed over it, so the read failed `not found`), and a rebuild tried to parse it.
async fn lane_with_orphans(rig: &Rig) {
    put(rig, SKILL, SKILL_LEGACY).await;
    for i in 0..3 {
        let (p, md) = legacy_page(i);
        put(rig, &p, &md).await;
    }
    let dir = rig
        .dir
        .path()
        .join("lane/tenants")
        .join(TENANT)
        .join("markdown/instances/note");
    // A truncated half-write (cannot parse) and a complete copy of a sibling (would be a duplicate).
    std::fs::write(dir.join("n1.md.tmp"), "---\ntype: inst").unwrap();
    std::fs::write(dir.join("n2.md.tmp"), legacy_page(2).1).unwrap();
}

#[tokio::test]
async fn an_orphaned_atomic_write_temp_file_is_not_a_page_and_does_not_break_the_migration() {
    let rig = rig();
    lane_with_orphans(&rig).await;
    let ix = indexer(&rig, "a.duckdb");
    assert!(ix.quarantine_legacy_kind_pages().await.unwrap());

    let dry = ix.migrate_kind(false).await.unwrap();
    assert!(
        dry.pages_to_migrate.iter().all(|p| !p.ends_with(".tmp")),
        "a temp file is not a page: {dry:?}"
    );

    let report = ix
        .migrate_kind(true)
        .await
        .expect("an orphaned temp file must not fail the migration");
    assert!(!report.tenant_quarantined, "{report:?}");
    assert!(
        report.not_a_page_kind.iter().all(|p| !p.ends_with(".tmp")),
        "{report:?}"
    );

    // Booted again over the same lane: healthy, and the index holds the four real pages only.
    let after = indexer(&rig, "b.duckdb");
    assert!(!after.quarantine_legacy_kind_pages().await.unwrap());
    after.rebuild().await.expect("a rebuild ignores orphans");
    for orphan in [
        "markdown/instances/note/n1.md.tmp",
        "markdown/instances/note/n2.md.tmp",
    ] {
        assert!(
            after.read_page_markdown(orphan).await.unwrap().is_none(),
            "{orphan} was indexed as a page"
        );
    }
    let instances = after
        .list_instances("note", None, None, None, None, None)
        .await
        .unwrap();
    assert_eq!(instances.len(), 3, "{instances:?}");
}
