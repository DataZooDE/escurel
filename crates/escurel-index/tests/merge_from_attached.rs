//! E2E for the DuckDB→DuckDB transfer (`Indexer::merge_from_attached`): the
//! import half of the offline batch loader. Real DuckDB files + real
//! `attach_external`, no mocks. The decisive assertion is that
//! `blocks.dense_vec` copies **byte-identical** — the target's embedder here is
//! a ZeroEmbedder, so if the merge ever re-embedded, the imported vectors would
//! be zero (a loud failure). Also covers skip/replace/error collision policies
//! and that imported chunks are FTS-searchable post-merge.

use std::sync::Arc;

use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::indexer::OnCollision;
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, LaneStore};
use tempfile::TempDir;

fn vhead(h: f32) -> Vec<f32> {
    let mut v = vec![0.0_f32; 768];
    v[0] = h;
    v
}

fn overlay(id: &str) -> String {
    format!(
        "---\ntype: instance\nskill: memo\nid: {id}\nbackend_ref: {{ kind: document }}\n---\n# {id}\n"
    )
}

fn page_id(id: &str) -> String {
    format!("markdown/instances/memo/{id}.md")
}

fn open_indexer(
    db_path: &std::path::Path,
    store_root: &std::path::Path,
    tenant: &str,
) -> Arc<Indexer> {
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_root.to_path_buf()));
    let conn = duckdb::Connection::open(db_path).unwrap();
    Migrator::up(&conn).unwrap();
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    Arc::new(Indexer::new(store, embedder, conn, tenant).unwrap())
}

/// Build a 2-doc source loader DuckDB (doc-a head 0.11, doc-b head 0.22) and
/// return its path. The source indexer is dropped so the file is closed before
/// the target attaches it read-only.
async fn build_source(dir: &TempDir) -> std::path::PathBuf {
    let db = dir.path().join("src.duckdb");
    let store_root = dir.path().join("src-store");
    {
        let idx = open_indexer(&db, &store_root, "src");
        idx.write_document_blocks(
            &page_id("doc-a"),
            &overlay("doc-a"),
            &["alpha zebra".to_owned()],
            &[vhead(0.11)],
        )
        .await
        .unwrap();
        idx.write_document_blocks(
            &page_id("doc-b"),
            &overlay("doc-b"),
            &["beta zebra".to_owned()],
            &[vhead(0.22)],
        )
        .await
        .unwrap();
        idx.reindex_vectors().await.unwrap();
    }
    db
}

/// First element of a page's (single) chunk vector, read from a closed DuckDB.
fn head_of(db: &std::path::Path, id: &str) -> Option<f32> {
    let conn = duckdb::Connection::open(db).unwrap();
    Migrator::load_extensions(&conn).unwrap();
    conn.query_row(
        "SELECT dense_vec[1] FROM blocks WHERE page_id = ? ORDER BY ordinal LIMIT 1",
        [page_id(id)],
        |r| r.get::<_, f32>(0),
    )
    .ok()
}

#[tokio::test]
async fn merge_skip_imports_new_pages_keeps_existing_and_copies_vectors_verbatim() {
    let sdir = TempDir::new().unwrap();
    let src_db = build_source(&sdir).await;

    let tdir = TempDir::new().unwrap();
    let tgt_db = tdir.path().join("tgt.duckdb");
    let tgt_store = tdir.path().join("tgt-store");
    let report = {
        let tgt = open_indexer(&tgt_db, &tgt_store, "acme");
        // Pre-existing doc-a in the target with a DIFFERENT vector (head 0.99).
        tgt.write_document_blocks(
            &page_id("doc-a"),
            &overlay("doc-a"),
            &["alpha original".to_owned()],
            &[vhead(0.99)],
        )
        .await
        .unwrap();
        tgt.attach_external("src", src_db.to_str().unwrap())
            .await
            .unwrap();
        let r = tgt
            .merge_from_attached("src", OnCollision::Skip)
            .await
            .unwrap();

        // Imported doc-b is FTS-searchable post-merge (proves refresh_fts ran).
        let hits = tgt
            .search("zebra", 10, None, None, None, None)
            .await
            .unwrap();
        assert!(
            hits.iter().any(|h| h.page_id == page_id("doc-b")),
            "doc-b found: {hits:?}"
        );
        r
    };

    assert_eq!(report.source_pages, 2);
    assert_eq!(report.collisions, 1, "doc-a collides");
    // Skip: doc-a keeps the target's 0.99; doc-b imported with the SOURCE's
    // 0.22 byte-for-byte (a re-embed would have produced 0.0).
    assert_eq!(head_of(&tgt_db, "doc-a"), Some(0.99), "existing untouched");
    assert_eq!(
        head_of(&tgt_db, "doc-b"),
        Some(0.22),
        "imported vector verbatim"
    );
}

#[tokio::test]
async fn merge_replace_overwrites_colliding_pages() {
    let sdir = TempDir::new().unwrap();
    let src_db = build_source(&sdir).await;

    let tdir = TempDir::new().unwrap();
    let tgt_db = tdir.path().join("tgt.duckdb");
    let tgt_store = tdir.path().join("tgt-store");
    {
        let tgt = open_indexer(&tgt_db, &tgt_store, "acme");
        tgt.write_document_blocks(
            &page_id("doc-a"),
            &overlay("doc-a"),
            &["alpha original".to_owned()],
            &[vhead(0.99)],
        )
        .await
        .unwrap();
        tgt.attach_external("src", src_db.to_str().unwrap())
            .await
            .unwrap();
        tgt.merge_from_attached("src", OnCollision::Replace)
            .await
            .unwrap();
    }
    // Replace: doc-a now carries the source's 0.11.
    assert_eq!(
        head_of(&tgt_db, "doc-a"),
        Some(0.11),
        "replaced with source"
    );
    assert_eq!(head_of(&tgt_db, "doc-b"), Some(0.22));
}

#[tokio::test]
async fn merge_error_aborts_on_collision() {
    let sdir = TempDir::new().unwrap();
    let src_db = build_source(&sdir).await;

    let tdir = TempDir::new().unwrap();
    let tgt_db = tdir.path().join("tgt.duckdb");
    let tgt_store = tdir.path().join("tgt-store");
    let tgt = open_indexer(&tgt_db, &tgt_store, "acme");
    tgt.write_document_blocks(
        &page_id("doc-a"),
        &overlay("doc-a"),
        &["x".to_owned()],
        &[vhead(0.99)],
    )
    .await
    .unwrap();
    tgt.attach_external("src", src_db.to_str().unwrap())
        .await
        .unwrap();
    let err = tgt
        .merge_from_attached("src", OnCollision::Error)
        .await
        .unwrap_err();
    assert!(
        format!("{err}").contains("already exist"),
        "collision aborts: {err}"
    );
}
