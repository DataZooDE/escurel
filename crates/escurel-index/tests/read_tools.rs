//! Integration tests for `Indexer::list_skills` and
//! `Indexer::list_instances`. Real DuckDB + real FsStore, no mocks.

use std::sync::Arc;

use bytes::Bytes;
use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator, OrderDir};
use escurel_storage::{FsStore, Key, LaneStore};
use tempfile::TempDir;

const TENANT: &str = "acme";

const SKILL_CUSTOMER: (&str, &str) = (
    "markdown/skills/customer.md",
    "---\n\
     type: skill\n\
     id: customer\n\
     description: A buying entity.\n\
     required_frontmatter:\n\
       - tier\n\
       - opened\n\
       - status\n\
     optional_frontmatter:\n\
       - mrr_band\n\
     ---\n\
     # customer\n",
);

const SKILL_MEETING: (&str, &str) = (
    "markdown/skills/meeting.md",
    "---\n\
     type: skill\n\
     id: meeting\n\
     description: An in-person or remote meeting.\n\
     required_frontmatter:\n\
       - at\n\
       - participants\n\
     optional_frontmatter:\n\
       - location\n\
     ---\n\
     # meeting (event-typed)\n",
);

const INSTANCE_ACME: (&str, &str) = (
    "markdown/instances/customer/acme-corp.md",
    "---\n\
     type: instance\n\
     skill: customer\n\
     id: acme-corp\n\
     tier: enterprise\n\
     ---\n\
     # Acme Corp\n",
);

const INSTANCE_GLOBEX: (&str, &str) = (
    "markdown/instances/customer/globex-llc.md",
    "---\n\
     type: instance\n\
     skill: customer\n\
     id: globex-llc\n\
     tier: mid-market\n\
     ---\n\
     # Globex LLC\n",
);

const MEETING_APR: (&str, &str) = (
    "markdown/instances/meeting/2026-04-12-acme-qbr.md",
    "---\n\
     type: instance\n\
     skill: meeting\n\
     id: 2026-04-12-acme-qbr\n\
     at: 2026-04-12T10:00:00+02:00\n\
     ---\n\
     # Acme QBR\n",
);

const MEETING_MAY: (&str, &str) = (
    "markdown/instances/meeting/2026-05-18-globex-renewal.md",
    "---\n\
     type: instance\n\
     skill: meeting\n\
     id: 2026-05-18-globex-renewal\n\
     at: 2026-05-18T14:30:00+02:00\n\
     ---\n\
     # Globex renewal\n",
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
    let duckdb_path = db_dir.path().join("escurel.duckdb");

    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(&duckdb_path).unwrap();
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

// --- list_skills ------------------------------------------------

#[tokio::test]
async fn list_skills_on_empty_index_returns_empty() {
    let h = fresh_harness();
    let skills = h.indexer.list_skills().await.unwrap();
    assert!(skills.is_empty());
}

#[tokio::test]
async fn list_skills_returns_one_per_skill_page() {
    let h = fresh_harness();
    seed(&h, &[SKILL_CUSTOMER, SKILL_MEETING, INSTANCE_ACME]).await;

    let mut skills = h.indexer.list_skills().await.unwrap();
    skills.sort_by(|a, b| a.id.cmp(&b.id));

    assert_eq!(skills.len(), 2, "instance row must not appear");
    assert_eq!(skills[0].id, "customer");
    assert_eq!(skills[1].id, "meeting");
}

#[tokio::test]
async fn list_skills_projects_description_and_frontmatter_keys() {
    let h = fresh_harness();
    seed(&h, &[SKILL_CUSTOMER]).await;

    let skills = h.indexer.list_skills().await.unwrap();
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].description, "A buying entity.");
    assert_eq!(
        skills[0].required_frontmatter,
        vec!["tier", "opened", "status"]
    );
    assert_eq!(skills[0].optional_frontmatter, vec!["mrr_band"]);
    assert!(!skills[0].is_event_typed);
}

#[tokio::test]
async fn list_skills_flags_event_typed_when_at_is_required() {
    let h = fresh_harness();
    seed(&h, &[SKILL_MEETING]).await;

    let skills = h.indexer.list_skills().await.unwrap();
    assert!(
        skills[0].is_event_typed,
        "`at` in required_frontmatter must set is_event_typed",
    );
}

// --- list_instances ---------------------------------------------

#[tokio::test]
async fn list_instances_filters_by_skill() {
    let h = fresh_harness();
    seed(
        &h,
        &[
            SKILL_CUSTOMER,
            SKILL_MEETING,
            INSTANCE_ACME,
            INSTANCE_GLOBEX,
            MEETING_APR,
        ],
    )
    .await;

    let customers = h
        .indexer
        .list_instances("customer", None, None)
        .await
        .unwrap();
    assert_eq!(customers.len(), 2);
    assert!(customers.iter().all(|i| i.skill == "customer"));

    let meetings = h
        .indexer
        .list_instances("meeting", None, None)
        .await
        .unwrap();
    assert_eq!(meetings.len(), 1);
    assert_eq!(meetings[0].skill, "meeting");
}

#[tokio::test]
async fn list_instances_for_unknown_skill_returns_empty() {
    let h = fresh_harness();
    seed(&h, &[SKILL_CUSTOMER, INSTANCE_ACME]).await;
    let out = h
        .indexer
        .list_instances("does-not-exist", None, None)
        .await
        .unwrap();
    assert!(out.is_empty());
}

#[tokio::test]
async fn list_instances_order_by_at_desc_is_chronological_reverse() {
    let h = fresh_harness();
    seed(&h, &[SKILL_MEETING, MEETING_APR, MEETING_MAY]).await;

    let out = h
        .indexer
        .list_instances("meeting", Some(OrderDir::Desc), None)
        .await
        .unwrap();
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].at.as_deref(), Some("2026-05-18T14:30:00+02:00"));
    assert_eq!(out[1].at.as_deref(), Some("2026-04-12T10:00:00+02:00"));
}

#[tokio::test]
async fn list_instances_order_by_at_asc_is_chronological_forward() {
    let h = fresh_harness();
    seed(&h, &[SKILL_MEETING, MEETING_APR, MEETING_MAY]).await;

    let out = h
        .indexer
        .list_instances("meeting", Some(OrderDir::Asc), None)
        .await
        .unwrap();
    assert_eq!(out[0].at.as_deref(), Some("2026-04-12T10:00:00+02:00"));
    assert_eq!(out[1].at.as_deref(), Some("2026-05-18T14:30:00+02:00"));
}

#[tokio::test]
async fn list_instances_respects_limit() {
    let h = fresh_harness();
    seed(&h, &[SKILL_MEETING, MEETING_APR, MEETING_MAY]).await;

    let out = h
        .indexer
        .list_instances("meeting", Some(OrderDir::Desc), Some(1))
        .await
        .unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].at.as_deref(), Some("2026-05-18T14:30:00+02:00"));
}

#[tokio::test]
async fn list_instances_surfaces_full_frontmatter_as_json() {
    let h = fresh_harness();
    seed(&h, &[SKILL_CUSTOMER, INSTANCE_ACME]).await;

    let out = h
        .indexer
        .list_instances("customer", None, None)
        .await
        .unwrap();
    assert_eq!(out.len(), 1);
    let fm = &out[0].frontmatter;
    assert_eq!(fm.get("id").and_then(|v| v.as_str()), Some("acme-corp"));
    assert_eq!(fm.get("tier").and_then(|v| v.as_str()), Some("enterprise"));
}
