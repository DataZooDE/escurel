//! Integration tests for `Indexer::resolve` and `Indexer::expand`.

use std::sync::Arc;

use bytes::Bytes;
use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
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
     # customer\n",
);

const INSTANCE_ACME: (&str, &str) = (
    "markdown/instances/customer/acme-corp.md",
    "---\n\
     type: instance\n\
     skill: customer\n\
     id: acme-corp\n\
     tier: enterprise\n\
     ---\n\
     # Acme Corp\n\
     \n\
     Industrial conglomerate. See [[customer::globex-llc]] for the\n\
     comparable, and the QBR notes in\n\
     [[meeting::2026-04-12-acme-qbr#blk-acme-signals]].\n",
);

const INSTANCE_GLOBEX: (&str, &str) = (
    "markdown/instances/customer/globex-llc.md",
    "---\n\
     type: instance\n\
     skill: customer\n\
     id: globex-llc\n\
     ---\n\
     # Globex LLC\n",
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

// --- resolve ----------------------------------------------------

#[tokio::test]
async fn resolve_typed_wikilink_to_known_page() {
    let h = fresh_harness();
    seed(&h, &[SKILL_CUSTOMER, INSTANCE_ACME]).await;

    let r = h
        .indexer
        .resolve("[[customer::acme-corp]]", None)
        .await
        .unwrap();
    assert!(r.exists());
    let page = r.page.unwrap();
    assert_eq!(page.skill, "customer");
    assert_eq!(page.slug.as_deref(), Some("acme-corp"));
    assert_eq!(page.page_type, PageType::Instance);
    assert_eq!(r.parsed.skill.as_deref(), Some("customer"));
    assert_eq!(r.parsed.id.as_deref(), Some("acme-corp"));
}

#[tokio::test]
async fn resolve_typed_wikilink_with_anchor_and_version_keeps_segments() {
    let h = fresh_harness();
    seed(&h, &[SKILL_CUSTOMER, INSTANCE_ACME]).await;

    let r = h
        .indexer
        .resolve("[[customer::acme-corp#billing@v3|Acme]]", None)
        .await
        .unwrap();
    assert!(r.exists(), "anchor + version don't affect target lookup");
    assert_eq!(r.parsed.anchor.as_deref(), Some("billing"));
    assert_eq!(r.parsed.version.as_deref(), Some("v3"));
    assert_eq!(r.parsed.alias.as_deref(), Some("Acme"));
}

#[tokio::test]
async fn resolve_unknown_target_returns_exists_false() {
    let h = fresh_harness();
    seed(&h, &[SKILL_CUSTOMER]).await;

    let r = h
        .indexer
        .resolve("[[customer::no-such-thing]]", None)
        .await
        .unwrap();
    assert!(!r.exists());
    assert!(r.page.is_none());
    assert_eq!(r.parsed.id.as_deref(), Some("no-such-thing"));
}

#[tokio::test]
async fn resolve_bare_wikilink_matches_on_slug_alone() {
    let h = fresh_harness();
    seed(&h, &[SKILL_CUSTOMER, INSTANCE_ACME]).await;

    let r = h.indexer.resolve("[[acme-corp]]", None).await.unwrap();
    assert!(r.exists());
    assert_eq!(r.parsed.skill, None);
    assert_eq!(r.parsed.id.as_deref(), Some("acme-corp"));
    assert_eq!(r.page.unwrap().slug.as_deref(), Some("acme-corp"));
}

#[tokio::test]
async fn resolve_skill_namespace_finds_the_skill_page() {
    let h = fresh_harness();
    seed(&h, &[SKILL_CUSTOMER, INSTANCE_ACME]).await;

    // `skill::` is a reserved namespace meaning "the skill page itself".
    // `[[skill::customer]]` must resolve the `customer` skill definition,
    // matching the bare `[[customer]]` form. See issue #212.
    let r = h
        .indexer
        .resolve("[[skill::customer]]", None)
        .await
        .unwrap();
    assert!(
        r.exists(),
        "[[skill::customer]] should resolve the skill page"
    );
    let page = r.page.unwrap();
    assert_eq!(page.slug.as_deref(), Some("customer"));
    assert_eq!(page.page_type, PageType::Skill);
    assert_eq!(r.parsed.skill.as_deref(), Some("skill"));
    assert_eq!(r.parsed.id.as_deref(), Some("customer"));
}

#[tokio::test]
async fn resolve_skill_namespace_does_not_match_instances() {
    let h = fresh_harness();
    seed(&h, &[SKILL_CUSTOMER, INSTANCE_ACME]).await;

    // `acme-corp` is an instance, not a skill. The reserved `skill::`
    // namespace must only resolve skill pages, so this finds nothing.
    let r = h
        .indexer
        .resolve("[[skill::acme-corp]]", None)
        .await
        .unwrap();
    assert!(
        !r.exists(),
        "[[skill::acme-corp]] must not resolve an instance"
    );
    assert!(r.page.is_none());
}

#[tokio::test]
async fn resolve_skill_only_no_id_returns_no_target() {
    let h = fresh_harness();
    seed(&h, &[SKILL_CUSTOMER]).await;

    // `[[customer::]]` has empty id segment — parser yields id=None.
    let r = h.indexer.resolve("[[customer::]]", None).await.unwrap();
    assert!(!r.exists());
}

// --- expand -----------------------------------------------------

#[tokio::test]
async fn expand_unknown_page_returns_none() {
    let h = fresh_harness();
    let out = h
        .indexer
        .expand("does/not/exist.md", None, None)
        .await
        .unwrap();
    assert!(out.is_none());
}

#[tokio::test]
async fn expand_returns_full_body_and_wikilinks() {
    let h = fresh_harness();
    seed(&h, &[SKILL_CUSTOMER, INSTANCE_ACME, INSTANCE_GLOBEX]).await;

    let out = h
        .indexer
        .expand(INSTANCE_ACME.0, None, None)
        .await
        .unwrap()
        .expect("acme page expands");

    assert_eq!(out.page.page_id, INSTANCE_ACME.0);
    assert_eq!(out.page.skill, "customer");
    assert_eq!(out.page.slug.as_deref(), Some("acme-corp"));
    assert_eq!(out.page.page_type, PageType::Instance);

    // Frontmatter projection.
    assert_eq!(
        out.frontmatter.get("id").and_then(|v| v.as_str()),
        Some("acme-corp"),
    );
    assert_eq!(
        out.frontmatter.get("tier").and_then(|v| v.as_str()),
        Some("enterprise"),
    );

    // Body includes the markdown content.
    assert!(out.body.contains("Acme Corp"));
    assert!(out.body.contains("Industrial conglomerate"));

    // Outbound wikilinks parsed from the body.
    assert_eq!(out.wikilinks_out.len(), 2);
    let dst_ids: Vec<_> = out
        .wikilinks_out
        .iter()
        .filter_map(|w| w.id.as_deref())
        .collect();
    assert!(dst_ids.contains(&"globex-llc"));
    assert!(dst_ids.contains(&"2026-04-12-acme-qbr"));

    // One block per page (with the spec convention `blk-0` anchor).
    assert_eq!(out.blocks.len(), 1);
    assert_eq!(out.blocks[0].anchor, "blk-0");
}
