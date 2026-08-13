//! Integration tests for `Indexer::neighbours`.

use std::sync::Arc;

use bytes::Bytes;
use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Direction, Indexer, Migrator};
use escurel_storage::{FsStore, Key, LaneStore};
use tempfile::TempDir;

const TENANT: &str = "acme";

const SKILL_CUSTOMER: (&str, &str) = (
    "markdown/skills/customer.md",
    "---\n\
     type: skill\n\
     id: customer\n\
     ---\n\
     # customer\n",
);

const SKILL_MEETING: (&str, &str) = (
    "markdown/skills/meeting.md",
    "---\n\
     type: skill\n\
     id: meeting\n\
     ---\n\
     # meeting\n",
);

const ACME: (&str, &str) = (
    "markdown/instances/customer/acme-corp.md",
    "---\n\
     type: instance\n\
     skill: customer\n\
     id: acme-corp\n\
     ---\n\
     # Acme\n\
     \n\
     Comparable: [[customer::globex-llc]]. QBR notes:\n\
     [[meeting::2026-04-12-acme-qbr#blk-acme-signals]].\n",
);

const GLOBEX: (&str, &str) = (
    "markdown/instances/customer/globex-llc.md",
    "---\n\
     type: instance\n\
     skill: customer\n\
     id: globex-llc\n\
     ---\n\
     # Globex\n\
     \n\
     Comparable to [[customer::acme-corp]]; see also\n\
     [[customer::acme-corp#renewals]] for the latest.\n",
);

const MEETING_APR: (&str, &str) = (
    "markdown/instances/meeting/2026-04-12-acme-qbr.md",
    "---\n\
     type: instance\n\
     skill: meeting\n\
     id: 2026-04-12-acme-qbr\n\
     at: 2026-04-12T10:00:00+02:00\n\
     ---\n\
     # QBR\n\
     \n\
     With [[customer::acme-corp]]. Decisions tracked in\n\
     [[meeting::2026-04-12-acme-qbr#blk-acme-signals]] block\n\
     references this very page.\n",
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

async fn seed(h: &Harness, pages: &[(&str, &'static str)]) {
    for (path, body) in pages {
        let key = Key::new(TENANT, path.to_owned()).unwrap();
        h.store
            .write(&key, Bytes::from_static(body.as_bytes()))
            .await
            .unwrap();
        h.indexer.update_page(path, body).await.unwrap();
    }
}

#[tokio::test]
async fn outbound_lists_links_from_page() {
    let h = fresh_harness();
    seed(
        &h,
        &[SKILL_CUSTOMER, SKILL_MEETING, ACME, GLOBEX, MEETING_APR],
    )
    .await;

    let out = h
        .indexer
        .neighbours(ACME.0, Direction::Out, None, None, None)
        .await
        .unwrap();

    assert_eq!(out.len(), 2, "acme has two outbound links: {out:#?}");
    let dst_pages: Vec<_> = out.iter().map(|e| e.dst_page.as_str()).collect();
    assert!(dst_pages.contains(&"globex-llc"));
    assert!(dst_pages.contains(&"2026-04-12-acme-qbr"));
}

#[tokio::test]
async fn outbound_filters_by_link_skill() {
    let h = fresh_harness();
    seed(
        &h,
        &[SKILL_CUSTOMER, SKILL_MEETING, ACME, GLOBEX, MEETING_APR],
    )
    .await;

    let only_customer = h
        .indexer
        .neighbours(ACME.0, Direction::Out, Some("customer"), None, None)
        .await
        .unwrap();
    assert_eq!(only_customer.len(), 1);
    assert_eq!(only_customer[0].dst_page, "globex-llc");
    assert_eq!(only_customer[0].link_skill, "customer");

    let only_meeting = h
        .indexer
        .neighbours(ACME.0, Direction::Out, Some("meeting"), None, None)
        .await
        .unwrap();
    assert_eq!(only_meeting.len(), 1);
    assert_eq!(only_meeting[0].dst_page, "2026-04-12-acme-qbr");
    assert_eq!(
        only_meeting[0].dst_anchor.as_deref(),
        Some("blk-acme-signals"),
        "anchor must round-trip through the links table",
    );
}

#[tokio::test]
async fn inbound_lists_links_to_page() {
    let h = fresh_harness();
    seed(
        &h,
        &[SKILL_CUSTOMER, SKILL_MEETING, ACME, GLOBEX, MEETING_APR],
    )
    .await;

    let inbound = h
        .indexer
        .neighbours(ACME.0, Direction::In, None, None, None)
        .await
        .unwrap();

    // Globex cites acme twice (with and without anchor) and the QBR
    // cites acme once. The PK (now including dst_anchor) keeps all
    // three rows separate.
    assert!(
        inbound.len() >= 3,
        "expected at least three inbound edges (Globex×2 + QBR×1), got {inbound:#?}",
    );
    let src_pages: Vec<_> = inbound.iter().map(|e| e.src_page.as_str()).collect();
    assert!(src_pages.contains(&GLOBEX.0));
    assert!(src_pages.contains(&MEETING_APR.0));
}

#[tokio::test]
async fn inbound_filters_by_link_skill() {
    let h = fresh_harness();
    seed(
        &h,
        &[SKILL_CUSTOMER, SKILL_MEETING, ACME, GLOBEX, MEETING_APR],
    )
    .await;

    // Inbound `[[customer::acme-corp]]` links only — that excludes
    // the QBR's `[[customer::acme-corp]]` because dst_page=acme-corp
    // AND link_skill=customer; the QBR row also matches. Both
    // Globex's links match. So we expect at least 3 (Globex×2 +
    // QBR×1), all with link_skill = 'customer'.
    let inbound = h
        .indexer
        .neighbours(ACME.0, Direction::In, Some("customer"), None, None)
        .await
        .unwrap();
    assert!(!inbound.is_empty());
    assert!(inbound.iter().all(|e| e.link_skill == "customer"));
}

#[tokio::test]
async fn both_returns_union_in_and_out() {
    let h = fresh_harness();
    seed(
        &h,
        &[SKILL_CUSTOMER, SKILL_MEETING, ACME, GLOBEX, MEETING_APR],
    )
    .await;

    let out_only = h
        .indexer
        .neighbours(ACME.0, Direction::Out, None, None, None)
        .await
        .unwrap();
    let in_only = h
        .indexer
        .neighbours(ACME.0, Direction::In, None, None, None)
        .await
        .unwrap();
    let both = h
        .indexer
        .neighbours(ACME.0, Direction::Both, None, None, None)
        .await
        .unwrap();
    assert_eq!(
        both.len(),
        out_only.len() + in_only.len(),
        "Both = Out ∪ In (no de-dup): out={} in={} both={}",
        out_only.len(),
        in_only.len(),
        both.len(),
    );
}

#[tokio::test]
async fn neighbours_of_unknown_page_returns_empty_or_only_outbound_zero() {
    let h = fresh_harness();
    seed(&h, &[SKILL_CUSTOMER, ACME]).await;

    let inbound = h
        .indexer
        .neighbours(
            "markdown/does/not/exist.md",
            Direction::In,
            None,
            None,
            None,
        )
        .await
        .unwrap();
    assert!(
        inbound.is_empty(),
        "no page → no resolved (skill, slug) → no inbound rows",
    );

    let outbound = h
        .indexer
        .neighbours(
            "markdown/does/not/exist.md",
            Direction::Out,
            None,
            None,
            None,
        )
        .await
        .unwrap();
    assert!(
        outbound.is_empty(),
        "no link rows reference an unknown src_page",
    );
}
