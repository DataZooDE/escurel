//! M7-PR1: frontmatter wikilinks (`about:`/`derived_from:`/…) must be
//! indexed into `links`, so an event whose only mention of its entity is
//! in frontmatter is still reachable via `neighbours(entity, In)`.
//! Real DuckDB + real FsStore + ZeroEmbedder, no mocks.

use std::sync::Arc;

use bytes::Bytes;
use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Direction, Indexer, Migrator};
use escurel_storage::{FsStore, Key, LaneStore};
use tempfile::TempDir;

const TENANT: &str = "acme";

const SKILL_ENGAGEMENT: (&str, &str) = (
    "markdown/skills/engagement.md",
    "---\ntype: skill\nid: engagement\ndescription: A delivery engagement.\n---\n# engagement\n",
);
const SKILL_EMAIL: (&str, &str) = (
    "markdown/skills/email.md",
    "---\ntype: skill\nid: email\ndescription: An email event.\nrequired_frontmatter:\n  - at\n---\n# email\n",
);

// The entity (instance). Its body has NO backlinks to anything.
const ENTITY_SPINE: (&str, &str) = (
    "markdown/instances/engagement/spine.md",
    "---\ntype: instance\nskill: engagement\nid: spine\n---\n# Spine\n\nThe lifecycle spine.\n",
);

// An event whose ONLY mention of the entity is the frontmatter `about:`
// link — the body deliberately never names the spine.
const EVENT_ABOUT: (&str, &str) = (
    "markdown/instances/email/ev1.md",
    "---\ntype: instance\nskill: email\nid: ev1\nat: 2026-04-01T09:00:00Z\nabout: [[engagement::spine]]\n---\n# Inbound mail\n\nBody text that names no entity at all.\n",
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

#[tokio::test]
async fn neighbours_in_finds_frontmatter_only_event_link() {
    let h = fresh_harness();
    seed(
        &h,
        &[SKILL_ENGAGEMENT, SKILL_EMAIL, ENTITY_SPINE, EVENT_ABOUT],
    )
    .await;

    let backlinks = h
        .indexer
        .neighbours(ENTITY_SPINE.0, Direction::In, None, None, None)
        .await
        .unwrap();

    assert_eq!(
        backlinks.len(),
        1,
        "the event's frontmatter `about:` link must be indexed as a backlink",
    );
    assert_eq!(backlinks[0].src_page, EVENT_ABOUT.0);
    assert_eq!(backlinks[0].link_skill, "engagement");
}
