//! Offline batch document loader for Escurel.
//!
//! Ingesting a huge corpus (e.g. ~20k PDFs) through the live server's
//! `POST /ingest/upload` is hopeless: each chunk is one embed, so the
//! per-tenant Embeds quota makes it a multi-week trickle. This crate runs the
//! **same** extract → chunk → embed → materialise pipeline ([`DocumentIngestWorker`]
//! → [`Indexer::materialize_document`]) **in-process**, against a *throwaway
//! loader instance* (its own DuckDB + blob dir), at full speed with no HTTP
//! and no quota. An operator then transfers the result into a live tenant
//! carrying the embeddings as data (see `escurel admin transfer` / PR 5+),
//! so production never re-embeds.
//!
//! The loader writes a [`Manifest`] (`manifest.json`) recording the embedder's
//! `model_id` + `dim` + the schema version; the transfer refuses an artifact
//! whose embedding space or schema doesn't match the live tenant.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use escurel_embed::Embedder;
use escurel_index::backend::{
    DeterministicProcessor, DocumentIngestWorker, ExtractConfig, Extractor, IngestOutcome,
};
use escurel_index::schema::Migrator;
use escurel_index::{Indexer, IndexerError};
use escurel_storage::{FsStore, LaneStore};
use serde::{Deserialize, Serialize};

/// The loader tenant name. The transfer re-keys blobs/overlays into the live
/// tenant, so this is internal and never leaks.
const LOADER_TENANT: &str = "loader";
/// Manifest filename inside the loader directory.
pub const MANIFEST_FILE: &str = "manifest.json";

#[derive(Debug, thiserror::Error)]
pub enum LoaderError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("duckdb: {0}")]
    Duckdb(#[from] duckdb::Error),
    #[error("migration: {0}")]
    Migration(#[from] escurel_index::schema::MigrationError),
    #[error("indexer: {0}")]
    Indexer(#[from] IndexerError),
    #[error("storage: {0}")]
    Storage(#[from] escurel_storage::StoreError),
    #[error("serialize manifest: {0}")]
    Json(#[from] serde_json::Error),
}

/// Artifact manifest — the compatibility contract the transfer validates.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    /// Embedder identity (`Embedder::model_id`); must equal the live tenant's.
    pub model_id: String,
    /// Embedding dimension; must equal the live tenant's.
    pub dim: usize,
    /// Per-tenant DuckDB schema version ([`Migrator::SCHEMA_VERSION`]).
    pub schema_version: u32,
    /// The document skill these instances were materialised under.
    pub skill: String,
    /// Documents materialised (excludes extraction failures).
    pub doc_count: usize,
    /// Total chunk `blocks` written.
    pub chunk_count: usize,
}

/// Outcome of a loader build.
#[derive(Debug, Clone)]
pub struct LoaderReport {
    pub manifest: Manifest,
    /// Files that failed extraction (blob retained in the loader inbox).
    pub failed: usize,
    /// Files skipped because the extractor doesn't accept their MIME.
    pub skipped: usize,
    pub loader_dir: PathBuf,
}

/// Builds a loader instance from a source directory of documents.
pub struct LoaderBuilder {
    loader_dir: PathBuf,
    skill: String,
    extractor: Arc<dyn Extractor>,
    embedder: Arc<dyn Embedder>,
    cfg: ExtractConfig,
}

impl LoaderBuilder {
    /// `loader_dir` becomes a full escurel data dir (`escurel.duckdb`,
    /// `markdown/`, `blobs/`, `manifest.json`). `skill` is the document skill
    /// the instances are materialised under. `extractor` handles the corpus's
    /// MIME(s) (e.g. `KreuzbergExtractor` for PDFs, `PlainTextExtractor` for
    /// text). `embedder` MUST match the live tenant's model + dim (768).
    pub fn new(
        loader_dir: impl Into<PathBuf>,
        skill: impl Into<String>,
        extractor: Arc<dyn Extractor>,
        embedder: Arc<dyn Embedder>,
    ) -> Self {
        Self {
            loader_dir: loader_dir.into(),
            skill: skill.into(),
            extractor,
            embedder,
            cfg: ExtractConfig::default(),
        }
    }

    /// Override the chunking config (defaults to [`ExtractConfig::default`]).
    /// Use the **same** chunk knobs as the live tenant's skill so a later
    /// `rebuild` re-derives identical chunks.
    #[must_use]
    pub fn with_extract_config(mut self, cfg: ExtractConfig) -> Self {
        self.cfg = cfg;
        self
    }

    /// Ingest every accepted file under `src` (recursive) into the loader
    /// instance, then rebuild the vector index + FTS once. Idempotent: the
    /// per-doc `instance_id` is the content sha256, so re-running over the same
    /// corpus re-materialises the same `page_id`s.
    pub async fn build(&self, src: &Path) -> Result<LoaderReport, LoaderError> {
        std::fs::create_dir_all(&self.loader_dir)?;
        let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(self.loader_dir.clone()));
        let db_path = self.loader_dir.join("escurel.duckdb");
        let conn = duckdb::Connection::open(&db_path)?;
        // Standalone-Indexer recipe (mirrors escurel-server config.rs): load
        // per-connection extensions, migrate a fresh DB, ensure the
        // boot-idempotent tables.
        Migrator::load_extensions(&conn)?;
        Migrator::up(&conn)?;
        Migrator::ensure_group_members(&conn)?;
        Migrator::ensure_external_credentials(&conn)?;
        let indexer = Arc::new(Indexer::new(
            Arc::clone(&store),
            Arc::clone(&self.embedder),
            conn,
            LOADER_TENANT,
        )?);

        let worker = DocumentIngestWorker::new(
            Arc::clone(&indexer),
            Arc::new(DeterministicProcessor::new(Arc::clone(&self.extractor))),
        );

        let mut doc_count = 0usize;
        let mut chunk_count = 0usize;
        let mut failed = 0usize;
        let mut skipped = 0usize;

        for entry in walkdir::WalkDir::new(src).sort_by_file_name() {
            let entry = entry.map_err(std::io::Error::other)?;
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            let Some(mime) = mime_for(path) else {
                skipped += 1;
                continue;
            };
            if !self.extractor.accepts(&mime) {
                skipped += 1;
                continue;
            }

            let bytes = std::fs::read(path)?;
            // Deposit into the loader inbox; the worker promotes it to canonical
            // on a successful materialise. `instance_id` = content sha256 so the
            // same bytes always yield the same page_id (idempotent + dedup).
            let blob = store
                .put_inbox_blob(LOADER_TENANT, bytes.into(), None)
                .await?;
            let instance_id = format!("doc-{}", &blob.hex()[..12.min(blob.hex().len())]);

            match worker
                .ingest(&blob, &mime, &self.skill, &instance_id, &self.cfg)
                .await?
            {
                IngestOutcome::Materialised { chunk_count: n, .. } => {
                    doc_count += 1;
                    chunk_count += n;
                }
                IngestOutcome::ExtractionFailed { .. } => failed += 1,
            }
        }

        // Bulk-load done: rebuild the HNSW vector index + the BM25 FTS snapshot
        // once (per-row HNSW maintenance + the one-shot FTS pragma — see the
        // index methods). Cheap relative to embedding; paid once.
        indexer.reindex_vectors().await?;
        indexer.refresh_fts().await?;

        let manifest = Manifest {
            model_id: self.embedder.model_id(),
            dim: self.embedder.dim(),
            schema_version: Migrator::SCHEMA_VERSION,
            skill: self.skill.clone(),
            doc_count,
            chunk_count,
        };
        std::fs::write(
            self.loader_dir.join(MANIFEST_FILE),
            serde_json::to_vec_pretty(&manifest)?,
        )?;

        Ok(LoaderReport {
            manifest,
            failed,
            skipped,
            loader_dir: self.loader_dir.clone(),
        })
    }
}

/// Best-effort MIME inference by extension — the loader's intake key (the
/// extractor still decides whether it `accepts` the MIME). `None` for
/// unrecognised extensions (skipped).
#[must_use]
pub fn mime_for(path: &Path) -> Option<String> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    let mime = match ext.as_str() {
        "pdf" => "application/pdf",
        "txt" => "text/plain",
        "md" | "markdown" => "text/markdown",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        _ => return None,
    };
    Some(mime.to_owned())
}

/// Read a loader [`Manifest`] from a loader directory.
pub fn read_manifest(loader_dir: &Path) -> Result<Manifest, LoaderError> {
    let raw = std::fs::read(loader_dir.join(MANIFEST_FILE))?;
    Ok(serde_json::from_slice(&raw)?)
}
