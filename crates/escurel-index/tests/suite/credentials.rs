//! Integration tests for the external-source credential registry
//! (SQL-view backend, PR-2a). Real DuckDB file, no mocks.
//!
//! The load-bearing security property: the secret is stored server-side in
//! `kb.duckdb` (the `external_credentials` table) and is reachable only via
//! `lookup_credential` — the operator `list_credentials` view never echoes
//! it, and nothing writes it into the markdown corpus.

use std::sync::Arc;

use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, LaneStore};
use tempfile::TempDir;

const TENANT: &str = "acme";

struct Harness {
    indexer: Indexer,
    duckdb_path: std::path::PathBuf,
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
    let indexer = Indexer::new(store, embedder, conn, TENANT).expect("indexer");
    Harness {
        indexer,
        duckdb_path,
        _store_dir: store_dir,
        _db_dir: db_dir,
    }
}

const DSN: &str = "postgresql://svc:hunter2@crm.internal:5432/crm";

#[tokio::test]
async fn register_lookup_list_delete_roundtrip() {
    let h = fresh_harness();

    h.indexer
        .register_credential("crm_pg", "postgres", DSN, Some("admin-1"))
        .await
        .expect("register");

    // lookup returns the secret for the backend to build an ATTACH.
    let rec = h.indexer.lookup_credential("crm_pg").await.unwrap();
    let rec = rec.expect("credential present");
    assert_eq!(rec.connector, "postgres");
    assert_eq!(rec.secret, DSN);

    // list NEVER carries the secret (REQ-SQL-05).
    let listed = h.indexer.list_credentials().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "crm_pg");
    assert_eq!(listed[0].connector, "postgres");
    assert_eq!(listed[0].created_by.as_deref(), Some("admin-1"));

    // missing name resolves to None, not an error.
    assert!(h.indexer.lookup_credential("nope").await.unwrap().is_none());

    h.indexer.delete_credential("crm_pg").await.unwrap();
    assert!(
        h.indexer
            .lookup_credential("crm_pg")
            .await
            .unwrap()
            .is_none()
    );
    assert!(h.indexer.list_credentials().await.unwrap().is_empty());
}

#[tokio::test]
async fn register_is_idempotent_upsert() {
    let h = fresh_harness();
    h.indexer
        .register_credential("crm_pg", "postgres", "old", None)
        .await
        .unwrap();
    h.indexer
        .register_credential("crm_pg", "postgres", DSN, Some("admin-2"))
        .await
        .unwrap();
    let rec = h
        .indexer
        .lookup_credential("crm_pg")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rec.secret, DSN, "second register overwrites the secret");
    assert_eq!(h.indexer.list_credentials().await.unwrap().len(), 1);
}

#[tokio::test]
async fn secret_survives_rebuild() {
    // The credential registry is a separate canonical input — rebuild
    // (which re-derives pages/links/blocks from markdown) must NOT drop it.
    let h = fresh_harness();
    h.indexer
        .register_credential("crm_pg", "postgres", DSN, None)
        .await
        .unwrap();
    h.indexer.rebuild().await.expect("rebuild");
    let rec = h.indexer.lookup_credential("crm_pg").await.unwrap();
    assert_eq!(rec.expect("survives rebuild").secret, DSN);
}

#[tokio::test]
async fn secret_is_server_side_only_never_in_markdown_corpus() {
    // Registering a credential stores the DSN server-side (retrievable via
    // lookup) and writes NOTHING into the LaneStore markdown corpus — so
    // tenant_export (which tars only markdown/) can never carry it.
    let h = fresh_harness();
    h.indexer
        .register_credential("crm_pg", "postgres", DSN, None)
        .await
        .unwrap();

    // Server-side: the secret is reachable for the backend to build ATTACH.
    let rec = h.indexer.lookup_credential("crm_pg").await.unwrap();
    assert_eq!(rec.expect("server-side").secret, DSN);
    // Used here so the harness keeps the db path field exercised.
    assert!(h.duckdb_path.exists());

    // Corpus: no file under the markdown store contains the secret.
    for entry in walk(h._store_dir.path()) {
        let bytes = std::fs::read(&entry).unwrap_or_default();
        assert!(
            find_subslice(&bytes, DSN.as_bytes()).is_none(),
            "secret leaked into corpus file {}",
            entry.display()
        );
    }
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
    }
    out
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}
