//! Source files retained as evidence: XML/JSON extraction, retained
//! workbooks, MIME aliases, failed-extraction retry, event settlement and
//! the MCP upload bridge. Every behaviour is declared by the document skill
//! (`backend.retain`, `backend.event_kind`, `backend.settle_event`) or is a
//! property of the content, never of a skill name.
//!
//! Real gateway + real Indexer + real DuckDB + real OIDC over real HTTP.

use std::sync::Arc;

use base64::Engine as _;
use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{EventKind, Indexer, Migrator};
use escurel_storage::{FsStore, LaneStore};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, Opts, Role};
use serde_json::{Value, json};
use tempfile::TempDir;

const TENANT: &str = "sources";
const XLSX: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";

// Declares everything under test.
const EVIDENCE_SKILL: &str = "\
---
kind: skill
id: evidence
description: Original files retained as evidence.
owner_field: author
acl:
  read: [public]
  create: [owner]
backend:
  kind: document
  accepts: [text/plain, text/xml, application/xml, application/json, application/vnd.openxmlformats-officedocument.spreadsheetml.sheet]
  retain: [application/vnd.openxmlformats-officedocument.spreadsheetml.sheet]
  event_kind: system
  settle_event: true
---
# evidence
";

// Control: declares none of it.
const NOTE_SKILL: &str = "\
---
kind: skill
id: note
description: Plain notes.
owner_field: author
acl:
  read: [public]
  create: [owner]
backend:
  kind: document
  accepts: [text/markdown]
---
# note
";

struct Setup {
    process: EscurelProcess,
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
    let indexer = Arc::new(Indexer::new(store, embedder, conn, TENANT).unwrap());
    for (id, body) in [("evidence", EVIDENCE_SKILL), ("note", NOTE_SKILL)] {
        indexer
            .update_page(&format!("markdown/skills/{id}.md"), body)
            .await
            .unwrap();
    }
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
        indexer,
        _dirs: vec![store_dir, db_dir],
    }
}

fn args(bytes: &[u8], ct: &str, skill: &str, title: Option<&str>, event: Option<&str>) -> Value {
    let mut body = json!({
        "content_type": ct,
        "bytes_b64": base64::engine::general_purpose::STANDARD.encode(bytes),
        "skill": skill,
    });
    if let Some(t) = title {
        body["title"] = json!(t);
    }
    if let Some(e) = event {
        body["event_id"] = json!(e);
    }
    body
}

async fn upload(p: &EscurelProcess, token: &str, body: Value) -> (reqwest::StatusCode, Value) {
    let resp = reqwest::Client::new()
        .post(format!("{}/ingest/upload", p.base_url()))
        .header("authorization", format!("Bearer {token}"))
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = resp.status();
    (status, resp.json().await.unwrap())
}

async fn rpc(p: &EscurelProcess, token: &str, method: &str, params: Value) -> Value {
    reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

async fn call(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    let body = rpc(
        p,
        token,
        "tools/call",
        json!({"name": name, "arguments": args}),
    )
    .await;
    body["result"]["structuredContent"].clone()
}

#[tokio::test]
async fn xml_and_json_are_extracted_and_invalid_json_keeps_its_original() {
    let s = setup().await;
    let token = s.process.mint_token(TENANT, Role::Agent);

    let xml = b"<?xml version=\"1.0\"?><process name=\"Order intake\"/>";
    let (st, r) = upload(
        &s.process,
        &token,
        args(xml, "application/xml", "evidence", None, None),
    )
    .await;
    assert_eq!(st, 202, "{r}");
    assert_eq!(r["status"], "materialised", "{r}");
    assert!(
        r["chunk_count"].as_u64().unwrap() > 0,
        "XML text is chunked: {r}"
    );

    let json_bytes = br#"{"capability":"Planning"}"#;
    let (st, r) = upload(
        &s.process,
        &token,
        args(json_bytes, "application/json", "evidence", None, None),
    )
    .await;
    assert_eq!(st, 202, "{r}");
    assert_eq!(r["status"], "materialised", "{r}");
    assert!(r["chunk_count"].as_u64().unwrap() > 0, "{r}");

    // A MIME with parameters reaches the same extractor.
    let (st, r) = upload(
        &s.process,
        &token,
        args(
            b"<a>x</a>",
            "text/xml; charset=utf-8",
            "evidence",
            None,
            None,
        ),
    )
    .await;
    assert_eq!(st, 202, "{r}");
    assert_eq!(r["status"], "materialised", "{r}");

    // Invalid JSON and NUL-bearing XML retain the original and report failure.
    let (st, r) = upload(
        &s.process,
        &token,
        args(b"{broken", "application/json", "evidence", None, None),
    )
    .await;
    assert_eq!(st, 202, "{r}");
    assert_eq!(r["status"], "extraction_failed", "{r}");
    let blob = r["blob_id"].as_str().unwrap();
    assert!(blob.starts_with("sha256:"), "{r}");
    let (st, r) = upload(
        &s.process,
        &token,
        args(b"<a>\0</a>", "application/xml", "evidence", None, None),
    )
    .await;
    assert_eq!(st, 202, "{r}");
    assert_eq!(r["status"], "extraction_failed", "{r}");

    // CONTROL: a skill that does not accept the MIME still refuses it.
    let (st, r) = upload(
        &s.process,
        &token,
        args(xml, "application/xml", "note", None, None),
    )
    .await;
    assert_eq!(st, 422, "{r}");
}

#[tokio::test]
async fn a_declared_retain_mime_keeps_bytes_without_extraction() {
    let s = setup().await;
    let token = s.process.mint_token(TENANT, Role::Agent);
    // Not a parsable workbook: retention must not depend on parsing.
    let bytes = b"PK\x03\x04 not a real workbook \x00\x01\x02";
    let (st, r) = upload(
        &s.process,
        &token,
        args(bytes, XLSX, "evidence", Some("Q1"), None),
    )
    .await;
    assert_eq!(st, 202, "{r}");
    assert_eq!(r["status"], "materialised", "{r}");
    assert_eq!(r["chunk_count"], 0, "retained, not chunked: {r}");
    let page_id = r["page_id"].as_str().unwrap();
    let blob = call(
        &s.process,
        &token,
        "fetch_blob",
        json!({"page_id": page_id}),
    )
    .await;
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(blob["blob"]["bytes_base64"].as_str().unwrap_or_default())
            .unwrap_or_default(),
        bytes,
        "the original bytes are fetchable verbatim: {blob}"
    );

    // Replaying the same bytes is idempotent.
    let (st, r2) = upload(
        &s.process,
        &token,
        args(bytes, XLSX, "evidence", Some("Q1"), None),
    )
    .await;
    assert_eq!(st, 200, "{r2}");
    assert_eq!(r2["status"], "existing_document", "{r2}");
}

#[tokio::test]
async fn equivalent_mime_spellings_reuse_the_document_others_conflict() {
    let s = setup().await;
    let token = s.process.mint_token(TENANT, Role::Agent);
    let xml = b"<a>x</a>";
    let (st, _) = upload(
        &s.process,
        &token,
        args(xml, "text/xml", "evidence", None, None),
    )
    .await;
    assert_eq!(st, 202);
    let (st, r) = upload(
        &s.process,
        &token,
        args(xml, "application/xml", "evidence", None, None),
    )
    .await;
    assert_eq!(
        st, 200,
        "text/xml and application/xml name one media type: {r}"
    );
    assert_eq!(r["status"], "existing_document", "{r}");

    let json_bytes = br#"{"a":1}"#;
    let (st, _) = upload(
        &s.process,
        &token,
        args(json_bytes, "text/plain", "evidence", None, None),
    )
    .await;
    assert_eq!(st, 202);
    let (st, r) = upload(
        &s.process,
        &token,
        args(json_bytes, "application/json", "evidence", None, None),
    )
    .await;
    assert_eq!(st, 200, "valid JSON read as text is the same document: {r}");

    // CONTROL: bytes that are not valid JSON are not aliased.
    let text = b"not json at all";
    let (st, _) = upload(
        &s.process,
        &token,
        args(text, "text/plain", "evidence", None, None),
    )
    .await;
    assert_eq!(st, 202);
    let (st, r) = upload(
        &s.process,
        &token,
        args(text, "application/json", "evidence", None, None),
    )
    .await;
    assert_eq!(st, 409, "{r}");
    assert_eq!(r["error"], "document_mime_conflict", "{r}");
    // CONTROL: unrelated MIMEs conflict.
    let (st, r) = upload(
        &s.process,
        &token,
        args(xml, "text/plain", "evidence", None, None),
    )
    .await;
    assert_eq!(st, 409, "{r}");
}

#[tokio::test]
async fn a_failed_extraction_is_retried_with_its_title_and_owner() {
    let s = setup().await;
    let alice = s.process.mint_token_with_sub(TENANT, Role::Agent, "alice");
    let bob = s.process.mint_token_with_sub(TENANT, Role::Agent, "bob");
    let bad = b"{broken";
    let (st, first) = upload(
        &s.process,
        &alice,
        args(bad, "application/json", "evidence", Some("Plan"), None),
    )
    .await;
    assert_eq!(st, 202, "{first}");
    assert_eq!(first["status"], "extraction_failed", "{first}");
    let page_id = first["page_id"].as_str().unwrap().to_owned();

    // Another caller repeats it: reprocessed, but still alice's, still "Plan".
    let (st, again) = upload(
        &s.process,
        &bob,
        args(bad, "application/json", "evidence", Some("Other"), None),
    )
    .await;
    assert_eq!(st, 202, "retry reprocesses instead of conflicting: {again}");
    assert_eq!(again["status"], "extraction_failed", "{again}");
    assert_eq!(again["page_id"], page_id.as_str());
    let page = s
        .indexer
        .read_page_markdown(&page_id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        page.contains("author: \"alice\""),
        "owner preserved:\n{page}"
    );
    assert!(page.contains("title: \"Plan\""), "title preserved:\n{page}");
    assert!(page.contains("status: extraction_failed"), "{page}");

    // CONTROL: a different declared MIME for the same bytes is a conflict.
    let (st, r) = upload(
        &s.process,
        &alice,
        args(bad, "text/plain", "evidence", None, None),
    )
    .await;
    assert_eq!(st, 409, "{r}");
}

#[tokio::test]
async fn a_declared_event_is_system_kind_and_settled_on_the_document() {
    let s = setup().await;
    let token = s.process.mint_token(TENANT, Role::Agent);
    let (st, r) = upload(
        &s.process,
        &token,
        args(
            b"<a>x</a>",
            "application/xml",
            "evidence",
            None,
            Some("ev-1"),
        ),
    )
    .await;
    assert_eq!(st, 202, "{r}");
    let page_id = r["page_id"].as_str().unwrap();
    let ev = s.indexer.get_event("ev-1").await.unwrap().unwrap();
    assert_eq!(ev.kind, EventKind::System, "{ev:?}");
    assert_eq!(ev.status, "processed", "{ev:?}");
    assert_eq!(ev.instance_page_id.as_deref(), Some(page_id), "{ev:?}");

    // A redelivery of the same upload is a duplicate and stays settled.
    let (st, dup) = upload(
        &s.process,
        &token,
        args(
            b"<a>x</a>",
            "application/xml",
            "evidence",
            None,
            Some("ev-1"),
        ),
    )
    .await;
    assert_eq!(st, 200, "{dup}");
    assert_eq!(dup["status"], "duplicate", "{dup}");

    // CONTROL: a skill that declares nothing leaves a user event in the inbox.
    let (st, r) = upload(
        &s.process,
        &token,
        args(b"# hi", "text/markdown", "note", None, Some("ev-2")),
    )
    .await;
    assert_eq!(st, 202, "{r}");
    let ev = s.indexer.get_event("ev-2").await.unwrap().unwrap();
    assert_eq!(ev.kind, EventKind::User, "{ev:?}");
    assert_eq!(ev.status, "inbox", "{ev:?}");
}

#[tokio::test]
async fn an_interrupted_upload_is_completed_and_settled_on_replay() {
    use sha2::{Digest, Sha256};
    let s = setup().await;
    let token = s.process.mint_token_with_sub(TENANT, Role::Agent, "alice");
    let bytes = b"<a>y</a>";
    let blob_id = format!("sha256:{:x}", Sha256::digest(bytes));
    // The first attempt captured its event and then died before it wrote the
    // document: the event is in the inbox, no page exists.
    s.indexer
        .capture_event(escurel_index::NewEvent {
            event_id: Some("ev-3".to_owned()),
            source: "ingest".to_owned(),
            mime: "application/xml".to_owned(),
            label_skill: "evidence".to_owned(),
            kind: EventKind::System,
            title: blob_id.clone(),
            provenance: Some(json!({
                "blob_id": blob_id, "content_type": "application/xml",
                "handler_skill": "evidence", "by": "alice",
                escurel_index::CAPTURED_BY_FIELD: "alice",
            })),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(
        s.indexer.get_event("ev-3").await.unwrap().unwrap().status,
        "inbox"
    );
    let (st, r) = upload(
        &s.process,
        &token,
        args(bytes, "application/xml", "evidence", None, Some("ev-3")),
    )
    .await;
    assert!(st.is_success(), "{r}");
    let ev = s.indexer.get_event("ev-3").await.unwrap().unwrap();
    assert_eq!(ev.status, "processed", "{ev:?}");
    assert!(ev.instance_page_id.is_some(), "{ev:?}");
}

#[tokio::test]
async fn the_mcp_bridge_uploads_and_reports_refusals_in_the_envelope() {
    let s = setup().await;
    let token = s.process.mint_token(TENANT, Role::Agent);

    let list = rpc(&s.process, &token, "tools/list", json!({})).await;
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert!(names.contains(&"ingest_upload"), "{names:?}");

    let ok = call(
        &s.process,
        &token,
        "ingest_upload",
        args(
            b"<a>z</a>",
            "application/xml",
            "evidence",
            Some("Z"),
            Some("ev-mcp"),
        ),
    )
    .await;
    assert_eq!(ok["status"], "materialised", "{ok}");
    let page_id = ok["page_id"].as_str().unwrap();
    let blob = call(
        &s.process,
        &token,
        "fetch_blob",
        json!({"page_id": page_id}),
    )
    .await;
    assert!(blob["blob"]["bytes_base64"].is_string(), "{blob}");

    // A refusal is a tool result with issues, carrying the HTTP status.
    let refused = call(
        &s.process,
        &token,
        "ingest_upload",
        args(b"<a/>", "application/xml", "note", None, None),
    )
    .await;
    assert_eq!(refused["ok"], false, "{refused}");
    assert_eq!(
        refused["issues"][0]["code"], "invalid_target_skill",
        "{refused}"
    );
    assert_eq!(refused["issues"][0]["http_status"], 422, "{refused}");

    // CONTROL: no token, no upload.
    let anon = reqwest::Client::new()
        .post(s.process.mcp_url())
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
            "params":{"name":"ingest_upload","arguments": args(b"x","text/plain","evidence",None,None)}}))
        .send().await.unwrap();
    assert_eq!(anon.status(), 401);
}
