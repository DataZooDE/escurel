//! After the hard cut a tenant that still holds pages with the removed `type:` page-kind key is
//! REFUSED, not served degraded: rebuild and boot collect ALL legacy pages in one pass and fail
//! once, naming the migration command, and a refused rebuild does not touch the index.
//!
//! Real DuckDB + real file store, no mocks.

use std::sync::Arc;

use bytes::Bytes;
use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::snapshot::{IndexStore, SingleFileStore};
use escurel_index::{Indexer, IndexerError, Migrator};
use escurel_storage::{FsStore, Key, LaneStore};
use tempfile::TempDir;

const TENANT: &str = "acme";

struct Harness {
    store: Arc<dyn LaneStore>,
    indexer: Indexer,
    side: Connection,
    _store_dir: TempDir,
    _db_dir: TempDir,
}

fn fresh() -> Harness {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let side = conn.try_clone().unwrap();
    let indexer = Indexer::new(Arc::clone(&store), embedder, conn, TENANT).unwrap();
    Harness {
        store,
        indexer,
        side,
        _store_dir: store_dir,
        _db_dir: db_dir,
    }
}

async fn lane_put(h: &Harness, path: &str, md: &str) {
    let key = Key::new(TENANT, path.to_owned()).unwrap();
    h.store
        .write(&key, Bytes::from(md.to_owned()))
        .await
        .unwrap();
}

fn legacy(id: usize) -> (String, String) {
    (
        format!("markdown/instances/note/legacy-{id:02}.md"),
        format!("---\ntype: instance\nskill: note\nid: legacy-{id:02}\n---\n# {id}\n"),
    )
}

fn count_pages(h: &Harness) -> i64 {
    h.side
        .query_row("SELECT count(*) FROM pages", [], |r| r.get(0))
        .unwrap()
}

#[tokio::test]
async fn rebuild_lists_every_legacy_page_fails_once_and_leaves_the_index_alone() {
    let h = fresh();
    // A healthy page that IS indexed; a rebuild that proceeded would truncate and re-derive.
    h.indexer
        .update_page(
            "markdown/skills/note.md",
            "---\nkind: skill\nid: note\n---\n# note\n",
        )
        .await
        .unwrap();
    let before = count_pages(&h);
    // 25 legacy pages sitting in the lane: more than the message lists, all in the error.
    for i in 0..25 {
        let (path, md) = legacy(i);
        lane_put(&h, &path, &md).await;
    }

    let err = h
        .indexer
        .rebuild()
        .await
        .expect_err("a legacy tenant must not rebuild");

    let IndexerError::LegacyKindPages { tenant, pages } = &err else {
        panic!("expected LegacyKindPages, got {err:?}");
    };
    assert_eq!(tenant, TENANT);
    assert_eq!(
        pages.len(),
        25,
        "ALL legacy pages are collected, not just the first"
    );
    let msg = err.to_string();
    assert!(
        msg.contains("escurel admin migrate-kind --tenant acme"),
        "{msg}"
    );
    assert!(
        msg.contains("and 5 more"),
        "the message lists 20 and counts the rest: {msg}"
    );
    assert_eq!(
        count_pages(&h),
        before,
        "a refused rebuild must not truncate the index"
    );
}

#[tokio::test]
async fn signed_pack_pages_are_named_as_needing_a_re_export() {
    let h = fresh();
    lane_put(
        &h,
        "markdown/base/pack/skills/shared.md",
        "---\ntype: skill\nid: shared\n---\n# shared\n",
    )
    .await;
    let err = h.indexer.rebuild().await.expect_err("refused");
    let msg = err.to_string();
    assert!(msg.contains("re-export and re-sign"), "{msg}");
}

#[tokio::test]
async fn update_page_rejects_the_removed_key_with_the_named_error() {
    let h = fresh();
    let err = h
        .indexer
        .update_page(
            "markdown/skills/x.md",
            "---\ntype: skill\nid: x\n---\n# x\n",
        )
        .await
        .expect_err("a write with the removed key is refused");
    assert!(
        err.to_string().contains("escurel admin migrate-kind"),
        "{err}"
    );
}

#[tokio::test]
async fn validate_reports_frontmatter_type_removed_with_the_tool_as_the_suggestion() {
    let h = fresh();
    let issues = h
        .indexer
        .validate(None, "---\ntype: instance\nskill: note\nid: n1\n---\n# n\n")
        .await
        .unwrap();
    let issue = issues
        .iter()
        .find(|i| i.code == "frontmatter_type_removed")
        .unwrap_or_else(|| panic!("expected frontmatter_type_removed, got {issues:?}"));
    assert_eq!(issue.location, "frontmatter.type");
    assert!(
        issue
            .suggestion
            .as_deref()
            .unwrap_or_default()
            .contains("escurel admin migrate-kind"),
        "{issue:?}"
    );
}

#[tokio::test]
async fn boot_refuses_a_tenant_with_legacy_pages_fresh_or_not() {
    let store_dir = TempDir::new().unwrap();
    let tenant_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    for i in 0..3 {
        let (path, md) = legacy(i);
        let key = Key::new(TENANT, path).unwrap();
        store.write(&key, Bytes::from(md)).await.unwrap();
    }
    let opener = |rebuild_on_boot: bool| SingleFileStore {
        tenant_dir: tenant_dir.path().to_path_buf(),
        rebuild_on_boot,
        store: Arc::clone(&store),
        embedder: Arc::new(ZeroEmbedder::default()),
        tenant: TENANT.to_owned(),
        contextualize: Default::default(),
        attach_retrieval: None,
        seed_dir: None,
    };

    // Fresh database: the cattle-node-loss rebuild refuses.
    let err = opener(false)
        .open()
        .await
        .err()
        .expect("fresh boot refuses");
    assert!(
        err.to_string().contains("escurel admin migrate-kind"),
        "{err}"
    );

    // The refused fresh boot left the database it created behind, so this is now an EXISTING
    // index (the derived index survived) with legacy pages still in the lane: the lane scan refuses.
    let err = opener(false).open().await.err().expect("warm boot refuses");
    assert!(
        err.to_string().contains("escurel admin migrate-kind"),
        "{err}"
    );
}

#[tokio::test]
async fn a_clean_tenant_boots_and_rebuilds_normally() {
    let h = fresh();
    lane_put(
        &h,
        "markdown/skills/note.md",
        "---\nkind: skill\nid: note\n---\n# note\n",
    )
    .await;
    assert!(h.indexer.legacy_kind_pages().await.unwrap().is_empty());
    h.indexer
        .rebuild()
        .await
        .expect("a migrated tenant rebuilds");
}
