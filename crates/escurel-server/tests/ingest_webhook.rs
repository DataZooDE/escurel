//! E2E for the `/ingest` document-ingestion webhook (PR-3c).
//! Real gateway + DuckDB + OIDC, real reqwest. No mocks.

use std::sync::Arc;

use bytes::Bytes;
use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_quota::{QuotaConfig, QuotaManager};
use escurel_storage::{FsStore, LaneStore};
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
  chunk: { max_chars: 800, overlap: 100 }
  retrieval: duckdb
---
# memo
";

struct Setup {
    process: EscurelProcess,
    blob_id: String,
    _dirs: Vec<TempDir>,
}

async fn setup(quota: Option<QuotaConfig>) -> Setup {
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
    // Deposit the original into the inbox first (the canonical-before-process
    // step); the webhook references it by id.
    let blob_id = store
        .put_inbox_blob(TENANT, Bytes::from_static(b"a short memo body"), None)
        .await
        .unwrap();

    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            indexer: Some(indexer),
            quota: quota.map(|c| Arc::new(QuotaManager::new(c))),
            ..Default::default()
        },
        ..Default::default()
    })
    .await;

    Setup {
        process,
        blob_id: blob_id.as_str().to_owned(),
        _dirs: vec![store_dir, db_dir],
    }
}

async fn post_ingest(p: &EscurelProcess, token: Option<&str>, body: Value) -> (StatusCode, Value) {
    let url = format!("{}/ingest", p.base_url());
    let mut req = reqwest::Client::new().post(&url).json(&body);
    if let Some(t) = token {
        req = req.header("authorization", format!("Bearer {t}"));
    }
    let resp = req.send().await.expect("post");
    let status = resp.status();
    let json: Value = resp.json().await.unwrap_or(Value::Null);
    (status, json)
}

#[tokio::test]
async fn ingest_records_event_and_routes_to_handler_skill() {
    let s = setup(None).await;
    let token = s.process.mint_token(TENANT, Role::Agent);
    let (status, body) = post_ingest(
        &s.process,
        Some(&token),
        json!({ "blob_id": s.blob_id, "content_type": "text/plain" }),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "body: {body}");
    assert_eq!(body["handler_skill"], "memo");
    // PR-3d wires the worker: a born-digital text blob materialises inline.
    assert_eq!(body["status"], "materialised", "body: {body}");
    assert!(
        body["event_id"].as_str().is_some_and(|s| !s.is_empty()),
        "an ingest Event must be recorded: {body}"
    );
    s.process.shutdown().await;
}

#[tokio::test]
async fn unmatched_mime_parked_no_handler_skill() {
    let s = setup(None).await;
    let token = s.process.mint_token(TENANT, Role::Agent);
    let (status, body) = post_ingest(
        &s.process,
        Some(&token),
        json!({ "blob_id": s.blob_id, "content_type": "application/zip" }),
    )
    .await;
    // Still 202 + an Event (auditable), but parked with no_handler_skill.
    assert_eq!(status, StatusCode::ACCEPTED, "body: {body}");
    assert_eq!(body["status"], "no_handler");
    assert_eq!(body["issue"]["code"], "no_handler_skill");
    assert!(body["event_id"].as_str().is_some_and(|s| !s.is_empty()));
    s.process.shutdown().await;
}

#[tokio::test]
async fn unauthenticated_ingest_rejected() {
    let s = setup(None).await;
    let (status, _body) = post_ingest(
        &s.process,
        None,
        json!({ "blob_id": s.blob_id, "content_type": "text/plain" }),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    s.process.shutdown().await;
}

#[tokio::test]
async fn ingest_rate_limited_per_tenant() {
    // One write/minute: the first ingest passes, the second is throttled.
    let s = setup(Some(QuotaConfig {
        writes_per_minute: 1,
        ..QuotaConfig::defaults()
    }))
    .await;
    let token = s.process.mint_token(TENANT, Role::Agent);
    let body = json!({ "blob_id": s.blob_id, "content_type": "text/plain" });
    let (s1, _) = post_ingest(&s.process, Some(&token), body.clone()).await;
    assert_eq!(s1, StatusCode::ACCEPTED);
    let (s2, _) = post_ingest(&s.process, Some(&token), body).await;
    assert_eq!(
        s2,
        StatusCode::TOO_MANY_REQUESTS,
        "second ingest should throttle"
    );
    s.process.shutdown().await;
}
