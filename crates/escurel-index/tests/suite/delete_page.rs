//! #300 — `Indexer::delete_page` soft-delete (archive) semantics, against the
//! real components: a real DuckDB file + a real `FsStore`, no mocks.
//!
//! The load-bearing property the MCP-level test can't reach: a deleted page's
//! retraction SURVIVES a from-scratch rebuild. `delete_page` stamps the
//! canonical markdown `archived: true` and drops the index rows; `rebuild`
//! re-indexes the lane but skips archived pages, so the page stays retracted.

use std::sync::Arc;

use bytes::Bytes;
use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, Key, LaneStore};
use tempfile::TempDir;

const TENANT: &str = "acme";

const SKILL: (&str, &str) = (
    "markdown/skills/customer.md",
    "---\ntype: skill\nid: customer\ndescription: A buying entity.\n---\n# customer\n",
);
const ACME: (&str, &str) = (
    "markdown/instances/customer/acme-corp.md",
    "---\ntype: instance\nskill: customer\nid: acme-corp\n---\n# Acme\n\nSee [[customer::globex-llc]].\n",
);
const GLOBEX: (&str, &str) = (
    "markdown/instances/customer/globex-llc.md",
    "---\ntype: instance\nskill: customer\nid: globex-llc\n---\n# Globex\n",
);
const ACME_PAGE: &str = "markdown/instances/customer/acme-corp.md";

struct Harness {
    store: Arc<dyn LaneStore>,
    indexer: Indexer,
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
    let indexer = Indexer::new(Arc::clone(&store), embedder, conn, TENANT).unwrap();
    Harness {
        store,
        indexer,
        _store_dir: store_dir,
        _db_dir: db_dir,
    }
}

async fn seed(h: &Harness) {
    for (path, body) in [SKILL, ACME, GLOBEX] {
        let key = Key::new(TENANT, path.to_owned()).unwrap();
        h.store
            .write(&key, Bytes::from_static(body.as_bytes()))
            .await
            .unwrap();
        h.indexer.update_page(path, body).await.unwrap();
    }
}

/// Sorted `page_id`s of the customer instances the index currently serves.
async fn customer_pages(indexer: &Indexer) -> Vec<String> {
    let mut ids: Vec<String> = indexer
        .list_instances("customer", None, None, None, None, None)
        .await
        .unwrap()
        .into_iter()
        .map(|i| i.page_id)
        .collect();
    ids.sort();
    ids
}

const GLOBEX_PAGE: &str = "markdown/instances/customer/globex-llc.md";

#[tokio::test]
async fn delete_page_archives_retracts_and_survives_rebuild() {
    let h = fresh();
    seed(&h).await;
    assert_eq!(customer_pages(&h.indexer).await, [ACME_PAGE, GLOBEX_PAGE]);

    // Delete acme-corp.
    assert!(
        h.indexer.delete_page(ACME_PAGE).await.unwrap(),
        "delete_page reports a page was archived"
    );

    // Retracted from the index immediately.
    assert_eq!(
        customer_pages(&h.indexer).await,
        [GLOBEX_PAGE],
        "acme-corp is retracted from the index"
    );

    // The canonical markdown is retained, re-stamped archived.
    let key = Key::new(TENANT, ACME_PAGE.to_owned()).unwrap();
    let retained = h.indexer.read_page_markdown(ACME_PAGE).await.unwrap();
    let retained = retained.expect("archived markdown retained in the lane");
    let page = escurel_md::parse(&retained).expect("archived markdown still parses");
    assert_eq!(
        page.frontmatter.fields["archived"].as_bool(),
        Some(true),
        "the retained markdown carries archived: true"
    );
    // ...and it is still physically present on the store.
    assert!(h.store.read(&key).await.is_ok());

    // The retraction SURVIVES a from-scratch rebuild (the archived page is
    // skipped rather than re-indexed).
    h.indexer.rebuild().await.unwrap();
    assert_eq!(
        customer_pages(&h.indexer).await,
        [GLOBEX_PAGE],
        "acme-corp stays retracted across a rebuild"
    );
}

#[tokio::test]
async fn delete_page_missing_returns_false() {
    let h = fresh();
    seed(&h).await;
    assert!(
        !h.indexer
            .delete_page("markdown/instances/customer/nope.md")
            .await
            .unwrap(),
        "deleting a non-existent page reports false, not an error"
    );
}
