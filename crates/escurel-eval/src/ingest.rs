//! Ingest a corpus once into a persistent DuckDB, then reopen it (read-only)
//! per retrieval config.
//!
//! The expensive step is embedding the corpus; we pay it exactly once via the
//! embed-free [`escurel_index::Indexer::write_document_blocks`] (it stores the
//! precomputed 768-d vectors verbatim), persist the DuckDB file, and let every
//! config-matrix run query the same index. Mirrors the index-setup recipe in
//! `escurel-loader::LoaderBuilder::build`.

use std::path::Path;
use std::sync::Arc;

use escurel_embed::Embedder;
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, LaneStore};

use crate::error::EvalError;

const TENANT: &str = "eval";

/// Number of docs embedded per `embed()` call (amortizes per-call overhead).
const EMBED_BATCH: usize = 64;

/// Summary of an ingest pass.
#[derive(Debug, Clone, Copy)]
pub struct IngestStats {
    pub docs: usize,
}

/// Open an `Indexer` over `db_path` (+ `store_dir` FsStore). When `fresh`, the
/// schema is created; otherwise only the per-connection extensions are loaded
/// (the schema already exists). The retrieval config / reranker are applied by
/// the caller via the `Indexer` builders.
pub fn open_indexer(
    db_path: &Path,
    store_dir: &Path,
    embedder: Arc<dyn Embedder>,
    fresh: bool,
) -> Result<Indexer, EvalError> {
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.to_path_buf()));
    let conn = duckdb::Connection::open(db_path)?;
    Migrator::load_extensions(&conn)?;
    Migrator::enable_hnsw_persistence(&conn)?;
    if fresh {
        Migrator::up(&conn)?;
    }
    Migrator::ensure_group_members(&conn)?;
    Migrator::ensure_external_credentials(&conn)?;
    Migrator::ensure_external_endpoints(&conn)?;
    Migrator::ensure_block_context(&conn)?;
    Ok(Indexer::new(store, embedder, conn, TENANT)?)
}

/// Embed + index every corpus doc into a fresh DuckDB at `db_path`, then
/// rebuild the HNSW + FTS indexes once. The corpus `_id` is the `page_id`.
/// `skill` is stamped on every instance (e.g. "paper").
pub async fn ingest_corpus(
    db_path: &Path,
    store_dir: &Path,
    embedder: Arc<dyn Embedder>,
    corpus: &[crate::dataset::CorpusDoc],
    skill: &str,
    contextualize: escurel_index::backend::document::ContextualizeMode,
) -> Result<IngestStats, EvalError> {
    use escurel_index::backend::document::{ContextualizeMode, structural_context_prefix};

    let indexer = open_indexer(db_path, store_dir, Arc::clone(&embedder), true)?;

    for batch in corpus.chunks(EMBED_BATCH) {
        let bodies: Vec<String> = batch.iter().map(crate::dataset::CorpusDoc::body).collect();
        // #216 measurability: embed the contextualized text (title-prefixed)
        // vs the plain body, so a run can measure the retrieval delta. A BEIR
        // doc is one whole "chunk" (no headings/pages), so the structural
        // context is `[<title>]`.
        let embed_inputs: Vec<String> = match contextualize {
            ContextualizeMode::Off => bodies.clone(),
            _ => batch
                .iter()
                .zip(bodies.iter())
                .map(
                    |(doc, body)| match structural_context_prefix(Some(&doc.title), &[], None) {
                        Some(ctx) => format!("{ctx}\n{body}"),
                        None => body.clone(),
                    },
                )
                .collect(),
        };
        let refs: Vec<&str> = embed_inputs.iter().map(String::as_str).collect();
        let vectors = embedder.embed(&refs).await?;

        for ((doc, body), vec) in batch.iter().zip(bodies.iter()).zip(vectors) {
            let overlay = overlay_markdown(skill, &doc.id, &doc.title);
            let chunk = match contextualize {
                ContextualizeMode::Off => escurel_index::IndexChunk::plain(body.clone()),
                _ => escurel_index::IndexChunk::contextualized(
                    structural_context_prefix(Some(&doc.title), &[], None),
                    body.clone(),
                ),
            };
            indexer
                .write_document_blocks(&doc.id, &overlay, &[chunk], &[vec])
                .await?;
        }
    }

    // FTS has no incremental refresh; HNSW is faster rebuilt once after a bulk
    // load. Both are O(rows) one-shots, like the loader's finish step.
    indexer.refresh_fts().await?;
    indexer.reindex_vectors().await?;

    Ok(IngestStats { docs: corpus.len() })
}

/// Minimal instance overlay: the frontmatter `write_document_blocks` reads for
/// `skill`/`id`/`slug`. The block bodies come from the `chunks` argument, not
/// this markdown body, so the heading is cosmetic.
fn overlay_markdown(skill: &str, id: &str, title: &str) -> String {
    let heading = if title.trim().is_empty() { id } else { title };
    format!("---\ntype: instance\nskill: {skill}\nid: {id}\n---\n# {heading}\n")
}
