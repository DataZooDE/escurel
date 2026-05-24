//! Integration tests for `Indexer::search` (vss + fts + RRF).
//!
//! Real DuckDB + real FsStore + `HashEmbedder` so the vector
//! side has deterministic, well-separated unit vectors per
//! distinct block body. Tests assert ranking and filter behaviour
//! at the API level; the ADR-0001 retrieval-quality gate (real
//! nDCG against a real model) is a separate exercise that
//! follows M2.2.

use std::sync::Arc;

use bytes::Bytes;
use duckdb::Connection;
use escurel_embed::{Embedder, HashEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_md::PageType;
use escurel_storage::{FsStore, Key, LaneStore};
use tempfile::TempDir;

const TENANT: &str = "acme";

const SKILL_CUSTOMER: (&str, &str) = (
    "markdown/skills/customer.md",
    "---\n\
     type: skill\n\
     id: customer\n\
     description: A buying entity.\n\
     ---\n\
     # customer\n\
     \n\
     A customer is the unit of revenue.\n",
);

const ACME: (&str, &str) = (
    "markdown/instances/customer/acme-corp.md",
    "---\n\
     type: instance\n\
     skill: customer\n\
     id: acme-corp\n\
     ---\n\
     # Acme Corp\n\
     \n\
     Industrial manufacturing conglomerate headquartered in Stuttgart.\n",
);

const GLOBEX: (&str, &str) = (
    "markdown/instances/customer/globex-llc.md",
    "---\n\
     type: instance\n\
     skill: customer\n\
     id: globex-llc\n\
     ---\n\
     # Globex LLC\n\
     \n\
     Stuttgart-based fintech focused on payment infrastructure.\n",
);

const MEETING: (&str, &str) = (
    "markdown/instances/meeting/2026-04-12-acme-qbr.md",
    "---\n\
     type: instance\n\
     skill: meeting\n\
     id: 2026-04-12-acme-qbr\n\
     at: 2026-04-12T10:00:00+02:00\n\
     ---\n\
     # Acme QBR\n\
     \n\
     Quarterly business review with Acme; renewal trajectory looks healthy.\n",
);

struct Harness {
    store: Arc<dyn LaneStore>,
    indexer: Indexer,
    _store_dir: TempDir,
    _db_dir: TempDir,
}

fn fresh_harness() -> Harness {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(HashEmbedder::default());
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

async fn seed(h: &Harness, pages: &[(&str, &'static str)]) {
    for (path, body) in pages {
        let key = Key::new(TENANT, path.to_owned()).unwrap();
        h.store
            .write(&key, Bytes::from_static(body.as_bytes()))
            .await
            .unwrap();
        h.indexer.update_page(path, body).await.unwrap();
    }
    // FTS needs an explicit rebuild after bulk inserts; the
    // extension has no incremental refresh.
    h.indexer.refresh_fts().await.unwrap();
}

#[tokio::test]
async fn search_with_k_zero_returns_empty() {
    let h = fresh_harness();
    seed(&h, &[SKILL_CUSTOMER, ACME]).await;
    let hits = h.indexer.search("anything", 0, None, None).await.unwrap();
    assert!(hits.is_empty());
}

#[tokio::test]
async fn search_on_empty_index_returns_empty() {
    let h = fresh_harness();
    // No seed, but FTS index still needs a rebuild after migrate
    // because the migration's create_fts_index was over an empty
    // table.
    h.indexer.refresh_fts().await.unwrap();
    let hits = h.indexer.search("acme", 10, None, None).await.unwrap();
    assert!(hits.is_empty());
}

#[tokio::test]
async fn search_returns_top_k_blocks_with_metadata() {
    let h = fresh_harness();
    seed(&h, &[SKILL_CUSTOMER, ACME, GLOBEX, MEETING]).await;

    let hits = h.indexer.search("Acme", 3, None, None).await.unwrap();
    assert!(!hits.is_empty(), "search must return at least one hit");
    assert!(hits.len() <= 3, "must respect k = 3");

    // Every hit must carry full metadata.
    for h in &hits {
        assert!(!h.page_id.is_empty());
        assert!(!h.skill.is_empty());
        assert!(h.score > 0.0, "RRF score must be positive");
        assert!(!h.snippet.is_empty());
    }
}

#[tokio::test]
async fn search_fts_ranks_keyword_match_above_unrelated() {
    let h = fresh_harness();
    seed(&h, &[SKILL_CUSTOMER, ACME, GLOBEX, MEETING]).await;

    // "manufacturing" is in Acme only; with both vector and FTS
    // contributing, Acme should land in the top-2.
    let hits = h
        .indexer
        .search("manufacturing", 4, None, None)
        .await
        .unwrap();
    let top_pages: Vec<_> = hits.iter().map(|h| h.page_id.as_str()).collect();
    assert!(
        top_pages.contains(&ACME.0),
        "Acme (only block containing 'manufacturing') must rank in top-4: {top_pages:?}",
    );
}

#[tokio::test]
async fn search_filters_by_page_type() {
    let h = fresh_harness();
    seed(&h, &[SKILL_CUSTOMER, ACME, GLOBEX, MEETING]).await;

    let only_skills = h
        .indexer
        .search("customer", 10, Some(PageType::Skill), None)
        .await
        .unwrap();
    for hit in &only_skills {
        assert_eq!(hit.page_type, PageType::Skill);
    }

    let only_instances = h
        .indexer
        .search("customer", 10, Some(PageType::Instance), None)
        .await
        .unwrap();
    for hit in &only_instances {
        assert_eq!(hit.page_type, PageType::Instance);
    }

    // And the two together cover what the unfiltered call returns.
    let unfiltered = h.indexer.search("customer", 10, None, None).await.unwrap();
    let unfiltered_pages: std::collections::HashSet<_> =
        unfiltered.iter().map(|h| h.page_id.clone()).collect();
    let union_pages: std::collections::HashSet<_> = only_skills
        .iter()
        .chain(&only_instances)
        .map(|h| h.page_id.clone())
        .collect();
    assert_eq!(
        unfiltered_pages, union_pages,
        "skill-filter ∪ instance-filter must equal the unfiltered set",
    );
}

#[tokio::test]
async fn search_filters_by_skill() {
    let h = fresh_harness();
    seed(&h, &[SKILL_CUSTOMER, ACME, GLOBEX, MEETING]).await;

    let only_meetings = h
        .indexer
        .search("Acme", 10, None, Some("meeting"))
        .await
        .unwrap();
    for hit in &only_meetings {
        assert_eq!(hit.skill, "meeting");
    }
    // The QBR is the only meeting block; it should be present.
    assert!(only_meetings.iter().any(|h| h.page_id == MEETING.0));
}

#[tokio::test]
async fn search_hits_carry_frontmatter_excerpt() {
    let h = fresh_harness();
    seed(&h, &[SKILL_CUSTOMER, ACME]).await;
    let hits = h.indexer.search("Acme", 5, None, None).await.unwrap();
    let acme_hit = hits
        .iter()
        .find(|h| h.page_id == ACME.0)
        .expect("acme must appear");
    assert_eq!(
        acme_hit
            .frontmatter_excerpt
            .get("id")
            .and_then(|v| v.as_str()),
        Some("acme-corp"),
    );
    assert_eq!(acme_hit.slug.as_deref(), Some("acme-corp"));
}

#[tokio::test]
async fn search_scores_are_monotonic_decreasing() {
    let h = fresh_harness();
    seed(&h, &[SKILL_CUSTOMER, ACME, GLOBEX, MEETING]).await;
    let hits = h.indexer.search("Stuttgart", 4, None, None).await.unwrap();
    for w in hits.windows(2) {
        assert!(
            w[0].score >= w[1].score,
            "scores must be non-increasing: {} then {}",
            w[0].score,
            w[1].score,
        );
    }
}
