//! `GET /blob/{page_id}` — the raw-bytes download twin of
//! `POST /ingest/upload` (2026-08-14 API review, B2).
//!
//! Bytes went IN over REST but could only come OUT as base64 inside a
//! JSON-RPC envelope (`fetch_blob`, 25 MiB cap, 33% inflation, no
//! Content-Type) — a browser previewing a PDF round-tripped the whole
//! blob through JSON. This route serves the retained original verbatim
//! with real headers, under exactly `fetch_blob`'s ACL: absent, hidden
//! and non-document pages are all the same `404` (no existence oracle).

use std::sync::Arc;

use bytes::Bytes;
use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, LaneStore};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, Opts, Role};
use serde_json::Value;
use tempfile::TempDir;

const TENANT: &str = "acme";
const MEMO_SKILL: &str = "\
---
type: skill
id: memo
description: Text memos ingested as documents.
backend:
  kind: document
  accepts: [text/plain]
  chunk: { max_chars: 40, overlap: 8 }
---
# memo
";

struct Setup {
    process: EscurelProcess,
    store: Arc<dyn LaneStore>,
    _dirs: Vec<TempDir>,
}

async fn setup() -> Setup {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Arc::new(Indexer::new(Arc::clone(&store), embedder, conn, TENANT).unwrap());
    indexer
        .update_page("markdown/skills/memo.md", MEMO_SKILL)
        .await
        .unwrap();
    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            indexer: Some(indexer),
            ..Default::default()
        },
        ..Default::default()
    })
    .await;
    Setup {
        process,
        store,
        _dirs: vec![store_dir, db_dir],
    }
}

/// Ingest `body` as a text memo and return the materialised `page_id`.
async fn ingest_text(s: &Setup, token: &str, body: &'static str) -> String {
    let blob = s
        .store
        .put_inbox_blob(TENANT, Bytes::from_static(body.as_bytes()), None)
        .await
        .unwrap();
    let resp: Value = reqwest::Client::new()
        .post(format!("{}/ingest", s.process.base_url()))
        .header("authorization", format!("Bearer {token}"))
        .json(&serde_json::json!({ "blob_id": blob.as_str(), "content_type": "text/plain" }))
        .send()
        .await
        .expect("post ingest")
        .json()
        .await
        .expect("decode");
    resp["page_id"]
        .as_str()
        .unwrap_or_else(|| panic!("ingest outcome: {resp}"))
        .to_owned()
}

#[tokio::test]
async fn get_blob_streams_the_original_bytes_with_real_headers() {
    let s = setup().await;
    let token = s.process.mint_token(TENANT, Role::Agent);
    let body = "The original bytes of the source document, verbatim.";
    let page_id = ingest_text(&s, &token, body).await;

    let resp = reqwest::Client::new()
        .get(format!("{}/blob/{page_id}", s.process.base_url()))
        .header("authorization", format!("Bearer {token}"))
        .send()
        .await
        .expect("get blob");
    assert_eq!(resp.status(), 200, "blob served");
    assert_eq!(
        resp.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("text/plain"),
        "declared MIME on the wire, not JSON"
    );
    assert_eq!(
        resp.content_length(),
        Some(body.len() as u64),
        "honest Content-Length"
    );
    let got = resp.bytes().await.expect("body");
    assert_eq!(&got[..], body.as_bytes(), "raw bytes, no base64 detour");

    s.process.shutdown().await;
}

#[tokio::test]
async fn get_blob_is_404_for_absent_and_non_document_pages_and_401_unauthed() {
    let s = setup().await;
    let token = s.process.mint_token(TENANT, Role::Agent);

    // A page that does not exist and a page with no blob behind it
    // (the skill catalogue page) answer identically — no oracle.
    for path in ["markdown/instances/memo/nope.md", "markdown/skills/memo.md"] {
        let resp = reqwest::Client::new()
            .get(format!("{}/blob/{path}", s.process.base_url()))
            .header("authorization", format!("Bearer {token}"))
            .send()
            .await
            .expect("get blob");
        assert_eq!(resp.status(), 404, "`{path}` must be a plain 404");
    }

    // No bearer at all → the gate refuses before any lookup.
    let resp = reqwest::Client::new()
        .get(format!(
            "{}/blob/markdown/skills/memo.md",
            s.process.base_url()
        ))
        .send()
        .await
        .expect("get blob");
    assert_eq!(resp.status(), 401, "unauthenticated → 401");

    s.process.shutdown().await;
}
