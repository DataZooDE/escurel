//! Integration test for [`IndexerCitationLookup`], the production
//! [`escurel_crdt::reconciler::CitationLookup`] impl backed by the
//! `links` table. Real DuckDB, real schema migrator, no mocks: the
//! `links` rows are populated by [`Indexer::update_page`] from real
//! wikilink-bearing markdown, so the test exercises the same
//! ingestion path production uses.

use std::sync::Arc;

use duckdb::Connection;
use escurel_crdt::CitationLookup;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, IndexerCitationLookup, Migrator};
use escurel_storage::{FsStore, LaneStore};
use tempfile::TempDir;

const TENANT: &str = "acme";

struct Harness {
    indexer: Arc<Indexer>,
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
    let indexer = Arc::new(Indexer::new(store, embedder, conn, TENANT).unwrap());

    Harness {
        indexer,
        _store_dir: store_dir,
        _db_dir: db_dir,
    }
}

const SKILL_CUSTOMER: &str = "---\n\
     type: skill\n\
     id: customer\n\
     description: A buying entity.\n\
     required_frontmatter: []\n\
     optional_frontmatter: []\n\
     ---\n\
     # customer\n";

const INSTANCE_ACME: &str = "---\n\
     type: instance\n\
     skill: customer\n\
     id: acme-corp\n\
     ---\n\
     # Acme Corp\n";

/// A note instance that cites `[[customer::acme-corp]]` so the
/// `links` table grows a row with `dst_page = 'acme-corp'`.
const INSTANCE_NOTE_CITING_ACME: &str = "---\n\
     type: instance\n\
     skill: customer\n\
     id: acme-meeting-note\n\
     ---\n\
     # Notes\n\
     \n\
     Met with [[customer::acme-corp]] yesterday.\n";

#[tokio::test]
async fn is_cited_returns_false_on_empty_links_table() {
    let h = fresh_harness();
    let lookup = IndexerCitationLookup::new(h.indexer.clone());

    let cited = lookup.is_cited(TENANT, "anything").await.unwrap();
    assert!(!cited);
}

#[tokio::test]
async fn is_cited_returns_true_when_another_page_wikilinks_in() {
    let h = fresh_harness();
    // Seed the wikilink target and a citing page.
    h.indexer
        .update_page("markdown/skills/customer.md", SKILL_CUSTOMER)
        .await
        .unwrap();
    h.indexer
        .update_page("markdown/instances/customer/acme-corp.md", INSTANCE_ACME)
        .await
        .unwrap();
    h.indexer
        .update_page(
            "markdown/instances/customer/acme-meeting-note.md",
            INSTANCE_NOTE_CITING_ACME,
        )
        .await
        .unwrap();

    let lookup = IndexerCitationLookup::new(h.indexer.clone());

    // Wikilinks store the bare id in `links.dst_page`; that is the
    // shape `is_cited` queries for.
    assert!(lookup.is_cited(TENANT, "acme-corp").await.unwrap());
    // Pages no one links into stay uncited.
    assert!(!lookup.is_cited(TENANT, "globex-llc").await.unwrap());
}
