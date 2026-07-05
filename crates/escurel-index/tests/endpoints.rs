//! Integration tests for the remote-backend endpoint registry
//! (openapi/mcp backends). Real DuckDB file, no mocks.
//!
//! The load-bearing security property: the base URL + auth secret are stored
//! server-side in `kb.duckdb` (the `external_endpoints` table) and the secret
//! is reachable only via `lookup_endpoint` — the operator `list_endpoints`
//! view never echoes it, and nothing writes it into the markdown corpus. This
//! is also the SSRF guard: a live remote instance can only reach an
//! admin-registered endpoint, never a raw URL from tenant markdown.

use std::sync::Arc;

use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::endpoints::EndpointAuth;
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, LaneStore};
use tempfile::TempDir;

const TENANT: &str = "acme";

struct Harness {
    indexer: Indexer,
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
        _store_dir: store_dir,
        _db_dir: db_dir,
    }
}

const SECRET: &str = "sk-live-abcdef0123456789";

#[tokio::test]
async fn register_lookup_list_delete_roundtrip() {
    let h = fresh_harness();

    h.indexer
        .register_endpoint(
            "crm_rest",
            "openapi",
            "https://crm.internal/api",
            &EndpointAuth::Bearer,
            Some(SECRET),
            Some("admin-1"),
        )
        .await
        .expect("register");

    // lookup returns the secret + base URL for the RemoteClient.
    let rec = h.indexer.lookup_endpoint("crm_rest").await.unwrap();
    let rec = rec.expect("endpoint present");
    assert_eq!(rec.kind, "openapi");
    assert_eq!(rec.base_url, "https://crm.internal/api");
    assert_eq!(rec.auth, EndpointAuth::Bearer);
    assert_eq!(rec.secret.as_deref(), Some(SECRET));

    // list NEVER carries the secret (REQ-REMOTE-05).
    let listed = h.indexer.list_endpoints().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "crm_rest");
    assert_eq!(listed[0].kind, "openapi");
    assert_eq!(listed[0].base_url, "https://crm.internal/api");
    assert_eq!(listed[0].auth_scheme, "bearer");
    assert_eq!(listed[0].created_by.as_deref(), Some("admin-1"));

    // missing name resolves to None, not an error.
    assert!(h.indexer.lookup_endpoint("nope").await.unwrap().is_none());

    h.indexer.delete_endpoint("crm_rest").await.unwrap();
    assert!(h.indexer.lookup_endpoint("crm_rest").await.unwrap().is_none());
    assert!(h.indexer.list_endpoints().await.unwrap().is_empty());
}

#[tokio::test]
async fn api_key_header_roundtrips() {
    let h = fresh_harness();
    let auth = EndpointAuth::ApiKey {
        header: "X-Acme-Key".to_owned(),
    };
    h.indexer
        .register_endpoint("kb", "mcp", "https://kb.internal/mcp", &auth, Some(SECRET), None)
        .await
        .unwrap();
    let rec = h.indexer.lookup_endpoint("kb").await.unwrap().unwrap();
    assert_eq!(rec.kind, "mcp");
    assert_eq!(
        rec.auth,
        EndpointAuth::ApiKey {
            header: "X-Acme-Key".to_owned()
        }
    );
    assert_eq!(h.indexer.list_endpoints().await.unwrap()[0].auth_scheme, "api_key");
}

#[tokio::test]
async fn register_is_idempotent_upsert() {
    let h = fresh_harness();
    h.indexer
        .register_endpoint("crm_rest", "openapi", "https://old", &EndpointAuth::None, None, None)
        .await
        .unwrap();
    h.indexer
        .register_endpoint(
            "crm_rest",
            "openapi",
            "https://crm.internal/api",
            &EndpointAuth::Bearer,
            Some(SECRET),
            Some("admin-2"),
        )
        .await
        .unwrap();
    let rec = h.indexer.lookup_endpoint("crm_rest").await.unwrap().unwrap();
    assert_eq!(rec.base_url, "https://crm.internal/api", "second register overwrites");
    assert_eq!(rec.secret.as_deref(), Some(SECRET));
    assert_eq!(h.indexer.list_endpoints().await.unwrap().len(), 1);
}

#[tokio::test]
async fn endpoint_survives_rebuild() {
    // The endpoint registry is a separate canonical input — rebuild (which
    // re-derives pages/links/blocks from markdown) must NOT drop it.
    let h = fresh_harness();
    h.indexer
        .register_endpoint(
            "crm_rest",
            "openapi",
            "https://crm.internal/api",
            &EndpointAuth::Bearer,
            Some(SECRET),
            None,
        )
        .await
        .unwrap();
    h.indexer.rebuild().await.expect("rebuild");
    let rec = h.indexer.lookup_endpoint("crm_rest").await.unwrap();
    assert_eq!(rec.expect("survives rebuild").secret.as_deref(), Some(SECRET));
}

#[tokio::test]
async fn secret_is_server_side_only_never_in_markdown_corpus() {
    let h = fresh_harness();
    h.indexer
        .register_endpoint(
            "crm_rest",
            "openapi",
            "https://crm.internal/api",
            &EndpointAuth::Bearer,
            Some(SECRET),
            None,
        )
        .await
        .unwrap();

    let rec = h.indexer.lookup_endpoint("crm_rest").await.unwrap();
    assert_eq!(rec.expect("server-side").secret.as_deref(), Some(SECRET));

    for entry in walk(h._store_dir.path()) {
        let bytes = std::fs::read(&entry).unwrap_or_default();
        assert!(
            find_subslice(&bytes, SECRET.as_bytes()).is_none(),
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
