//! Integration tests for `Indexer::validate` (dry-run authoring
//! checks). Real DuckDB + real FsStore, no mocks. These pin the
//! exact issue set produced for a draft that references several
//! skills — some indexed, some not — so the batched single-pass
//! skill resolution stays behaviourally identical to the old
//! per-wikilink query path.

use std::sync::Arc;

use bytes::Bytes;
use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator, Severity};
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
       - status\n\
     ---\n\
     # customer\n",
);

const SKILL_MEETING: (&str, &str) = (
    "markdown/skills/meeting.md",
    "---\n\
     type: skill\n\
     id: meeting\n\
     description: A meeting.\n\
     ---\n\
     # meeting\n",
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
async fn validate_clean_draft_has_no_issues() {
    let h = fresh_harness();
    seed(&h, &[SKILL_CUSTOMER]).await;

    let draft = "---\n\
                 type: instance\n\
                 skill: customer\n\
                 id: acme\n\
                 tier: enterprise\n\
                 status: active\n\
                 ---\n\
                 # Acme\n";
    let issues = h.indexer.validate(None, draft).await.unwrap();
    assert!(issues.is_empty(), "{issues:?}");
}

#[tokio::test]
async fn validate_batches_mixed_wikilink_skills_with_identical_issue_set() {
    let h = fresh_harness();
    seed(&h, &[SKILL_CUSTOMER, SKILL_MEETING]).await;

    // Draft references: customer (exists), meeting (exists, twice),
    // vendor (unknown), project (unknown), plus an empty-id typed
    // link and a bare link (no skill). It also declares skill:
    // customer but omits the required `status` key.
    let draft = "---\n\
                 type: instance\n\
                 skill: customer\n\
                 id: acme\n\
                 tier: enterprise\n\
                 ---\n\
                 # Acme\n\
                 Linked to [[customer::globex]] and [[meeting::qbr]].\n\
                 Also [[meeting::renewal]] and [[vendor::aws]].\n\
                 And [[project::atlas]] plus [[customer::]] and [[bare-id]].\n";

    let issues = h.indexer.validate(None, draft).await.unwrap();

    // Required-key miss: status (customer requires tier+status; tier present).
    let required_misses: Vec<_> = issues
        .iter()
        .filter(|i| i.code == "frontmatter_required_key_missing")
        .collect();
    assert_eq!(required_misses.len(), 1, "{issues:?}");
    assert_eq!(required_misses[0].location, "frontmatter.status");
    assert_eq!(required_misses[0].severity, Severity::Error);

    // Unknown-skill errors: vendor + project (customer/meeting exist).
    let mut unknown: Vec<_> = issues
        .iter()
        .filter(|i| i.code == "unknown_skill")
        .map(|i| i.message.clone())
        .collect();
    unknown.sort();
    assert_eq!(unknown.len(), 2, "{issues:?}");
    assert!(unknown[0].contains("project"), "{unknown:?}");
    assert!(unknown[1].contains("vendor"), "{unknown:?}");

    // Empty-id typed wikilink: one wikilink_parse warning.
    let parse_warns: Vec<_> = issues
        .iter()
        .filter(|i| i.code == "wikilink_parse")
        .collect();
    assert_eq!(parse_warns.len(), 1, "{issues:?}");
    assert_eq!(parse_warns[0].severity, Severity::Warning);

    // Total issue count is exactly these four.
    assert_eq!(issues.len(), 4, "unexpected extra issues: {issues:?}");
}

#[tokio::test]
async fn validate_instance_with_unknown_declared_skill_errors() {
    let h = fresh_harness();
    // No skills seeded.
    let draft = "---\n\
                 type: instance\n\
                 skill: ghost\n\
                 id: x\n\
                 ---\n\
                 # X\n";
    let issues = h.indexer.validate(None, draft).await.unwrap();
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert_eq!(issues[0].code, "unknown_skill");
    assert_eq!(issues[0].location, "frontmatter.skill");
}
