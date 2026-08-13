//! `Indexer::write_document_blocks` stores PRECOMPUTED chunk vectors verbatim —
//! the embed-free write half that lets the offline batch loader / DuckDB
//! transfer carry embeddings without re-embedding. Real DuckDB + FsStore, no
//! mocks. The indexer here uses a `ZeroEmbedder`, so if the write path ever
//! re-embedded, the stored vectors would be zero — a loud failure.

use std::sync::Arc;

use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{IndexChunk, Indexer, Migrator};
use escurel_storage::{FsStore, LaneStore};
use tempfile::TempDir;

const TENANT: &str = "acme";
const OVERLAY: &str = "\
---
type: instance
skill: memo
id: doc-x
backend_ref: { kind: document }
---
# doc-x
";
const PAGE_ID: &str = "markdown/instances/memo/doc-x.md";

fn vec_with_head(head: f32) -> Vec<f32> {
    let mut v = vec![0.0_f32; 768];
    v[0] = head;
    v
}

#[tokio::test]
async fn write_document_blocks_stores_given_vectors_verbatim() {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let db_path = db_dir.path().join("escurel.duckdb");
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));

    // Distinct sentinel heads the ZeroEmbedder would never produce.
    let vectors = vec![vec_with_head(0.42), vec_with_head(0.99)];
    {
        let conn = duckdb::Connection::open(&db_path).unwrap();
        Migrator::up(&conn).unwrap();
        let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
        let indexer = Indexer::new(Arc::clone(&store), embedder, conn, TENANT).unwrap();
        indexer
            .write_document_blocks(
                PAGE_ID,
                OVERLAY,
                &[
                    IndexChunk::plain("first chunk"),
                    IndexChunk::plain("second chunk"),
                ],
                &vectors,
            )
            .await
            .expect("write_document_blocks");
        // drop indexer → release the connection before reopening to read.
    }

    // Reopen read-side and read the first element of each chunk's dense_vec.
    let conn = duckdb::Connection::open(&db_path).unwrap();
    Migrator::load_extensions(&conn).unwrap();
    Migrator::enable_hnsw_persistence(&conn).unwrap();
    let mut stmt = conn
        .prepare("SELECT dense_vec[1] FROM blocks WHERE page_id = ? ORDER BY ordinal")
        .unwrap();
    let heads: Vec<f32> = stmt
        .query_map([PAGE_ID], |r| r.get::<_, f32>(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();

    assert_eq!(heads.len(), 2, "two chunk blocks written");
    assert!(
        (heads[0] - 0.42).abs() < 1e-6,
        "chunk-0 vector stored verbatim, got {}",
        heads[0]
    );
    assert!(
        (heads[1] - 0.99).abs() < 1e-6,
        "chunk-1 vector stored verbatim, got {}",
        heads[1]
    );
}

#[tokio::test]
async fn write_document_blocks_rejects_vector_count_mismatch() {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let conn = duckdb::Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let indexer = Indexer::new(store, embedder, conn, TENANT).unwrap();

    // 2 chunks, 1 vector → must fail closed (never write a misaligned page).
    let err = indexer
        .write_document_blocks(
            PAGE_ID,
            OVERLAY,
            &[IndexChunk::plain("a"), IndexChunk::plain("b")],
            &[vec_with_head(0.1)],
        )
        .await
        .unwrap_err();
    assert!(
        format!("{err}").to_lowercase().contains("dim") || format!("{err}").contains("mismatch"),
        "expected a dim/mismatch error, got: {err}"
    );
}

/// The two materialisation paths differ in exactly one column, and that
/// difference is load-bearing.
///
/// A markdown page writes one whole-body block with `context IS NULL`; a
/// document overlay writes one block per chunk and preserves each chunk's
/// situating context (GH #216), which is concatenated only at embed/FTS/
/// rerank time. R6 folded the two column lists into one shared helper taking
/// `context: Option<&str>`, so this asymmetry became a value rather than a
/// difference in SQL — and nothing asserted it in either shape.
///
/// Both directions matter. If markdown started writing a non-NULL context,
/// retrieval would score pages against text their authors never wrote; if
/// overlays stopped preserving it, contextual retrieval would silently
/// degrade to the legacy behaviour with no error anywhere (codex review).
#[tokio::test]
async fn markdown_blocks_have_no_context_while_document_chunks_keep_theirs() {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let db_path = db_dir.path().join("escurel.duckdb");
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));

    const MD_PAGE: &str = "markdown/skills/memo.md";
    const MD: &str = "---\ntype: skill\nid: memo\ndescription: memo\n---\n# memo\n\nplain body.\n";

    {
        let conn = duckdb::Connection::open(&db_path).unwrap();
        Migrator::up(&conn).unwrap();
        let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
        let indexer = Indexer::new(Arc::clone(&store), embedder, conn, TENANT).unwrap();

        // Markdown path.
        indexer.update_page(MD_PAGE, MD).await.expect("update_page");

        // Document path, with a context on one chunk and none on the other,
        // so "preserved" is distinguishable from "always set".
        indexer
            .write_document_blocks(
                PAGE_ID,
                OVERLAY,
                &[
                    IndexChunk::contextualized(
                        Some("Section 2 of the Q3 report".to_owned()),
                        "first chunk",
                    ),
                    IndexChunk::plain("second chunk"),
                ],
                &[vec_with_head(0.42), vec_with_head(0.99)],
            )
            .await
            .expect("write_document_blocks");
    }

    let conn = duckdb::Connection::open(&db_path).unwrap();
    Migrator::load_extensions(&conn).unwrap();
    Migrator::enable_hnsw_persistence(&conn).unwrap();

    let contexts = |page: &str| -> Vec<Option<String>> {
        let mut stmt = conn
            .prepare("SELECT context FROM blocks WHERE page_id = ? ORDER BY ordinal")
            .unwrap();
        stmt.query_map([page], |r| r.get::<_, Option<String>>(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };

    assert_eq!(
        contexts(MD_PAGE),
        vec![None],
        "a markdown page writes one whole-body block with a NULL context"
    );
    assert_eq!(
        contexts(PAGE_ID),
        vec![Some("Section 2 of the Q3 report".to_owned()), None],
        "document chunks keep the context they were given, and only that"
    );
}
