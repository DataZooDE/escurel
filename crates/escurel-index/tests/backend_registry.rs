//! Integration tests for the `InstanceBackend` seam (PR-1a).
//!
//! Real DuckDB file in a `tempfile::TempDir`, real `FsStore`, real
//! `ZeroEmbedder` — no mocks. These pin the three places a future
//! SQL/Document backend could silently regress markdown:
//!
//! 1. the registry resolves the markdown default for an unannotated skill;
//! 2. the markdown backend's `search_contribution` is byte-identical to the
//!    indexer's `search_with` (delegation fidelity — the pure-refactor
//!    guarantee).

use std::sync::Arc;

use bytes::Bytes;
use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::backend::{BackendCtx, BackendKind, BackendRegistry, MarkdownBackend};
use escurel_index::{AclCaller, Granularity, Indexer, InstanceBackend, Migrator};
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

const INSTANCE_ACME: (&str, &str) = (
    "markdown/instances/customer/acme-corp.md",
    "---\n\
     type: instance\n\
     skill: customer\n\
     id: acme-corp\n\
     ---\n\
     # Acme Corp\n\
     \n\
     Industrial conglomerate based in Stuttgart.\n",
);

struct Harness {
    store: Arc<dyn LaneStore>,
    indexer: Arc<Indexer>,
    _store_dir: TempDir,
    _db_dir: TempDir,
}

fn fresh_harness() -> Harness {
    let store_dir = TempDir::new().expect("tempdir for store");
    let db_dir = TempDir::new().expect("tempdir for db");
    let duckdb_path = db_dir.path().join("escurel.duckdb");

    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(&duckdb_path).expect("open duckdb");
    Migrator::up(&conn).expect("migrate v1 schema");

    let indexer = Arc::new(
        Indexer::new(Arc::clone(&store), embedder, conn, TENANT)
            .expect("indexer with 768-dim embedder constructs"),
    );

    Harness {
        store,
        indexer,
        _store_dir: store_dir,
        _db_dir: db_dir,
    }
}

async fn write_and_index(h: &Harness, path: &str, body: &'static str) {
    let key = Key::new(TENANT, path.to_owned()).expect("valid key");
    h.store
        .write(&key, Bytes::from_static(body.as_bytes()))
        .await
        .expect("write markdown");
    h.indexer.update_page(path, body).await.expect("index page");
}

fn ctx() -> BackendCtx<'static> {
    BackendCtx {
        caller: AclCaller {
            subject: "tester",
            is_admin: true,
            token_groups: &[],
        },
        as_of: None,
        scenario: None,
    }
}

#[tokio::test]
async fn registry_resolves_markdown_backend_for_unannotated_skill() {
    let h = fresh_harness();
    write_and_index(&h, SKILL_CUSTOMER.0, SKILL_CUSTOMER.1).await;

    let md: Arc<dyn escurel_index::InstanceBackend> =
        Arc::new(MarkdownBackend::new(Arc::clone(&h.indexer)));
    let registry = BackendRegistry::new(Arc::clone(&md));

    // An unannotated skill (and a never-seen skill) both resolve to markdown.
    let backend = registry.for_skill("customer");
    assert_eq!(backend.kind(), BackendKind::Markdown);
    assert!(backend.capabilities().writable);
    assert!(backend.capabilities().supports_crdt);

    assert_eq!(
        registry.for_skill("does-not-exist").kind(),
        BackendKind::Markdown
    );
}

#[tokio::test]
async fn markdown_backend_search_matches_indexer_search() {
    let h = fresh_harness();
    write_and_index(&h, SKILL_CUSTOMER.0, SKILL_CUSTOMER.1).await;
    write_and_index(&h, INSTANCE_ACME.0, INSTANCE_ACME.1).await;
    h.indexer.refresh_fts().await.expect("refresh fts");

    let backend = MarkdownBackend::new(Arc::clone(&h.indexer));

    // Same args to both surfaces; delegation must be lossless.
    let via_indexer = h
        .indexer
        .search_with(
            "Stuttgart",
            10,
            None,
            None,
            None,
            None,
            Granularity::Block,
            None,
        )
        .await
        .expect("indexer search");
    let via_backend = backend
        .search_contribution(ctx(), "Stuttgart", 10, None, None, Granularity::Block, None)
        .await
        .expect("backend search");

    assert_eq!(via_indexer, via_backend);
    assert!(
        !via_backend.is_empty(),
        "the Stuttgart instance should be a hit so the equality is meaningful"
    );
}
