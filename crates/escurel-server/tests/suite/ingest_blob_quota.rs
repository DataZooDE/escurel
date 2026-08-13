//! E2E for the per-upload blob-size quota on `/ingest/upload` (Phase-1.1).
//! Real gateway + DuckDB + FsStore + OIDC, real reqwest. No mocks.
//!
//! An upload larger than the tenant's `max_blob_bytes` must be rejected with
//! HTTP 413 **before** the bytes are deposited into the inbox — so an
//! oversize payload can never land on the host volume. An under-cap upload of
//! the same skill materialises normally.

use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_quota::{QuotaConfig, QuotaManager};
use escurel_storage::{BlobId, FsStore, LaneStore};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, Opts, Role};
use reqwest::StatusCode;
use serde_json::{Value, json};
use tempfile::TempDir;

const TENANT: &str = "acme";
const MEMO_SKILL: &str = "\
---
type: skill
id: memo
description: Text memos ingested as documents.
backend:
  kind: document
  accepts: [text/plain, text/markdown]
---
# memo
";

struct Setup {
    process: EscurelProcess,
    store: Arc<dyn LaneStore>,
    _dirs: Vec<TempDir>,
}

async fn setup(max_blob_bytes: u64) -> Setup {
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
            quota: Some(Arc::new(QuotaManager::new(QuotaConfig {
                max_blob_bytes,
                ..QuotaConfig::defaults()
            }))),
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

async fn post_upload(
    p: &EscurelProcess,
    token: &str,
    content_type: &str,
    bytes: &[u8],
) -> (StatusCode, Value) {
    let url = format!("{}/ingest/upload", p.base_url());
    let resp = reqwest::Client::new()
        .post(&url)
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "content_type": content_type,
            "bytes_b64": B64.encode(bytes),
        }))
        .send()
        .await
        .expect("post");
    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    (status, body)
}

#[tokio::test]
async fn oversize_upload_rejected_before_deposit() {
    // Cap at 64 bytes; the payload is well over it.
    let s = setup(64).await;
    let token = s.process.mint_token(TENANT, Role::Agent);
    let big = vec![b'x'; 4096];

    let (status, body) = post_upload(&s.process, &token, "text/plain", &big).await;
    assert_eq!(
        status,
        StatusCode::PAYLOAD_TOO_LARGE,
        "oversize upload must be 413: {body}"
    );

    // The decisive invariant: the bytes were NEVER deposited. The inbox blob
    // for this content must not exist.
    let id = BlobId::of(&big);
    let deposited = s.store.get_inbox_blob(TENANT, &id).await;
    assert!(
        deposited.is_err(),
        "oversize payload must not be deposited into the inbox (rejected before deposit)"
    );

    s.process.shutdown().await;
}

#[tokio::test]
async fn under_cap_upload_materialises() {
    // Generous cap; a small text doc sails through and materialises.
    let s = setup(25 * 1024 * 1024).await;
    let token = s.process.mint_token(TENANT, Role::Agent);

    let (status, body) = post_upload(&s.process, &token, "text/plain", b"a short memo body").await;
    assert_eq!(status, StatusCode::ACCEPTED, "under-cap upload: {body}");
    assert_eq!(body["status"], "materialised", "body: {body}");
    assert_eq!(body["handler_skill"], "memo");

    s.process.shutdown().await;
}
