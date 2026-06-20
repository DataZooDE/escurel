//! Integration tests for document rebuild-from-blob + audit (PR-3e).
//! Real DuckDB + FsStore, born-digital text. No mocks.

use std::sync::Arc;

use bytes::Bytes;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::backend::{
    ChunkConfig, DeterministicProcessor, DocumentIngestWorker, ExtractConfig, OcrPolicy,
    PlainTextExtractor,
};
use escurel_index::{Indexer, Migrator};
use escurel_storage::{BlobId, FsStore, LaneStore};
use tempfile::TempDir;

const TENANT: &str = "acme";
const MEMO_SKILL: &str = "\
---
type: skill
id: memo
description: text memos
backend:
  kind: document
  accepts: [text/plain]
  chunk: { max_chars: 30, overlap: 5 }
---
# memo
";

fn indexer_on(store: Arc<dyn LaneStore>, db: &TempDir) -> Arc<Indexer> {
    let conn = duckdb::Connection::open(db.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    Arc::new(Indexer::new(store, embedder, conn, TENANT).unwrap())
}

fn worker(indexer: &Arc<Indexer>) -> DocumentIngestWorker {
    DocumentIngestWorker::new(
        Arc::clone(indexer),
        Arc::new(DeterministicProcessor::new(Arc::new(PlainTextExtractor))),
    )
}

fn cfg() -> ExtractConfig {
    ExtractConfig {
        ocr: OcrPolicy::Off,
        chunk: ChunkConfig {
            max_chars: 30,
            overlap: 5,
        },
    }
}

#[tokio::test]
async fn rebuild_reextracts_document_chunks_from_retained_blob() {
    let store_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let body = "The quarterly report spans three regions and many product lines this year.";

    // indexer1 ingests the document (chunks + overlay + promoted blob land in
    // the shared store).
    let db1 = TempDir::new().unwrap();
    let (page_id, n_chunks) = {
        let i1 = indexer_on(Arc::clone(&store), &db1);
        i1.update_page("markdown/skills/memo.md", MEMO_SKILL)
            .await
            .unwrap();
        let blob = store
            .put_inbox_blob(TENANT, Bytes::from_static(body.as_bytes()), None)
            .await
            .unwrap();
        let out = worker(&i1)
            .ingest(&blob, "text/plain", "memo", "doc-q3", &cfg())
            .await
            .unwrap();
        match out {
            escurel_index::backend::IngestOutcome::Materialised {
                page_id,
                chunk_count,
            } => {
                assert!(chunk_count > 1, "expected multiple chunks");
                (page_id, chunk_count)
            }
            other => panic!("expected materialised, got {other:?}"),
        }
    };

    // indexer2: FROM-SCRATCH DuckDB over the SAME corpus + blobs. The chunks
    // do not exist until rebuild re-extracts them from the retained blob.
    let db2 = TempDir::new().unwrap();
    let i2 = indexer_on(Arc::clone(&store), &db2);
    i2.rebuild().await.expect("rebuild");

    let page = i2
        .expand(&page_id, None, None)
        .await
        .unwrap()
        .expect("page");
    assert_eq!(
        page.blocks.len(),
        n_chunks,
        "rebuild must reconstruct all chunk-blocks from the blob"
    );
    assert_eq!(page.frontmatter["backend_ref"]["kind"], "document");
}

#[tokio::test]
async fn audit_documents_detects_missing_blob() {
    let store_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let db = TempDir::new().unwrap();
    let i = indexer_on(Arc::clone(&store), &db);
    i.update_page("markdown/skills/memo.md", MEMO_SKILL)
        .await
        .unwrap();
    let blob = store
        .put_inbox_blob(TENANT, Bytes::from_static(b"a body to index"), None)
        .await
        .unwrap();
    worker(&i)
        .ingest(&blob, "text/plain", "memo", "doc-x", &cfg())
        .await
        .unwrap();

    // Healthy: blob retained.
    assert!(i.audit_documents().await.unwrap().is_empty());

    // Delete the canonical blob → audit reports the orphan overlay.
    let key = escurel_storage::Key::new(TENANT, format!("blobs/{}", blob.hex())).unwrap();
    store.delete(&key).await.unwrap();
    let problems = i.audit_documents().await.unwrap();
    assert_eq!(problems.len(), 1, "got {problems:?}");
    assert!(problems[0].0.contains("memo/doc-x"));

    let _ = BlobId::parse(blob.as_str());
}
