//! Integration tests for Contextual Retrieval, Variant A (structural).
//!
//! Real DuckDB + real FsStore + real `HashEmbedder` + the document ingest
//! worker. No mocks. A document whose *title* carries a distinctive token
//! absent from every chunk body is ingested; after `refresh_fts` the BM25
//! lane must match that title token against the chunk (proving the
//! contextualised text — `[<title> › p.<page>]` prefix — feeds the FTS lane
//! and, since `blocks.body` is the embedded text, the dense lane too).

use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use escurel_embed::{Embedder, HashEmbedder};
use escurel_index::backend::{
    Chunk, ChunkConfig, DocMetadata, DocumentIngestWorker, DocumentProcessor, ExtractConfig,
    ExtractError, ExtractionResult, IngestOutcome, OcrPolicy,
};
use escurel_index::{Indexer, Migrator};
use escurel_storage::{BlobId, FsStore, LaneStore};
use tempfile::TempDir;

const TENANT: &str = "acme";

// The title token `Zephyrology` does NOT appear in the chunk body below; only
// the structural prefix can put it into `blocks.body`.
const TITLE_TOKEN: &str = "Zephyrology";
const DOC_TITLE: &str = "Zephyrology Field Manual";
const CHUNK_BODY: &str = "Wind speeds were measured across three coastal stations during spring.";

const MEMO_SKILL: &str = "\
---
type: skill
id: memo
description: text memos
backend:
  kind: document
  accepts: [text/plain]
---
# memo
";

/// A processor that yields a known title + a single paged chunk, so the test
/// controls exactly what the structural prefix should be built from. (The
/// real `PlainTextExtractor` carries no title; we need one here.)
struct TitledProcessor;

#[async_trait]
impl DocumentProcessor for TitledProcessor {
    fn engine(&self) -> String {
        "titled-test@1".to_owned()
    }
    async fn process(
        &self,
        bytes: &[u8],
        _mime: &str,
        _cfg: &ExtractConfig,
    ) -> Result<ExtractionResult, ExtractError> {
        let content = std::str::from_utf8(bytes).unwrap().to_owned();
        Ok(ExtractionResult {
            metadata: DocMetadata {
                title: Some(DOC_TITLE.to_owned()),
                ..Default::default()
            },
            chunks: vec![Chunk {
                ordinal: 0,
                byte_start: 0,
                byte_end: content.len(),
                page: Some(7),
                text: content.clone(),
            }],
            content,
        })
    }
}

fn indexer_on(store: Arc<dyn LaneStore>, db: &TempDir) -> Arc<Indexer> {
    let conn = duckdb::Connection::open(db.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let embedder: Arc<dyn Embedder> = Arc::new(HashEmbedder::default());
    Arc::new(Indexer::new(store, embedder, conn, TENANT).unwrap())
}

fn cfg() -> ExtractConfig {
    ExtractConfig {
        ocr: OcrPolicy::Off,
        chunk: ChunkConfig::default(),
    }
}

async fn ingest_doc(indexer: &Arc<Indexer>, store: &Arc<dyn LaneStore>) -> String {
    indexer
        .update_page("markdown/skills/memo.md", MEMO_SKILL)
        .await
        .unwrap();
    let blob = store
        .put_inbox_blob(TENANT, Bytes::from_static(CHUNK_BODY.as_bytes()), None)
        .await
        .unwrap();
    let worker = DocumentIngestWorker::new(Arc::clone(indexer), Arc::new(TitledProcessor));
    let out = worker
        .ingest(
            &blob,
            "text/plain",
            "memo",
            "doc-z",
            &cfg(),
            &serde_json::Value::Null,
        )
        .await
        .unwrap();
    let _ = BlobId::parse(blob.as_str());
    match out {
        IngestOutcome::Materialised { page_id, .. } => page_id,
        other => panic!("expected materialised, got {other:?}"),
    }
}

/// The structural prefix overwrites `blocks.body`, so a BM25 query for the
/// title token (absent from the raw chunk) now matches the chunk.
#[tokio::test]
async fn title_token_matches_chunk_via_structural_prefix() {
    let store_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let db = TempDir::new().unwrap();
    let i = indexer_on(Arc::clone(&store), &db);

    let page_id = ingest_doc(&i, &store).await;
    i.refresh_fts().await.unwrap();

    // BM25 lane: the title token is absent from the raw chunk but present in
    // the structural prefix → it must now retrieve the chunk.
    let hits = i
        .search(TITLE_TOKEN, 10, None, None, None, None)
        .await
        .unwrap();
    assert!(
        hits.iter().any(|h| h.page_id == page_id),
        "title token `{TITLE_TOKEN}` must retrieve the contextualised chunk; hits: {hits:?}"
    );

    // The stored block body begins with the `[title › p.page]` prefix.
    let page = i.expand(&page_id, None, None).await.unwrap().expect("page");
    let first = &page.blocks.first().expect("a chunk block").content;
    assert!(
        first.starts_with(&format!("[{DOC_TITLE} \u{203a} p.7]")),
        "block body must start with the structural prefix, got: {first:?}"
    );
    assert!(
        first.contains(CHUNK_BODY),
        "original chunk text must be preserved after the prefix"
    );
}
