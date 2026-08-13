//! M7-PR6: `seed_from_dir` also loads an `events.json` (into the events
//! table / inbox, optionally assigned to an instance) and a
//! `history.json` (CRDT snapshot timelines). Real DuckDB + FsStore +
//! ZeroEmbedder, no mocks.

use std::sync::Arc;

use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, LaneStore};
use tempfile::TempDir;

const TENANT: &str = "acme";
const SPINE: &str = "markdown/instances/engagement__spine.md";

fn fresh_indexer() -> (Indexer, TempDir, TempDir) {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Indexer::new(store, embedder, conn, TENANT).unwrap();
    (indexer, store_dir, db_dir)
}

/// Build a minimal seed dir: two skills, one entity instance, an
/// events.json (one assigned + one inbox), a history.json for the spine.
fn write_corpus() -> TempDir {
    let dir = TempDir::new().unwrap();
    let p = dir.path();
    std::fs::create_dir_all(p.join("skills")).unwrap();
    std::fs::create_dir_all(p.join("instances")).unwrap();

    std::fs::write(
        p.join("skills/email.md"),
        "---\ntype: skill\nid: email\ndescription: An email event.\nrequired_frontmatter:\n  - at\n---\n# email\n",
    )
    .unwrap();
    std::fs::write(
        p.join("skills/engagement.md"),
        "---\ntype: skill\nid: engagement\ndescription: A delivery engagement.\n---\n# engagement\n",
    )
    .unwrap();
    std::fs::write(
        p.join("instances/engagement__spine.md"),
        "---\ntype: instance\nskill: engagement\nid: spine\nat: 2026-03-01T00:00:00Z\ncontract_value: \"620k\"\n---\n# Spine\n",
    )
    .unwrap();

    std::fs::write(
        p.join("events.json"),
        serde_json::to_string_pretty(&serde_json::json!([
            {
                "at": "2026-03-12T09:00:00Z",
                "source": "gmail",
                "mime": "message/rfc822",
                "label_skill": "email",
                "title": "Proposal",
                "instance": SPINE,
                "status": "processed"
            },
            {
                "at": "2026-06-01T07:30:00Z",
                "source": "agent",
                "mime": "text/markdown",
                "label_skill": "doc",
                "title": "Scope-creep auto-detector"
            }
        ]))
        .unwrap(),
    )
    .unwrap();

    std::fs::write(
        p.join("history.json"),
        serde_json::to_string_pretty(&serde_json::json!([
            {
                "page_id": SPINE,
                "states": [
                    { "taken_at": "2026-03-10T00:00:00Z",
                      "markdown": "---\ntype: instance\nskill: engagement\nid: spine\nat: 2026-03-01T00:00:00Z\ncontract_value: \"350k\"\n---\n# Spine\n" },
                    { "taken_at": "2026-05-10T00:00:00Z",
                      "markdown": "---\ntype: instance\nskill: engagement\nid: spine\nat: 2026-03-01T00:00:00Z\ncontract_value: \"620k\"\n---\n# Spine\n" }
                ]
            }
        ]))
        .unwrap(),
    )
    .unwrap();

    dir
}

#[tokio::test]
async fn seed_loads_events_inbox_history_and_assignments() {
    let (indexer, _s, _d) = fresh_indexer();
    let dir = write_corpus();
    indexer.seed_from_dir(dir.path()).await.unwrap();

    // The unassigned doc event is in the inbox; the assigned email isn't.
    let inbox = indexer.list_inbox(None).await.unwrap();
    assert_eq!(inbox.len(), 1, "one unassigned event in the inbox");
    assert_eq!(inbox[0].source, "agent");

    // The assigned email is in the spine's event history.
    let history = indexer.list_events(SPINE, None).await.unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].title, "Proposal");
    assert_eq!(history[0].status, "processed");

    // The snapshot history is queryable: state-at-T reconstructs.
    let early = indexer
        .expand(SPINE, Some("2026-04-01T00:00:00Z"), None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        early
            .frontmatter
            .get("contract_value")
            .and_then(|v| v.as_str()),
        Some("350k"),
    );
}
