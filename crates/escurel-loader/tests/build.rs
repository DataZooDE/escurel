//! E2E for the offline loader build: real DuckDB + FsStore + real extraction
//! (PlainTextExtractor) + deterministic offline HashEmbedder (768-dim). No
//! mocks, no network. Asserts the loader materialises one page-with-chunks per
//! file, every chunk gets a non-NULL vector, and the manifest pins the
//! embedding space.

use std::sync::Arc;

use escurel_embed::{Embedder, HashEmbedder};
use escurel_index::backend::{
    ChunkConfig, ExtractConfig, Extractor, OcrPolicy, PlainTextExtractor,
};
use escurel_index::schema::Migrator;
use escurel_loader::{LoaderBuilder, read_manifest};
use tempfile::TempDir;

#[tokio::test]
async fn loader_build_materialises_documents_with_vectors_and_manifest() {
    // A small corpus: two text files, one long enough to chunk into several.
    let src = TempDir::new().unwrap();
    std::fs::write(src.path().join("a.txt"), "short note about widgets").unwrap();
    std::fs::write(
        src.path().join("b.txt"),
        "Clause one. Clause two. Clause three. Clause four. Clause five. Clause six.",
    )
    .unwrap();
    // A non-ingestable extension is skipped.
    std::fs::write(src.path().join("ignore.bin"), [0u8, 1, 2, 3]).unwrap();

    let out = TempDir::new().unwrap();
    let extractor: Arc<dyn Extractor> = Arc::new(PlainTextExtractor);
    let embedder: Arc<dyn Embedder> = Arc::new(HashEmbedder::default()); // offline, 768-dim

    let report = LoaderBuilder::new(out.path(), "attachment", extractor, embedder)
        .with_extract_config(ExtractConfig {
            ocr: OcrPolicy::Off,
            chunk: ChunkConfig {
                max_chars: 24,
                overlap: 4,
            },
        })
        .build(src.path())
        .await
        .expect("loader build");

    assert_eq!(report.manifest.doc_count, 2, "two text docs materialised");
    assert_eq!(report.skipped, 1, "the .bin file is skipped");
    assert_eq!(report.failed, 0);
    assert!(
        report.manifest.chunk_count >= 3,
        "the long doc chunks: {:?}",
        report.manifest
    );
    assert_eq!(report.manifest.model_id, "hash");
    assert_eq!(report.manifest.dim, 768);
    assert_eq!(report.manifest.schema_version, Migrator::SCHEMA_VERSION);

    // Manifest round-trips from disk.
    assert_eq!(read_manifest(out.path()).unwrap(), report.manifest);

    // Reopen the loader DuckDB: pages + blocks landed, every block has a vector.
    let conn = duckdb::Connection::open(out.path().join("escurel.duckdb")).unwrap();
    Migrator::load_extensions(&conn).unwrap();
    Migrator::enable_hnsw_persistence(&conn).unwrap();
    let pages: i64 = conn
        .query_row(
            "SELECT count(*) FROM pages WHERE page_type='instance'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(pages, 2);
    let blocks: i64 = conn
        .query_row("SELECT count(*) FROM blocks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(blocks as usize, report.manifest.chunk_count);
    let null_vecs: i64 = conn
        .query_row(
            "SELECT count(*) FROM blocks WHERE dense_vec IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(null_vecs, 0, "every chunk block has an embedding");

    // The canonical blobs were promoted into the loader store (FsStore layout
    // is {root}/tenants/{tenant}/blobs/<hash>); not left in inbox.
    let blobs = out.path().join("tenants/loader/blobs");
    assert!(blobs.exists(), "canonical blobs dir present");
    let canonical = std::fs::read_dir(&blobs)
        .unwrap()
        .filter(|e| e.as_ref().unwrap().file_type().unwrap().is_file())
        .count();
    assert_eq!(canonical, 2, "two promoted canonical blobs");
}
