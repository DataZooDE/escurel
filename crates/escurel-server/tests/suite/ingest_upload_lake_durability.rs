//! #567: `POST /ingest/upload`'s `202 Accepted` + `"status":
//! "materialised"` is the same durability promise `update_page`'s
//! `ok:true` makes — #414/#415 gave `update_page` (and friends) a
//! synchronous, scoped lake publish before that promise is made; this
//! path (`materialize_document`/`write_document_blocks`) never got it,
//! because nothing in escurel-server was found calling it at the time.
//! It is called, from `run_document_ingest` — this proves the fix.
//!
//! Offline harness, no Docker: same DuckDB-file-catalog shape
//! `ducklake_publish.rs`/`synchronous_lake_durability.rs` already use,
//! reusing `ingest_blob_quota.rs`'s document-skill setup pattern.

use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::snapshot::{LakeConfig, ObjectStoreSecret};
use escurel_index::{Indexer, Migrator};
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

fn lake_config(lake_dir: &TempDir) -> LakeConfig {
    LakeConfig {
        catalog_dsn: lake_dir
            .path()
            .join("catalog.ducklake")
            .to_str()
            .unwrap()
            .to_owned(),
        data_path: lake_dir.path().join("data").to_str().unwrap().to_owned(),
        object_store: ObjectStoreSecret::None,
    }
}

fn reader_conn(cfg: &LakeConfig) -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(&escurel_index::snapshot::install_load_sql(cfg))
        .unwrap();
    conn.execute_batch(&escurel_index::snapshot::attach_sql(cfg, true).unwrap())
        .unwrap();
    conn
}

fn lake_has_page(cfg: &LakeConfig, page_id: &str) -> bool {
    let conn = reader_conn(cfg);
    let count: i64 = conn
        .query_row(
            "SELECT count(*) FROM lake.pages WHERE page_id = ?",
            [page_id],
            |r| r.get(0),
        )
        .unwrap();
    count > 0
}

struct Setup {
    process: EscurelProcess,
    lake: LakeConfig,
    _dirs: Vec<TempDir>,
}

/// Builds an indexer with the lake already attached (mirroring writer
/// boot's idempotent `ATTACH`) so `state.lake` and the indexer's own
/// connection agree, then boots the gateway on top of it.
async fn setup() -> Setup {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let lake_dir = TempDir::new().unwrap();
    std::fs::create_dir_all(lake_dir.path().join("data")).unwrap();
    let lake = lake_config(&lake_dir);

    let store: Arc<dyn escurel_storage::LaneStore> = Arc::new(escurel_storage::FsStore::new(
        store_dir.path().to_path_buf(),
    ));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Arc::new(Indexer::new(Arc::clone(&store), embedder, conn, TENANT).unwrap());
    indexer
        .update_page("markdown/skills/memo.md", MEMO_SKILL)
        .await
        .unwrap();
    indexer.attach_lake(&lake).await.expect("attach lake");

    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            indexer: Some(indexer),
            lake: Some(lake.clone()),
            ..Default::default()
        },
        ..Default::default()
    })
    .await;

    Setup {
        process,
        lake,
        _dirs: vec![store_dir, db_dir, lake_dir],
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
async fn materialised_upload_is_durable_in_the_lake_before_the_response() {
    let s = setup().await;
    let token = s.process.mint_token(TENANT, Role::Agent);

    let (status, body) = post_upload(&s.process, &token, "text/plain", b"a short memo body").await;
    assert_eq!(status, StatusCode::ACCEPTED, "upload: {body}");
    assert_eq!(body["status"], "materialised", "body: {body}");
    let page_id = body["page_id"]
        .as_str()
        .expect("page_id in response")
        .to_owned();

    // No publish_snapshot call anywhere in this test, and no periodic
    // publish task exists in this harness at all. If the page is in
    // the lake, only run_document_ingest's synchronous publish could
    // have put it there.
    assert!(
        lake_has_page(&s.lake, &page_id),
        "a materialised upload's 202 must mean durable in the lake, with \
         no publish_snapshot call and no periodic publish task"
    );

    s.process.shutdown().await;
}

#[tokio::test]
async fn extraction_failed_overlay_is_durable_too() {
    let s = setup().await;
    let token = s.process.mint_token(TENANT, Role::Agent);

    // `text/plain` routes to the memo skill, but PlainTextExtractor's
    // `extract` requires valid UTF-8 (`document.rs`) — invalid bytes
    // force the extraction_failed branch while still resolving a
    // handler.
    let (status, body) = post_upload(&s.process, &token, "text/plain", &[0xff, 0xfe, 0x00]).await;
    assert_eq!(status, StatusCode::ACCEPTED, "upload: {body}");
    assert_eq!(body["status"], "extraction_failed", "body: {body}");
    let page_id = body["page_id"]
        .as_str()
        .expect("page_id in response")
        .to_owned();

    assert!(
        lake_has_page(&s.lake, &page_id),
        "an extraction_failed overlay is still a real page write and must be \
         durable in the lake before the response, same as a successful one"
    );

    s.process.shutdown().await;
}
