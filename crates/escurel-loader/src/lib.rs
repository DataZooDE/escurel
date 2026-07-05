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
use escurel_embed::ZeroEmbedder;
use escurel_index::backend::{
    DeterministicProcessor, DocumentIngestWorker, ExtractConfig, Extractor, IngestOutcome,
};
use escurel_index::indexer::{MergeReport, OnCollision};
use escurel_index::schema::Migrator;
use escurel_index::{Indexer, IndexerError};
use escurel_storage::{FsStore, Key, LaneStore};
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
    #[error("invalid key: {0}")]
    Key(#[from] escurel_storage::KeyError),
    #[error("serialize manifest: {0}")]
    Json(#[from] serde_json::Error),
    #[error("incompatible loader artifact: {0}")]
    Incompatible(String),
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
    /// Per-document extra frontmatter, keyed by source file name (e.g.
    /// `16_100_D.pdf` → `{nummer, titel, wp, doctype, stand, sachgebiet}`).
    /// Written verbatim into each instance's frontmatter, so a `drucksache`
    /// corpus carries facetable metadata + real titles.
    metadata: serde_json::Map<String, serde_json::Value>,
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
            metadata: serde_json::Map::new(),
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

    /// Per-document extra frontmatter keyed by source file name (the sidecar).
    /// Each entry must be a JSON object; it is merged verbatim into the
    /// materialised instance's frontmatter.
    #[must_use]
    pub fn with_metadata(mut self, metadata: serde_json::Map<String, serde_json::Value>) -> Self {
        self.metadata = metadata;
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
        Migrator::ensure_external_endpoints(&conn)?;
        Migrator::ensure_block_context(&conn)?;
        // Contextual Retrieval (GH #216): ensure `blocks.context` on EVERY
        // boot (idempotent), so a tenant DB provisioned before the column
        // existed gains it before `refresh_fts` indexes it.
        Migrator::ensure_block_context(&conn)?;
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

            // Per-doc extra frontmatter from the sidecar, keyed by file name.
            let extra = path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| self.metadata.get(n))
                .cloned()
                .unwrap_or(serde_json::Value::Null);

            match worker
                .ingest(&blob, &mime, &self.skill, &instance_id, &self.cfg, &extra)
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

/// Tally of a [`copy_files`] run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FileCopyReport {
    /// Canonical blobs copied into the live tenant.
    pub blobs: usize,
    /// Overlay markdown keys copied into the live tenant.
    pub overlays: usize,
}

/// Copy the loader's canonical blobs + overlay markdown into a live tenant's
/// LaneStore — the **files-first** half of a transfer, run BEFORE
/// [`Indexer::merge_from_attached`] copies the DuckDB rows. Both writes are
/// content-addressed / key-deterministic, so a re-run (or a crash + retry) is
/// idempotent: at worst it leaves orphan blobs the existing
/// `audit_documents`/`reclaim_orphan_blobs` reclaim — never rows pointing at a
/// missing blob.
///
/// `loader_dir` is the directory a [`LoaderBuilder::build`] wrote (its FsStore
/// root); `live` is the destination store; `live_tenant` the target tenant.
pub async fn copy_files(
    loader_dir: &Path,
    live: &dyn LaneStore,
    live_tenant: &str,
) -> Result<FileCopyReport, LoaderError> {
    let loader: Arc<dyn LaneStore> = Arc::new(FsStore::new(loader_dir.to_path_buf()));
    let mut report = FileCopyReport::default();

    // Blobs first: every chunk block references a canonical blob by id, so the
    // blob must exist before the rows land.
    for id in loader.list_blobs(LOADER_TENANT).await? {
        let bytes = loader.get_blob(LOADER_TENANT, &id).await?;
        // put_blob is content-addressed → re-deriving the same id is idempotent.
        live.put_blob(live_tenant, bytes, None).await?;
        report.blobs += 1;
    }

    // Overlay markdown: re-key each loader path under the live tenant verbatim
    // (the path — markdown/instances/<skill>/<id>.md — is tenant-independent).
    for key in loader.list(&Key::new(LOADER_TENANT, "markdown")?).await? {
        let body = loader.read(&key).await?;
        live.write(&Key::new(live_tenant, key.path())?, body)
            .await?;
        report.overlays += 1;
    }

    Ok(report)
}

/// Alias the loader DuckDB is `ATTACH`ed under during a transfer.
const TRANSFER_ALIAS: &str = "loader_src";

/// Outcome of a [`transfer`].
#[derive(Debug, Clone)]
pub struct TransferReport {
    /// The validated source manifest.
    pub manifest: Manifest,
    /// Blobs + overlays copied into the live tenant.
    pub files: FileCopyReport,
    /// Rows merged DuckDB→DuckDB.
    pub merge: MergeReport,
}

/// Transfer a loader artifact at `from` into the live escurel data dir at `to`,
/// under `live_tenant`, carrying embeddings as data (NO re-embed).
///
/// `expect_model` is the live tenant's embedder identity (`Embedder::model_id`);
/// the manifest's `model_id`/`dim`/`schema_version` are validated against it +
/// this binary's [`Migrator::SCHEMA_VERSION`] **before** anything is touched.
/// A mismatch aborts with [`LoaderError::Incompatible`] — mixing embedding
/// spaces silently destroys retrieval, so we fail closed.
///
/// Order (crash-safe): validate → copy blobs + overlays (files first) → attach
/// the loader DuckDB read-only → `merge_from_attached` (rows last, in one
/// transaction). A crash before the merge leaves orphan blobs the audit
/// reclaims, never rows pointing at missing content.
pub async fn transfer(
    from: &Path,
    to: &Path,
    live_tenant: &str,
    expect_model: &str,
    on_collision: OnCollision,
) -> Result<TransferReport, LoaderError> {
    let manifest = read_manifest(from)?;
    if manifest.model_id != expect_model {
        return Err(LoaderError::Incompatible(format!(
            "embedder model mismatch: artifact was built with '{}', live tenant expects '{}'",
            manifest.model_id, expect_model
        )));
    }
    if manifest.dim != escurel_index::indexer::BLOCKS_DENSE_VEC_DIM {
        return Err(LoaderError::Incompatible(format!(
            "embedding dim {} != live schema dim {}",
            manifest.dim,
            escurel_index::indexer::BLOCKS_DENSE_VEC_DIM
        )));
    }
    if manifest.schema_version != Migrator::SCHEMA_VERSION {
        return Err(LoaderError::Incompatible(format!(
            "artifact schema version {} != this binary's {}",
            manifest.schema_version,
            Migrator::SCHEMA_VERSION
        )));
    }

    // Open the live tenant index. The merge copies vectors verbatim and never
    // embeds, so a ZeroEmbedder placeholder (768-dim) is all Indexer::new needs.
    // `to` is the escurel data dir; each tenant's DuckDB + LaneStore live under
    // `<data_dir>/tenants/<tenant>/` (escurel-server config.rs), so the DB path
    // must match — the FsStore below keys blobs/overlays under the same prefix.
    let live_store: Arc<dyn LaneStore> = Arc::new(FsStore::new(to.to_path_buf()));
    let tenant_dir = to.join("tenants").join(live_tenant);
    std::fs::create_dir_all(&tenant_dir)?;
    let db_path = tenant_dir.join("escurel.duckdb");
    // Mirror the server boot recipe (config.rs): the schema DDL (`up`) is
    // one-time, so run it ONLY for a fresh DB; load_extensions + ensure_* are
    // idempotent and run every time (a transfer into an existing tenant must
    // not re-CREATE the tables).
    let fresh = !db_path.exists();
    let conn = duckdb::Connection::open(&db_path)?;
    Migrator::load_extensions(&conn)?;
    if fresh {
        Migrator::up(&conn)?;
    }
    Migrator::ensure_group_members(&conn)?;
    Migrator::ensure_external_credentials(&conn)?;
    Migrator::ensure_external_endpoints(&conn)?;
    let live = Indexer::new(
        Arc::clone(&live_store),
        Arc::new(ZeroEmbedder::default()),
        conn,
        live_tenant,
    )?;

    // Attach the source read-only up front so an `error`-collision policy can
    // preflight BEFORE we mutate the live store: `error` means "abort if
    // anything collides", so it must not have copied files by the time it
    // aborts.
    live.attach_external(
        TRANSFER_ALIAS,
        from.join("escurel.duckdb").to_string_lossy().as_ref(),
    )
    .await?;
    if matches!(on_collision, OnCollision::Error) {
        let collisions = live.attached_page_collisions(TRANSFER_ALIAS).await?;
        if collisions > 0 {
            return Err(LoaderError::Incompatible(format!(
                "{collisions} page_id(s) already exist in tenant '{live_tenant}' \
                 (on_collision=error); nothing was copied"
            )));
        }
    }

    // Files first (idempotent), then the DuckDB rows.
    let files = copy_files(from, live_store.as_ref(), live_tenant).await?;
    let merge = live
        .merge_from_attached(TRANSFER_ALIAS, on_collision)
        .await?;

    Ok(TransferReport {
        manifest,
        files,
        merge,
    })
}

/// Read a loader [`Manifest`] from a loader directory.
pub fn read_manifest(loader_dir: &Path) -> Result<Manifest, LoaderError> {
    let raw = std::fs::read(loader_dir.join(MANIFEST_FILE))?;
    Ok(serde_json::from_slice(&raw)?)
}
