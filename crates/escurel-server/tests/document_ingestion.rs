//! E2E for the document ingestion pipeline (PR-3d): /ingest → worker →
//! materialise → searchable. Real gateway + DuckDB + OIDC, real reqwest,
//! born-digital text via PlainTextExtractor (no kreuzberg, fully offline).

use std::sync::Arc;

use base64::Engine as _;
use bytes::Bytes;
use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_storage::{BlobId, FsStore, LaneStore};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, Opts, Role};
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
  accepts: [text/plain]
  chunk: { max_chars: 40, overlap: 8 }
---
# memo
";

struct Setup {
    process: EscurelProcess,
    store: Arc<dyn LaneStore>,
    indexer: Arc<Indexer>,
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
            indexer: Some(Arc::clone(&indexer)),
            ..Default::default()
        },
        ..Default::default()
    })
    .await;
    Setup {
        process,
        store,
        indexer,
        _dirs: vec![store_dir, db_dir],
    }
}

async fn call(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": args },
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

async fn post_ingest(p: &EscurelProcess, token: &str, blob_id: &str, ct: &str) -> Value {
    reqwest::Client::new()
        .post(format!("{}/ingest", p.base_url()))
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({ "blob_id": blob_id, "content_type": ct }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

#[tokio::test]
async fn ingest_text_end_to_end_materialises_searchable_instance() {
    let s = setup().await;
    let token = s.process.mint_token(TENANT, Role::Agent);
    let body = "The zephyr proposal covers Q3 logistics across three regions in detail.";
    let blob = s
        .store
        .put_inbox_blob(TENANT, Bytes::from_static(body.as_bytes()), None)
        .await
        .unwrap();

    let resp = post_ingest(&s.process, &token, blob.as_str(), "text/plain").await;
    assert_eq!(resp["status"], "materialised", "resp: {resp}");
    let page_id = resp["page_id"].as_str().expect("page_id").to_owned();
    assert!(resp["chunk_count"].as_u64().unwrap() >= 1);

    // expand: overlay carries the document backend_ref + the chunk blocks.
    let ex = call(&s.process, &token, "expand", json!({ "page_id": page_id })).await;
    let page = &ex["result"]["structuredContent"];
    assert_eq!(page["frontmatter"]["backend_ref"]["kind"], "document");
    assert_eq!(page["frontmatter"]["backend_ref"]["status"], "ok");
    assert!(
        !page["blocks"].as_array().unwrap().is_empty(),
        "chunks must be indexed as blocks: {page}"
    );

    // The original blob was promoted to the canonical area.
    assert!(s.indexer.read_blob(&blob).await.is_ok());

    // search finds the document via its chunks.
    s.indexer.refresh_fts().await.unwrap();
    let hits = call(
        &s.process,
        &token,
        "search",
        json!({ "q": "zephyr", "k": 10 }),
    )
    .await;
    let ids: Vec<String> = hits["result"]["structuredContent"]["hits"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|h| h["page_id"].as_str().map(str::to_owned))
        .collect();
    assert!(
        ids.iter().any(|p| p.contains("memo/doc-")),
        "search miss: {ids:?}"
    );

    s.process.shutdown().await;
}

#[tokio::test]
async fn fetch_blob_returns_original_bytes_for_a_document_instance() {
    // The retained original file is fetchable byte-for-byte via fetch_blob
    // (for a faithful client-side preview), with a sniffed content type.
    let s = setup().await;
    let token = s.process.mint_token(TENANT, Role::Agent);
    let body = "The original bytes of the source document, verbatim.";
    let blob = s
        .store
        .put_inbox_blob(TENANT, Bytes::from_static(body.as_bytes()), None)
        .await
        .unwrap();
    let resp = post_ingest(&s.process, &token, blob.as_str(), "text/plain").await;
    let page_id = resp["page_id"].as_str().unwrap().to_owned();

    let r = call(
        &s.process,
        &token,
        "fetch_blob",
        json!({ "page_id": page_id }),
    )
    .await;
    let got = &r["result"]["structuredContent"]["blob"];
    assert_eq!(got["content_type"], "text/plain", "sniffed type: {r}");
    assert_eq!(got["size"].as_u64().unwrap(), body.len() as u64);
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(got["bytes_base64"].as_str().unwrap())
        .unwrap();
    assert_eq!(decoded, body.as_bytes(), "bytes must round-trip verbatim");

    s.process.shutdown().await;
}

#[tokio::test]
async fn fetch_blob_is_null_for_missing_or_non_document_page() {
    let s = setup().await;
    let token = s.process.mint_token(TENANT, Role::Agent);
    // A skill page (catalogue, not a document instance) → null blob.
    let r = call(
        &s.process,
        &token,
        "fetch_blob",
        json!({ "page_id": "markdown/skills/memo.md" }),
    )
    .await;
    assert!(r["result"]["structuredContent"]["blob"].is_null(), "{r}");

    // A page that does not exist → null blob.
    let r2 = call(
        &s.process,
        &token,
        "fetch_blob",
        json!({ "page_id": "markdown/instances/memo/nope.md" }),
    )
    .await;
    assert!(r2["result"]["structuredContent"]["blob"].is_null(), "{r2}");
    s.process.shutdown().await;
}

#[tokio::test]
async fn extraction_failure_retains_blob_and_marks_instance() {
    let s = setup().await;
    let token = s.process.mint_token(TENANT, Role::Agent);
    // Invalid UTF-8 → PlainTextExtractor fails; the blob must be retained.
    let blob = s
        .store
        .put_inbox_blob(TENANT, Bytes::from_static(&[0xff, 0xfe, 0x00, 0x01]), None)
        .await
        .unwrap();

    let resp = post_ingest(&s.process, &token, blob.as_str(), "text/plain").await;
    assert_eq!(resp["status"], "extraction_failed", "resp: {resp}");
    assert_eq!(resp["issue"]["code"], "extraction_failed");

    // The inbox blob is retained (never lost), NOT promoted to canonical.
    assert!(
        s.indexer.read_inbox_blob(&blob).await.is_ok(),
        "inbox blob retained"
    );
    assert!(
        s.indexer.read_blob(&blob).await.is_err(),
        "not promoted on failure"
    );

    // The instance exists, marked extraction_failed, with no chunks.
    let page_id = resp["page_id"].as_str().unwrap();
    let ex = call(&s.process, &token, "expand", json!({ "page_id": page_id })).await;
    let page = &ex["result"]["structuredContent"];
    assert_eq!(
        page["frontmatter"]["backend_ref"]["status"],
        "extraction_failed"
    );
    assert!(page["blocks"].as_array().unwrap().is_empty());

    s.process.shutdown().await;
}

#[tokio::test]
async fn expand_returns_bounded_chunk_lead_not_full_text() {
    let s = setup().await;
    let token = s.process.mint_token(TENANT, Role::Agent);
    // A long body → many small chunks (skill chunk.max_chars = 40).
    let body = "lorem ipsum dolor sit amet ".repeat(30);
    let blob = s
        .store
        .put_inbox_blob(TENANT, Bytes::from(body.into_bytes()), None)
        .await
        .unwrap();
    let resp = post_ingest(&s.process, &token, blob.as_str(), "text/plain").await;
    let page_id = resp["page_id"].as_str().unwrap().to_owned();
    let total = resp["chunk_count"].as_u64().unwrap();
    assert!(
        total > 8,
        "need >8 chunks to exercise truncation; got {total}"
    );

    let ex = call(&s.process, &token, "expand", json!({ "page_id": page_id })).await;
    let page = &ex["result"]["structuredContent"];
    // expand returns a bounded lead, never the full chunk set (REQ-DOC-05).
    assert!(page["blocks"].as_array().unwrap().len() <= 8);
    assert_eq!(page["chunks_truncated"], true);
    assert_eq!(page["chunks_total"].as_u64().unwrap(), total);
    s.process.shutdown().await;
}

#[tokio::test]
async fn update_page_on_document_instance_rejected() {
    // Documents are pipeline-managed; update_page must not clobber their
    // chunks (backend_read_only).
    let s = setup().await;
    let token = s.process.mint_token(TENANT, Role::Agent);
    let blob = s
        .store
        .put_inbox_blob(TENANT, Bytes::from_static(b"some memo text"), None)
        .await
        .unwrap();
    let resp = post_ingest(&s.process, &token, blob.as_str(), "text/plain").await;
    let page_id = resp["page_id"].as_str().unwrap().to_owned();

    let content = "---\ntype: instance\nskill: memo\nid: doc-x\nbackend_ref:\n  kind: document\n---\n# edited\n";
    let body = call(
        &s.process,
        &token,
        "update_page",
        json!({ "page_id": page_id, "content": content }),
    )
    .await;
    let r = &body["result"]["structuredContent"];
    assert_eq!(r["ok"], false, "must be rejected: {body}");
    assert_eq!(r["issues"][0]["code"], "backend_read_only");
    s.process.shutdown().await;
}

#[tokio::test]
async fn ingest_is_idempotent_on_content_hash() {
    // Same bytes → same BlobId → same instance id → re-ingest overwrites.
    let s = setup().await;
    let token = s.process.mint_token(TENANT, Role::Agent);
    let body = "idempotent body";
    let blob = s
        .store
        .put_inbox_blob(TENANT, Bytes::from(body.as_bytes().to_vec()), None)
        .await
        .unwrap();
    let id = blob.as_str().to_owned();
    let r1 = post_ingest(&s.process, &token, &id, "text/plain").await;
    // re-deposit (idempotent put) + re-ingest.
    s.store
        .put_inbox_blob(TENANT, Bytes::from(body.as_bytes().to_vec()), None)
        .await
        .unwrap();
    let r2 = post_ingest(&s.process, &token, &id, "text/plain").await;
    assert_eq!(r1["page_id"], r2["page_id"], "same content → same instance");

    let _ = BlobId::parse(&id).expect("valid id");
    s.process.shutdown().await;
}

const REPORT_SKILL: &str = "\
---
type: skill
id: report
description: PDF reports.
backend:
  kind: document
  accepts: [application/pdf]
---
# report
";

/// Seed a PDF-accepting document skill on the shared indexer.
async fn seed_report_skill(s: &Setup) {
    s.indexer
        .update_page("markdown/skills/report.md", REPORT_SKILL)
        .await
        .unwrap();
}

#[tokio::test]
async fn ingest_pdf_without_kreuzberg_feature_fails_gracefully() {
    // /ingest must ROUTE by content type. Without the kreuzberg feature there
    // is no PDF extractor, so a PDF must fail closed (extraction_failed, blob
    // retained) — not be silently mis-extracted as text. This is the wiring
    // the "always-PlainText" bug broke.
    let s = setup().await;
    seed_report_skill(&s).await;
    let token = s.process.mint_token(TENANT, Role::Agent);
    let pdf = include_bytes!("fixtures/report.pdf");
    let blob = s
        .store
        .put_inbox_blob(TENANT, Bytes::from_static(pdf), None)
        .await
        .unwrap();
    let resp = post_ingest(&s.process, &token, blob.as_str(), "application/pdf").await;
    assert_eq!(
        resp["handler_skill"], "report",
        "must route to the pdf skill: {resp}"
    );
    #[cfg(not(feature = "kreuzberg"))]
    {
        assert_eq!(
            resp["status"], "extraction_failed",
            "no PDF extractor without the feature: {resp}"
        );
        // The original is retained for reprocessing once the feature is on.
        assert!(s.indexer.read_inbox_blob(&blob).await.is_ok());
    }
    #[cfg(feature = "kreuzberg")]
    {
        assert_eq!(
            resp["status"], "materialised",
            "kreuzberg should extract the PDF: {resp}"
        );
        assert!(resp["chunk_count"].as_u64().unwrap() >= 1);
    }
    s.process.shutdown().await;
}

const DOCX_MIME: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";
const DOCX_SKILL: &str = "\
---
type: skill
id: worddoc
description: Word documents.
backend:
  kind: document
  accepts: [application/vnd.openxmlformats-officedocument.wordprocessingml.document]
---
# worddoc
";

#[cfg(feature = "kreuzberg")]
#[tokio::test]
async fn ingest_docx_end_to_end_materialises_searchable_instance() {
    // The full /ingest path for a real DOCX under the (default-on) kreuzberg
    // extractor — not just the unit-level kreuzberg_extract.rs. Routes by MIME,
    // extracts, chunks, materialises, and the chunks are reachable via expand.
    let s = setup().await;
    s.indexer
        .update_page("markdown/skills/worddoc.md", DOCX_SKILL)
        .await
        .unwrap();
    let token = s.process.mint_token(TENANT, Role::Agent);
    let docx = include_bytes!("fixtures/memo.docx");
    let blob = s
        .store
        .put_inbox_blob(TENANT, Bytes::from_static(docx), None)
        .await
        .unwrap();

    let resp = post_ingest(&s.process, &token, blob.as_str(), DOCX_MIME).await;
    assert_eq!(resp["handler_skill"], "worddoc", "route by MIME: {resp}");
    assert_eq!(resp["status"], "materialised", "kreuzberg DOCX: {resp}");
    let chunks = resp["chunk_count"].as_u64().unwrap();
    assert!(chunks >= 1, "DOCX must yield chunks: {resp}");

    // The materialised instance is a read-only document overlay reachable via
    // expand, carrying the kreuzberg extract engine in its backend_ref.
    let page_id = resp["page_id"].as_str().unwrap();
    let ex = call(&s.process, &token, "expand", json!({ "page_id": page_id })).await;
    let page = &ex["result"]["structuredContent"];
    assert_eq!(page["frontmatter"]["backend_ref"]["kind"], "document");
    assert!(
        page["frontmatter"]["backend_ref"]["extract_engine"]
            .as_str()
            .is_some_and(|e| e.contains("kreuzberg")),
        "DOCX must record the kreuzberg engine: {page}"
    );
    s.process.shutdown().await;
}

#[tokio::test]
async fn list_skills_reports_document_kind_and_capabilities() {
    // G4 uniform surface for the document backend (parallel to the sql_view
    // capability test): read-only, block-grain, hybrid search, overlay-CRDT.
    let s = setup().await;
    let token = s.process.mint_token(TENANT, Role::Agent);
    let r = call(&s.process, &token, "list_skills", json!({})).await;
    let skills = r["result"]["structuredContent"]["skills"]
        .as_array()
        .unwrap();
    let memo = skills
        .iter()
        .find(|sk| sk["id"] == "memo")
        .expect("memo skill");
    assert_eq!(memo["backend"]["kind"], "document");
    let caps = &memo["capabilities"];
    assert_eq!(caps["writable"], false);
    assert_eq!(caps["granularity"], "block");
    assert_eq!(caps["search"], "hybrid");
    assert_eq!(caps["supports_crdt"], true);
    s.process.shutdown().await;
}
