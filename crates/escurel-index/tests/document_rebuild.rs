//! Integration tests for document rebuild-from-blob + audit (PR-3e).
//! Real DuckDB + FsStore, born-digital text. No mocks.

use std::sync::Arc;

use bytes::Bytes;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::backend::{
    ChunkConfig, DeterministicProcessor, DocumentIngestWorker, ExtractConfig, IngestOutcome,
    OcrPolicy, PlainTextExtractor, SqlConnector, SqlViewBackend, SqlViewBinding,
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

#[tokio::test]
async fn rebuild_reconstructs_mixed_corpus() {
    // Derivability across ALL backend kinds at once: a from-scratch DuckDB over
    // one corpus + blobs must reconstruct markdown, sql_view, AND document
    // instances together (each path is tested alone elsewhere; this catches
    // cross-kind interaction in rebuild).
    let store_dir = TempDir::new().unwrap();
    let data_dir = TempDir::new().unwrap();
    std::fs::write(data_dir.path().join("a.json"), br#"{"name":"Acme"}"#).unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));

    // indexer1: seed one instance of each kind.
    let db1 = TempDir::new().unwrap();
    let (sql_view, doc_page) = {
        let i1 = indexer_on(Arc::clone(&store), &db1);
        // markdown
        i1.update_page(
            "markdown/skills/note.md",
            "---\ntype: skill\nid: note\ndescription: x\n---\n# note\n",
        )
        .await
        .unwrap();
        i1.update_page(
            "markdown/instances/note/n1.md",
            "---\ntype: instance\nskill: note\nid: n1\n---\n# Note one\n",
        )
        .await
        .unwrap();
        // sql_view
        let sv = SqlViewBinding {
            connector: SqlConnector::JsonDir,
            attach: None,
            relation: data_dir.path().to_str().unwrap().to_owned(),
            filter: None,
            project: Default::default(),
            search_text: vec!["name".to_owned()],
        };
        let m = SqlViewBackend::new(Arc::clone(&i1))
            .create_instance("customers", &sv, "eu", "# EU")
            .await
            .unwrap();
        // document
        i1.update_page("markdown/skills/memo.md", MEMO_SKILL)
            .await
            .unwrap();
        let blob = store
            .put_inbox_blob(
                TENANT,
                Bytes::from_static(b"the mixed corpus memo body"),
                None,
            )
            .await
            .unwrap();
        let out = worker(&i1)
            .ingest(&blob, "text/plain", "memo", "doc-m", &cfg())
            .await
            .unwrap();
        let page_id = match out {
            IngestOutcome::Materialised { page_id, .. } => page_id,
            other => panic!("expected materialised, got {other:?}"),
        };
        (m.view, page_id)
    };

    // indexer2: FROM SCRATCH over the same corpus + blobs.
    let db2 = TempDir::new().unwrap();
    let i2 = indexer_on(Arc::clone(&store), &db2);
    i2.rebuild().await.expect("rebuild mixed corpus");

    // markdown instance reconstructed
    assert!(
        i2.resolve("[[note::n1]]", None)
            .await
            .unwrap()
            .page
            .is_some(),
        "markdown instance must rebuild"
    );
    // sql_view reconstructed (view queryable)
    let rows = SqlViewBackend::new(Arc::clone(&i2))
        .project_rows(&sql_view, 10)
        .await
        .expect("sql view must rebuild");
    assert_eq!(rows.len(), 1);
    // document reconstructed (chunk-blocks from the retained blob)
    let page = i2
        .expand(&doc_page, None, None)
        .await
        .unwrap()
        .expect("doc page");
    assert!(!page.blocks.is_empty(), "document chunks must rebuild");
}

#[tokio::test]
async fn document_skill_for_mime_is_deterministic_on_ambiguous_accepts() {
    // REQ-DOC-06: an inbox arrival's content type must resolve to exactly one
    // handling skill, deterministically, even when two skills accept it.
    let store_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let db = TempDir::new().unwrap();
    let i = indexer_on(Arc::clone(&store), &db);
    let pdf_skill = |id: &str| {
        format!(
            "---\ntype: skill\nid: {id}\ndescription: x\nbackend:\n  kind: document\n  accepts: [application/pdf]\n---\n# {id}\n"
        )
    };
    i.update_page("markdown/skills/zreport.md", &pdf_skill("zreport"))
        .await
        .unwrap();
    i.update_page("markdown/skills/areport.md", &pdf_skill("areport"))
        .await
        .unwrap();

    // Deterministic: lowest id wins, stable across calls.
    let first = i.document_skill_for_mime("application/pdf").await.unwrap();
    assert_eq!(
        first.as_deref(),
        Some("areport"),
        "deterministic by id order"
    );
    assert_eq!(
        i.document_skill_for_mime("application/pdf").await.unwrap(),
        first
    );
    // Unmatched MIME → no handler (parked upstream).
    assert!(
        i.document_skill_for_mime("application/x-unknown")
            .await
            .unwrap()
            .is_none()
    );
}
