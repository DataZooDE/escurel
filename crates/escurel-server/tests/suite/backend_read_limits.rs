//! E2E for the *configurable* read-display caps (Phase-1.2): a `sql_view`
//! skill's `projection_limit` bounds the rows `expand` returns (and sets
//! `backend_projection.truncated`), and a `document` skill's `lead_chunks`
//! bounds the chunk lead `expand` returns (and sets `chunks_truncated`).
//! Real gateway + DuckDB + FsStore, real reqwest, offline json_dir source +
//! born-digital text. No mocks.

use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::backend::{SqlConnector, SqlViewBackend, SqlViewBinding};
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, LaneStore};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, Opts};
use serde_json::{Value, json};
use tempfile::TempDir;

const TENANT: &str = "acme";

// A sql_view skill that caps its read projection at 2 rows.
fn sql_skill_md(data_dir: &str) -> String {
    format!(
        "---\n\
         type: skill\n\
         id: customers\n\
         description: EU customers, mirrored read-only.\n\
         backend:\n\
        \x20 kind: sql_view\n\
        \x20 projection_limit: 2\n\
        \x20 source:\n\
        \x20   connector: json_dir\n\
        \x20   relation: {data_dir}\n\
        \x20 project:\n\
        \x20   name: name\n\
        \x20 search_text: [name]\n\
         ---\n\
         # customers\n"
    )
}

// A document skill: tiny chunks (so a short body yields many) + a 2-chunk lead.
const DOC_SKILL_MD: &str = "\
---
type: skill
id: memo
description: Text memos ingested as documents.
backend:
  kind: document
  accepts: [text/plain]
  lead_chunks: 2
  chunk: { max_chars: 40, overlap: 0 }
---
# memo
";

struct Setup {
    process: EscurelProcess,
    sql_page_id: String,
    _dirs: Vec<TempDir>,
}

async fn setup() -> Setup {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let data_dir = TempDir::new().unwrap();
    // Three source rows — more than the projection_limit of 2.
    for (f, body) in [
        ("a.json", r#"{"name":"Acme"}"#),
        ("b.json", r#"{"name":"Globex"}"#),
        ("c.json", r#"{"name":"Initech"}"#),
    ] {
        std::fs::write(data_dir.path().join(f), body).unwrap();
    }

    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Arc::new(Indexer::new(store, embedder, conn, TENANT).unwrap());

    indexer
        .update_page(
            "markdown/skills/customers.md",
            &sql_skill_md(data_dir.path().to_str().unwrap()),
        )
        .await
        .unwrap();
    indexer
        .update_page("markdown/skills/memo.md", DOC_SKILL_MD)
        .await
        .unwrap();

    let binding = SqlViewBinding {
        connector: SqlConnector::JsonDir,
        attach: None,
        relation: data_dir.path().to_str().unwrap().to_owned(),
        filter: None,
        project: [("name".to_owned(), "name".to_owned())]
            .into_iter()
            .collect(),
        search_text: vec!["name".to_owned()],
    };
    let m = SqlViewBackend::new(Arc::clone(&indexer))
        .create_instance("customers", &binding, "eu", "# EU customers")
        .await
        .unwrap();

    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::Disabled,
        config_overrides: ConfigOverrides {
            indexer: Some(indexer),
            ..Default::default()
        },
        ..Default::default()
    })
    .await;

    Setup {
        process,
        sql_page_id: m.page_id,
        _dirs: vec![store_dir, db_dir, data_dir],
    }
}

async fn call(p: &EscurelProcess, name: &str, args: Value) -> Value {
    reqwest::Client::new()
        .post(p.mcp_url())
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": { "name": name, "arguments": args } }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json")
}

#[tokio::test]
async fn sql_view_projection_honours_skill_projection_limit() {
    let s = setup().await;
    let body = call(&s.process, "expand", json!({ "page_id": s.sql_page_id })).await;
    let proj = &body["result"]["structuredContent"]["backend_projection"];
    let rows = proj["rows"].as_array().expect("projection rows");
    assert_eq!(
        rows.len(),
        2,
        "projection_limit:2 must cap rows at 2: {proj}"
    );
    assert_eq!(
        proj["truncated"],
        json!(true),
        "more source rows than the limit must flag truncated: {proj}"
    );
    s.process.shutdown().await;
}

#[tokio::test]
async fn document_expand_honours_skill_lead_chunks() {
    let s = setup().await;
    // A body that chunks (max_chars:40) into clearly more than 2 chunks.
    let body_text = "Clause one is the first. \
        Clause two is the second. \
        Clause three is the third. \
        Clause four is the fourth. \
        Clause five is the fifth.";
    let resp = reqwest::Client::new()
        .post(format!("{}/ingest/upload", s.process.base_url()))
        .json(&json!({ "content_type": "text/plain", "bytes_b64": B64.encode(body_text) }))
        .send()
        .await
        .expect("upload");
    let out: Value = resp.json().await.unwrap();
    assert_eq!(out["status"], "materialised", "ingest: {out}");
    let page_id = out["page_id"].as_str().expect("page_id").to_owned();

    let ex = call(&s.process, "expand", json!({ "page_id": page_id })).await;
    let page = &ex["result"]["structuredContent"];
    let blocks = page["blocks"].as_array().expect("blocks");
    assert_eq!(
        blocks.len(),
        2,
        "lead_chunks:2 must cap the lead at 2: {page}"
    );
    assert!(
        page["chunks_total"].as_u64().unwrap() > 2,
        "the document must have produced >2 chunks total: {page}"
    );
    assert_eq!(
        page["chunks_truncated"],
        json!(true),
        "more chunks than the lead must flag chunks_truncated: {page}"
    );
    s.process.shutdown().await;
}

#[tokio::test]
async fn document_expand_full_returns_all_chunks() {
    let s = setup().await;
    // Same multi-chunk body as the lead-chunks test (lead_chunks:2).
    let body_text = "Clause one is the first. \
        Clause two is the second. \
        Clause three is the third. \
        Clause four is the fourth. \
        Clause five is the fifth.";
    let resp = reqwest::Client::new()
        .post(format!("{}/ingest/upload", s.process.base_url()))
        .json(&json!({ "content_type": "text/plain", "bytes_b64": B64.encode(body_text) }))
        .send()
        .await
        .expect("upload");
    let out: Value = resp.json().await.unwrap();
    assert_eq!(out["status"], "materialised", "ingest: {out}");
    let page_id = out["page_id"].as_str().expect("page_id").to_owned();

    // `full: true` bypasses the lead cap (the detail/heatmap view needs every
    // chunk) — returns all chunks_total blocks and does NOT flag truncation.
    let ex = call(
        &s.process,
        "expand",
        json!({ "page_id": page_id, "full": true }),
    )
    .await;
    let page = &ex["result"]["structuredContent"];
    let blocks = page["blocks"].as_array().expect("blocks");
    let total = page["chunks_total"].as_u64().expect("chunks_total");
    assert!(total > 2, "the document must have >2 chunks total: {page}");
    assert_eq!(
        blocks.len() as u64,
        total,
        "full=true must return ALL chunks, not the lead: {page}"
    );
    assert_eq!(
        page["chunks_truncated"],
        json!(false),
        "full=true must not flag chunks_truncated: {page}"
    );
    s.process.shutdown().await;
}
