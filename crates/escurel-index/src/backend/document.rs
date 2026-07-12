//! Document/RAG backend — the `Extractor` seam (PR-3b).
//!
//! Document ingestion turns one uploaded file into one page-with-N-blocks:
//! extract text + metadata, chunk it, embed the chunks, index them. This
//! module owns the **extraction** contract; chunking + the
//! `DocumentBackend` materialise/read paths land with PR-3d/3e.
//!
//! ## The `Extractor` trait (REQ-DOC-02, HLD §8)
//!
//! [`ExtractionResult`] is the contract: `{ content, metadata, chunks }`,
//! shaped as a superset of what the kreuzberg crate returns (spike S5) so a
//! `KreuzbergExtractor` slots in behind the trait without changing the
//! contract. Two impls ship here:
//!
//! - [`PlainTextExtractor`] — a real born-digital extractor for `text/*`
//!   (plain, markdown). No native deps, always available offline.
//! - [`NullExtractor`] — a no-op for tests that exercise the *pipeline*
//!   without caring about extraction output.
//!
//! ## kreuzberg (PDF/DOCX) — behind the `kreuzberg` feature
//!
//! [`KreuzbergExtractor`] (PDF/DOCX/PPTX via the in-process kreuzberg crate,
//! `bundled-pdfium`) is wired behind the **`kreuzberg`** cargo feature
//! (off by default — the heavy ELv2-licensed native dep is opt-in; the
//! default build stays light + offline). Enabling it required bumping the
//! workspace MSRV to 1.91 (see
//! `docs/notes/discovered/2026-06-21-kreuzberg-msrv-191.md`). The trait keeps
//! the extractor swappable (REQ-NF-08, ELv2).

use std::sync::Arc;

use async_trait::async_trait;
use escurel_storage::BlobId;

use crate::{IndexChunk, Indexer, IndexerError};

/// Extracted metadata about a document (a subset of kreuzberg's metadata).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DocMetadata {
    pub title: Option<String>,
    pub authors: Vec<String>,
    pub page_count: Option<u32>,
    /// RFC 3339 creation timestamp, when the format carries one.
    pub created: Option<String>,
}

/// One chunk of an extracted document, with provenance back into the
/// original (REQ-DOC-02). `byte_start..byte_end` index into `content`;
/// `page` is the source page when known; `ordinal` is the 0-based order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub ordinal: u32,
    pub byte_start: usize,
    pub byte_end: usize,
    pub page: Option<u32>,
    pub text: String,
}

/// The result of extracting one document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractionResult {
    pub content: String,
    pub metadata: DocMetadata,
    pub chunks: Vec<Chunk>,
}

/// Contextual-retrieval mode (GH #216, Variant A). When `Structural`, each
/// chunk carries a small `[<title> › <heading path> › p.<page>]` situating
/// context derived from the document title, the markdown heading hierarchy
/// above the chunk and the chunk's page. The context is stored SEPARATELY
/// (`blocks.context`) — `blocks.body` stays the verbatim chunk for display +
/// provenance — and feeds retrieval only: the dense embedding input, the
/// BM25 FTS index (which indexes both columns) and the rerank passage.
/// `Off` is byte-for-byte the legacy behaviour. `Llm` (Variant B, #216) has an
/// LLM write the situating context — behind the `contextualize-llm` feature and
/// off by default; see [`ContextualizeMode::Llm`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ContextualizeMode {
    /// No contextualisation; no situating context is stored.
    Off,
    /// Situate each chunk with `[<title> › <heading path> › p.<page>]`.
    #[default]
    Structural,
    /// Variant B (#216): an LLM writes a one-sentence situating context per
    /// chunk. This is a **network path**, so it is off by default (behind the
    /// `contextualize-llm` feature) and applied only by the async ingest layer;
    /// the pure, air-gap-safe path here degrades `Llm` to `Structural` so
    /// rebuilds and LLM-less builds stay deterministic.
    Llm,
}

impl ContextualizeMode {
    /// Parse the `ESCUREL_INGEST_CONTEXTUALIZE` value (`off` | `structural` |
    /// `llm`); unknown / empty → the default ([`ContextualizeMode::Structural`]).
    #[must_use]
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" => Self::Off,
            "llm" => Self::Llm,
            _ => Self::Structural,
        }
    }
}

/// Build the structural situating context for one chunk, or `None` when
/// there is nothing to situate with (no title, no headings AND no page).
/// Format: `[<title> › <heading> › … › p.<page>]` — segments joined by
/// ` › ` (U+203A), each omitted when absent. A heading equal to the
/// preceding segment (e.g. a `# <title>` H1 repeating the document title)
/// is dropped rather than duplicated.
#[must_use]
pub fn structural_context_prefix(
    title: Option<&str>,
    headings: &[String],
    page: Option<u32>,
) -> Option<String> {
    let mut segments: Vec<&str> = Vec::with_capacity(headings.len() + 2);
    if let Some(t) = title.map(str::trim).filter(|t| !t.is_empty()) {
        segments.push(t);
    }
    for h in headings {
        let h = h.trim();
        if h.is_empty() || segments.last().is_some_and(|s| s.eq_ignore_ascii_case(h)) {
            continue;
        }
        segments.push(h);
    }
    let page_seg = page.map(|p| format!("p.{p}"));
    let mut parts: Vec<&str> = segments;
    if let Some(p) = page_seg.as_deref() {
        parts.push(p);
    }
    if parts.is_empty() {
        None
    } else {
        Some(format!("[{}]", parts.join(" \u{203a} ")))
    }
}

/// The markdown ATX heading path open at `byte_offset` into `content`: the
/// innermost hierarchy of `#`…`######` headings on lines that END before the
/// offset. A deeper-or-equal heading replaces the stack from its level down,
/// mirroring how a document outline nests. Deterministic, no inference —
/// documents without markdown headings simply yield an empty path.
#[must_use]
pub fn heading_path_at(content: &str, byte_offset: usize) -> Vec<String> {
    let mut stack: Vec<(usize, String)> = Vec::new();
    let mut pos = 0usize;
    for line in content.split_inclusive('\n') {
        let line_start = pos;
        pos += line.len();
        if line_start >= byte_offset {
            break;
        }
        let trimmed = line.trim_start();
        let level = trimmed.bytes().take_while(|b| *b == b'#').count();
        if level == 0 || level > 6 {
            continue;
        }
        let Some(text) = trimmed[level..].strip_prefix([' ', '\t']) else {
            continue; // `#hashtag`, not a heading
        };
        let text = text.trim().trim_end_matches('#').trim();
        if text.is_empty() {
            continue;
        }
        stack.retain(|(l, _)| *l < level);
        stack.push((level, text.to_owned()));
    }
    stack.into_iter().map(|(_, t)| t).collect()
}

/// Turn extracted [`Chunk`]s into the [`IndexChunk`]s the write path stores:
/// the verbatim chunk text as `body`, plus (in `Structural` mode) the
/// situating context built from the document `title`, the markdown heading
/// path open at the chunk's `byte_start` into `content`, and the chunk's
/// page. Shared by the live ingest worker and the rebuild path so both
/// produce identical rows.
#[must_use]
pub fn contextualized_chunks(
    mode: ContextualizeMode,
    title: Option<&str>,
    content: &str,
    chunks: &[Chunk],
) -> Vec<IndexChunk> {
    chunks
        .iter()
        .map(|c| match mode {
            ContextualizeMode::Off => IndexChunk::plain(c.text.clone()),
            // `Llm` degrades to structural in the pure path (#216): the LLM
            // prefix, when available, is applied by the async ingest layer;
            // rebuild and air-gap builds stay deterministic.
            ContextualizeMode::Structural | ContextualizeMode::Llm => IndexChunk::contextualized(
                structural_context_prefix(title, &heading_path_at(content, c.byte_start), c.page),
                c.text.clone(),
            ),
        })
        .collect()
}

/// OCR policy for scanned/image PDFs (REQ-NF-05). `Off` ⇒ born-digital only
/// (no OCR runtime needed); scanned PDFs then degrade to `ocr_unavailable`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OcrPolicy {
    #[default]
    Off,
    Auto,
    Force,
}

/// Chunking knobs (the skill's `chunk:` block). Sizes are in characters in
/// v1 (a token≈char proxy); a real tokenizer can replace this behind the
/// same config without touching callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkConfig {
    pub max_chars: usize,
    pub overlap: usize,
}

impl Default for ChunkConfig {
    fn default() -> Self {
        Self {
            max_chars: 1200,
            overlap: 150,
        }
    }
}

/// Extraction configuration (the skill's `extract:` + `chunk:` blocks).
#[derive(Debug, Clone, Copy, Default)]
pub struct ExtractConfig {
    pub ocr: OcrPolicy,
    pub chunk: ChunkConfig,
}

/// Typed extraction failures (REQ-DOC-04 / REQ-NF-05). Each maps to a
/// surfaced `Issue`; on any failure the inbox blob is retained.
#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    #[error("extraction_failed: {0}")]
    ExtractionFailed(String),
    #[error("unsupported_media_type: no extractor accepts `{0}`")]
    Unsupported(String),
    #[error("ocr_unavailable: scanned/image content needs OCR which is not configured")]
    OcrUnavailable,
}

/// Pluggable document extractor (REQ-NF-08: the alternative-extractor seam).
#[async_trait]
pub trait Extractor: Send + Sync {
    /// Stable engine name recorded in `backend_ref.extract_engine`.
    fn name(&self) -> &str;

    /// Whether this extractor handles `mime`.
    fn accepts(&self, mime: &str) -> bool;

    /// Extract text + metadata + chunks from `bytes`.
    async fn extract(
        &self,
        bytes: &[u8],
        mime: &str,
        cfg: &ExtractConfig,
    ) -> Result<ExtractionResult, ExtractError>;
}

/// Real born-digital extractor for `text/*` (plain, markdown). UTF-8 decode
/// + character-window chunking. No native deps — always available offline.
#[derive(Debug, Default)]
pub struct PlainTextExtractor;

#[async_trait]
impl Extractor for PlainTextExtractor {
    fn name(&self) -> &str {
        "plain-text@1"
    }

    fn accepts(&self, mime: &str) -> bool {
        mime == "text/plain"
            || mime == "text/markdown"
            || mime == "text/x-markdown"
            || mime.starts_with("text/")
    }

    async fn extract(
        &self,
        bytes: &[u8],
        _mime: &str,
        cfg: &ExtractConfig,
    ) -> Result<ExtractionResult, ExtractError> {
        let content = std::str::from_utf8(bytes)
            .map_err(|e| ExtractError::ExtractionFailed(format!("not valid UTF-8: {e}")))?
            .to_owned();
        let chunks = chunk_text(&content, cfg.chunk);
        Ok(ExtractionResult {
            content,
            metadata: DocMetadata::default(),
            chunks,
        })
    }
}

/// No-op extractor for pipeline tests that don't care about content.
#[derive(Debug, Default)]
pub struct NullExtractor;

#[async_trait]
impl Extractor for NullExtractor {
    fn name(&self) -> &str {
        "null@1"
    }
    fn accepts(&self, _mime: &str) -> bool {
        true
    }
    async fn extract(
        &self,
        _bytes: &[u8],
        _mime: &str,
        _cfg: &ExtractConfig,
    ) -> Result<ExtractionResult, ExtractError> {
        Ok(ExtractionResult {
            content: String::new(),
            metadata: DocMetadata::default(),
            chunks: Vec::new(),
        })
    }
}

/// In-process PDF/DOCX/PPTX extractor via the kreuzberg crate (REQ-DOC-02,
/// HLD §8). ELv2-licensed; behind the `kreuzberg` cargo feature so the
/// default build stays light. `bundled-pdfium` makes the PDF path
/// self-contained (no system libpdfium). OCR is opt-in: with the `ocr`
/// feature absent, an `OcrPolicy::Force` request returns `ocr_unavailable`
/// rather than silently extracting nothing from a scanned PDF.
#[cfg(feature = "kreuzberg")]
#[derive(Debug, Default)]
pub struct KreuzbergExtractor;

#[cfg(feature = "kreuzberg")]
#[async_trait]
impl Extractor for KreuzbergExtractor {
    fn name(&self) -> &str {
        "kreuzberg@4.9.9"
    }

    fn accepts(&self, mime: &str) -> bool {
        matches!(
            mime,
            "application/pdf"
                | "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
                | "application/vnd.openxmlformats-officedocument.presentationml.presentation"
                | "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        )
    }

    async fn extract(
        &self,
        bytes: &[u8],
        mime: &str,
        cfg: &ExtractConfig,
    ) -> Result<ExtractionResult, ExtractError> {
        use kreuzberg::{ChunkingConfig, ExtractionConfig, PageConfig};

        // OCR is not compiled in (no `ocr`/`paddle-ocr` feature); a Force
        // request on scanned/image content can't be honoured → fail loudly
        // rather than return empty text.
        if matches!(cfg.ocr, OcrPolicy::Force) {
            return Err(ExtractError::OcrUnavailable);
        }

        let kcfg = ExtractionConfig {
            chunking: Some(ChunkingConfig {
                max_characters: cfg.chunk.max_chars,
                overlap: cfg.chunk.overlap,
                ..Default::default()
            }),
            pages: Some(PageConfig {
                extract_pages: true,
                ..Default::default()
            }),
            ..Default::default()
        };

        let r = kreuzberg::extract_bytes(bytes, mime, &kcfg)
            .await
            .map_err(|e| ExtractError::ExtractionFailed(e.to_string()))?;

        let metadata = DocMetadata {
            title: r.metadata.title.clone(),
            authors: r.metadata.authors.clone().unwrap_or_default(),
            page_count: r.metadata.pages.as_ref().map(|p| p.total_count as u32),
            created: None,
        };

        // Prefer kreuzberg's chunks (they carry page provenance); fall back to
        // our own char-window chunker if it produced none (tiny docs).
        let chunks: Vec<Chunk> = match r.chunks.as_ref() {
            Some(ks) if !ks.is_empty() => ks
                .iter()
                .map(|c| Chunk {
                    ordinal: c.metadata.chunk_index as u32,
                    byte_start: c.metadata.byte_start,
                    byte_end: c.metadata.byte_end,
                    page: c.metadata.first_page.map(|p| p as u32),
                    text: c.content.clone(),
                })
                .collect(),
            _ => chunk_text(&r.content, cfg.chunk),
        };

        Ok(ExtractionResult {
            content: r.content,
            metadata,
            chunks,
        })
    }
}

/// The processor seam (REQ-DOC-07): turns raw bytes into an
/// [`ExtractionResult`]. v1 is [`DeterministicProcessor`] (an [`Extractor`]);
/// a future LLM-driven processor slots in here without touching intake or
/// materialize.
#[async_trait]
pub trait DocumentProcessor: Send + Sync {
    /// Engine name recorded in `backend_ref.extract_engine`.
    fn engine(&self) -> String;
    async fn process(
        &self,
        bytes: &[u8],
        mime: &str,
        cfg: &ExtractConfig,
    ) -> Result<ExtractionResult, ExtractError>;
}

/// v1 deterministic processor: delegates to an [`Extractor`] (no LLM).
pub struct DeterministicProcessor {
    extractor: Arc<dyn Extractor>,
}

impl DeterministicProcessor {
    #[must_use]
    pub fn new(extractor: Arc<dyn Extractor>) -> Self {
        Self { extractor }
    }
}

#[async_trait]
impl DocumentProcessor for DeterministicProcessor {
    fn engine(&self) -> String {
        self.extractor.name().to_owned()
    }
    async fn process(
        &self,
        bytes: &[u8],
        mime: &str,
        cfg: &ExtractConfig,
    ) -> Result<ExtractionResult, ExtractError> {
        if !self.extractor.accepts(mime) {
            return Err(ExtractError::Unsupported(mime.to_owned()));
        }
        self.extractor.extract(bytes, mime, cfg).await
    }
}

/// Outcome of one document ingestion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngestOutcome {
    Materialised { page_id: String, chunk_count: usize },
    ExtractionFailed { page_id: String, reason: String },
}

/// The deterministic document-ingest worker (REQ-DOC-07). Runs the slow
/// extract/chunk/embed work **off** the per-tenant write lock, then
/// materialises **under** a brief lock (materialize_document takes the lock
/// only for its transaction). On extraction failure the inbox blob is
/// retained and the instance is marked `extraction_failed` (REQ-DOC-04).
pub struct DocumentIngestWorker {
    indexer: Arc<Indexer>,
    processor: Arc<dyn DocumentProcessor>,
    contextualize: ContextualizeMode,
    /// Variant B (#216): the LLM situating-context generator. `Some` only when
    /// the `contextualize-llm` feature is built and the operator configured an
    /// endpoint; then `Llm` mode uses it (falling back to structural per chunk
    /// on error).
    #[cfg(feature = "contextualize-llm")]
    llm_ctx: Option<Arc<super::contextualize_llm::LlmContextualizer>>,
}

impl DocumentIngestWorker {
    /// Build a worker with the default contextualisation mode
    /// ([`ContextualizeMode::Structural`], GH #216 Variant A).
    #[must_use]
    pub fn new(indexer: Arc<Indexer>, processor: Arc<dyn DocumentProcessor>) -> Self {
        Self {
            indexer,
            processor,
            contextualize: ContextualizeMode::default(),
            #[cfg(feature = "contextualize-llm")]
            llm_ctx: None,
        }
    }

    /// Override the contextualisation mode (builder style). `Off` restores the
    /// legacy byte-for-byte chunk text.
    #[must_use]
    pub fn with_contextualize(mut self, mode: ContextualizeMode) -> Self {
        self.contextualize = mode;
        self
    }

    /// Attach the Variant B LLM contextualizer (#216, builder style). Only
    /// available under the `contextualize-llm` feature.
    #[cfg(feature = "contextualize-llm")]
    #[must_use]
    pub fn with_llm_contextualizer(
        mut self,
        ctx: Arc<super::contextualize_llm::LlmContextualizer>,
    ) -> Self {
        self.llm_ctx = Some(ctx);
        self
    }

    /// Build the storage chunks for this worker's contextualize mode. The
    /// default (and any non-`Llm` mode) is the pure, deterministic path; under
    /// the `contextualize-llm` feature, `Llm` mode asks the LLM for a per-chunk
    /// situating context and falls back to the structural prefix on error.
    async fn build_chunks(
        &self,
        title: Option<&str>,
        content: &str,
        chunks: &[Chunk],
    ) -> Vec<IndexChunk> {
        #[cfg(feature = "contextualize-llm")]
        if self.contextualize == ContextualizeMode::Llm
            && let Some(ctx) = &self.llm_ctx
        {
            let mut out = Vec::with_capacity(chunks.len());
            for c in chunks {
                let headings = heading_path_at(content, c.byte_start);
                let prefix = ctx
                    .context_prefix(title, &headings, c.page, &c.text)
                    .await
                    .or_else(|| structural_context_prefix(title, &headings, c.page));
                out.push(IndexChunk::contextualized(prefix, c.text.clone()));
            }
            return out;
        }
        contextualized_chunks(self.contextualize, title, content, chunks)
    }

    /// Ingest the inbox blob `blob_id` as instance `skill::instance_id`.
    pub async fn ingest(
        &self,
        blob_id: &BlobId,
        mime: &str,
        skill: &str,
        instance_id: &str,
        cfg: &ExtractConfig,
        extra: &serde_json::Value,
    ) -> Result<IngestOutcome, IndexerError> {
        let page_id = format!("markdown/instances/{skill}/{instance_id}.md");
        let bytes = self.indexer.read_inbox_blob(blob_id).await?;

        // Extract + chunk OFF the write lock.
        match self.processor.process(&bytes, mime, cfg).await {
            Ok(result) => {
                // Contextual Retrieval, Variant A (GH #216): situate each
                // chunk with `[<title> › <heading path> › p.<page>]`. The
                // context is stored beside the verbatim body and feeds only
                // the retrieval representations (dense embedding, FTS,
                // rerank) — display text and byte-span provenance stay clean.
                let chunks = self
                    .build_chunks(
                        result.metadata.title.as_deref(),
                        &result.content,
                        &result.chunks,
                    )
                    .await;
                let overlay = document_overlay(
                    skill,
                    instance_id,
                    blob_id,
                    chunks.len(),
                    &self.processor.engine(),
                    "ok",
                    &result.metadata,
                    extra,
                );
                self.indexer
                    .materialize_document(&page_id, &overlay, &chunks)
                    .await?;
                // Promote the inbox blob to the canonical area.
                self.indexer.promote_blob(blob_id).await?;
                Ok(IngestOutcome::Materialised {
                    page_id,
                    chunk_count: chunks.len(),
                })
            }
            Err(e) => {
                // Retain the inbox blob (do NOT promote); mark the instance
                // extraction_failed with a zero-chunk overlay so it is
                // queryable and the upload is never lost.
                let reason = e.to_string();
                let overlay = document_overlay(
                    skill,
                    instance_id,
                    blob_id,
                    0,
                    &self.processor.engine(),
                    "extraction_failed",
                    &DocMetadata::default(),
                    extra,
                );
                self.indexer
                    .materialize_document(&page_id, &overlay, &[])
                    .await?;
                Ok(IngestOutcome::ExtractionFailed { page_id, reason })
            }
        }
    }
}

/// Re-derive every document instance's chunks from its retained canonical
/// blob (rebuild step, REQ-NF-01). The main rebuild loop re-indexes the
/// overlay markdown as an ordinary page (one block); this runs after and
/// re-materialises the correct chunk-blocks from the blob, so a from-scratch
/// DuckDB over the same `pages/` + `blobs/` is fully reconstructed.
///
/// v1 reconstructs born-digital **text** (the only kind materialisable while
/// kreuzberg is gated on the MSRV decision); a non-UTF-8 blob is left for the
/// kreuzberg path and reported by `audit_documents`, not silently dropped.
/// `extraction_failed` instances are left as-is (no chunks).
pub(crate) async fn rebuild_documents(indexer: &Indexer) -> Result<(), IndexerError> {
    use escurel_storage::{BlobId, Key};

    let overlays = enumerate_document_overlays(indexer).await?;
    let store = indexer.lane_store();
    for ov in overlays {
        if ov.status != "ok" {
            continue;
        }
        let Some(blob_id) = BlobId::parse(&ov.blob_id) else {
            continue;
        };
        let Ok(bytes) = indexer.read_blob(&blob_id).await else {
            continue; // orphan blob — reported by audit_documents
        };
        let Ok(content) = std::str::from_utf8(&bytes) else {
            continue; // non-text (kreuzberg-gated); not reconstructable in v1
        };
        // Chunk knobs from the skill binding.
        let (max_chars, overlap) = indexer
            .skill_backend(&ov.skill)
            .await
            .ok()
            .and_then(|b| b.document)
            .map(|d| (d.max_chars, d.overlap))
            .unwrap_or((None, None));
        let defaults = ChunkConfig::default();
        let cfg = ChunkConfig {
            max_chars: max_chars.unwrap_or(defaults.max_chars),
            overlap: overlap.unwrap_or(defaults.overlap),
        };
        let raw_chunks = chunk_text(content, cfg);
        // The overlay markdown is already canonical on the lane (re-written by
        // the main rebuild loop); re-materialise to replace its blocks with
        // the freshly re-chunked content.
        let Ok(key) = Key::new(indexer.tenant(), ov.page_id.clone()) else {
            continue;
        };
        let Ok(overlay_bytes) = store.read(&key).await else {
            continue;
        };
        let Ok(overlay_md) = String::from_utf8(overlay_bytes.to_vec()) else {
            continue;
        };
        // Apply the same structural contextualisation as the live ingest path
        // (GH #216, Variant A) so a from-scratch rebuild reproduces identical
        // stored rows — this is also the operator's cutover/re-embed path
        // after flipping `ESCUREL_INGEST_CONTEXTUALIZE`. The title comes from
        // the overlay's `# <title>` heading; heading paths come from the blob
        // content; a plain re-chunk has no per-chunk page.
        let title = match indexer.contextualize {
            ContextualizeMode::Off => None,
            // `Llm` re-materialises with the structural prefix in the pure
            // rebuild path (#216); a from-scratch rebuild is deterministic.
            ContextualizeMode::Structural | ContextualizeMode::Llm => {
                overlay_heading_title(&overlay_md)
            }
        };
        let chunks = contextualized_chunks(
            indexer.contextualize,
            title.as_deref(),
            content,
            &raw_chunks,
        );
        indexer
            .materialize_document(&ov.page_id, &overlay_md, &chunks)
            .await?;
    }
    Ok(())
}

/// Reconcile document state for `audit` (REQ-NF-02): a document overlay whose
/// canonical blob is missing is an orphan; a healthy one with status `ok`
/// must have its blob retained. Returns `(page_id, reason)` for each problem.
pub(crate) async fn audit_documents(
    indexer: &Indexer,
) -> Result<Vec<(String, String)>, IndexerError> {
    use escurel_storage::BlobId;
    let mut problems = Vec::new();
    for ov in enumerate_document_overlays(indexer).await? {
        match BlobId::parse(&ov.blob_id) {
            None => problems.push((ov.page_id, format!("invalid blob_id `{}`", ov.blob_id))),
            Some(id) => {
                if ov.status == "ok" && indexer.read_blob(&id).await.is_err() {
                    problems.push((
                        ov.page_id,
                        "canonical blob missing for ok instance".to_owned(),
                    ));
                }
            }
        }
    }
    // The mirror direction: a canonical blob no overlay references (a
    // materialise that failed after promotion, or a deleted instance). Keyed
    // by the blob id so the operator can see what `rebuild` will reclaim.
    let referenced = referenced_blob_ids(indexer).await?;
    for id in indexer.lane_store().list_blobs(indexer.tenant()).await? {
        if !referenced.contains(id.as_str()) {
            problems.push((
                id.as_str().to_owned(),
                "orphan blob (no overlay)".to_owned(),
            ));
        }
    }
    Ok(problems)
}

/// The set of canonical blob ids referenced by a live document overlay.
async fn referenced_blob_ids(
    indexer: &Indexer,
) -> Result<std::collections::HashSet<String>, IndexerError> {
    Ok(enumerate_document_overlays(indexer)
        .await?
        .into_iter()
        .map(|o| o.blob_id)
        .collect())
}

/// Reclaim canonical blobs no overlay references (REQ-NF-02). Returns the
/// count removed. Inbox blobs are *not* touched — an `extraction_failed`
/// upload is deliberately retained there for reprocessing (REQ-DOC-04).
pub(crate) async fn reclaim_orphan_blobs(indexer: &Indexer) -> Result<usize, IndexerError> {
    let referenced = referenced_blob_ids(indexer).await?;
    let store = indexer.lane_store();
    let tenant = indexer.tenant();
    let mut removed = 0;
    for id in store.list_blobs(tenant).await? {
        if !referenced.contains(id.as_str()) {
            store.delete_blob(tenant, &id).await?;
            removed += 1;
        }
    }
    Ok(removed)
}

struct DocOverlay {
    page_id: String,
    skill: String,
    blob_id: String,
    status: String,
}

async fn enumerate_document_overlays(indexer: &Indexer) -> Result<Vec<DocOverlay>, IndexerError> {
    let conn = indexer.conn.lock().await;
    let mut stmt = conn.prepare(
        "SELECT page_id, skill, \
         json_extract_string(frontmatter, '$.backend_ref.blob_id'), \
         json_extract_string(frontmatter, '$.backend_ref.status') \
         FROM pages \
         WHERE page_type = 'instance' \
           AND json_extract_string(frontmatter, '$.backend_ref.kind') = 'document'",
    )?;
    let rows: Vec<DocOverlay> = stmt
        .query_map([], |r| {
            Ok(DocOverlay {
                page_id: r.get(0)?,
                skill: r.get(1)?,
                blob_id: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                status: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
            })
        })?
        .collect::<Result<_, _>>()?;
    Ok(rows)
}

/// Build the document instance's overlay markdown with its `backend_ref`.
#[allow(clippy::too_many_arguments)]
fn document_overlay(
    skill: &str,
    id: &str,
    blob_id: &BlobId,
    chunk_count: usize,
    engine: &str,
    status: &str,
    meta: &DocMetadata,
    extra: &serde_json::Value,
) -> String {
    // Extra caller-supplied top-level frontmatter (e.g. the offline loader's
    // per-doc metadata: nummer/titel/wp/doctype/…). Each JSON value serialises
    // to a valid YAML flow scalar; a `titel`/`title` here wins the heading.
    let mut extra_block = String::new();
    if let Some(obj) = extra.as_object() {
        for (k, v) in obj {
            extra_block.push_str(&format!(
                "{k}: {}\n",
                serde_json::to_string(v).unwrap_or_default()
            ));
        }
    }
    let title = extra
        .get("titel")
        .or_else(|| extra.get("title"))
        .and_then(|v| v.as_str())
        .map(str::to_owned)
        .or_else(|| meta.title.clone())
        .unwrap_or_else(|| id.to_owned());
    let mut extracted = String::new();
    if let Some(pages) = meta.page_count {
        extracted.push_str(&format!("    pages: {pages}\n"));
    }
    if !meta.authors.is_empty() {
        extracted.push_str(&format!("    authors: [{}]\n", meta.authors.join(", ")));
    }
    let extracted_block = if extracted.is_empty() {
        String::new()
    } else {
        format!("  extracted:\n{extracted}")
    };
    format!(
        "---\n\
         type: instance\n\
         skill: {skill}\n\
         id: {id}\n\
         backend_ref:\n\
        \x20 kind: document\n\
        \x20 blob_id: {blob}\n\
        \x20 chunk_count: {chunk_count}\n\
        \x20 extract_engine: {engine}\n\
        \x20 status: {status}\n\
         {extracted_block}\
         {extra_block}\
         ---\n\
         # {title}\n",
        blob = blob_id.as_str(),
    )
}

/// Extract the document title from a document overlay's `# <title>` heading
/// (the line `document_overlay` writes). Returns `None` when no `# ` heading
/// is present in the body. Used by the rebuild path, which only has the
/// canonical overlay markdown to recover the title from.
fn overlay_heading_title(overlay_md: &str) -> Option<String> {
    let body = escurel_md::parse(overlay_md).ok()?.body;
    body.lines().find_map(|l| {
        l.strip_prefix("# ")
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_owned)
    })
}

/// Character-window chunking with overlap, split on a UTF-8 char boundary.
/// Each chunk carries its `byte_start..byte_end` span into `content` and a
/// 0-based ordinal. A single-page (no page map) document leaves `page = None`.
#[must_use]
pub fn chunk_text(content: &str, cfg: ChunkConfig) -> Vec<Chunk> {
    let max = cfg.max_chars.max(1);
    let overlap = cfg.overlap.min(max - 1);
    let step = max - overlap;

    // Char-boundary byte offsets, plus the end sentinel.
    let mut offsets: Vec<usize> = content.char_indices().map(|(i, _)| i).collect();
    offsets.push(content.len());
    let n_chars = offsets.len() - 1;
    if n_chars == 0 {
        return Vec::new();
    }

    let mut chunks = Vec::new();
    let mut start_char = 0usize;
    let mut ordinal = 0u32;
    while start_char < n_chars {
        let end_char = (start_char + max).min(n_chars);
        let byte_start = offsets[start_char];
        let byte_end = offsets[end_char];
        chunks.push(Chunk {
            ordinal,
            byte_start,
            byte_end,
            page: None,
            text: content[byte_start..byte_end].to_owned(),
        });
        ordinal += 1;
        if end_char == n_chars {
            break;
        }
        start_char += step;
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn plain_text_extracts_content_and_chunk_spans() {
        let body = "alpha beta gamma delta epsilon zeta eta theta".as_bytes();
        let ex = PlainTextExtractor;
        assert!(ex.accepts("text/plain"));
        assert!(ex.accepts("text/markdown"));
        let cfg = ExtractConfig {
            ocr: OcrPolicy::Off,
            chunk: ChunkConfig {
                max_chars: 12,
                overlap: 4,
            },
        };
        let r = ex.extract(body, "text/plain", &cfg).await.unwrap();
        assert_eq!(r.content, std::str::from_utf8(body).unwrap());
        assert!(r.chunks.len() > 1, "should split into multiple chunks");
        // Spans index back into content, ordinals are sequential.
        for (i, c) in r.chunks.iter().enumerate() {
            assert_eq!(c.ordinal as usize, i);
            assert_eq!(&r.content[c.byte_start..c.byte_end], c.text);
        }
        // First chunk starts at the beginning, last reaches the end.
        assert_eq!(r.chunks.first().unwrap().byte_start, 0);
        assert_eq!(r.chunks.last().unwrap().byte_end, r.content.len());
    }

    #[tokio::test]
    async fn invalid_utf8_is_typed_extraction_failed() {
        let ex = PlainTextExtractor;
        let err = ex
            .extract(&[0xff, 0xfe, 0x00], "text/plain", &ExtractConfig::default())
            .await
            .unwrap_err();
        assert!(
            matches!(err, ExtractError::ExtractionFailed(_)),
            "got {err:?}"
        );
    }

    #[test]
    fn chunk_text_handles_multibyte_on_boundaries() {
        // "héllo wörld …" — ensure we never split inside a multibyte char.
        let content = "héllo wörld ☃ snowman café";
        let chunks = chunk_text(
            content,
            ChunkConfig {
                max_chars: 5,
                overlap: 1,
            },
        );
        assert!(!chunks.is_empty());
        for c in &chunks {
            // Slicing on the recorded spans must not panic (valid boundaries).
            assert_eq!(&content[c.byte_start..c.byte_end], c.text);
        }
        assert_eq!(chunks.last().unwrap().byte_end, content.len());
    }

    #[test]
    fn chunk_text_empty_is_no_chunks() {
        assert!(chunk_text("", ChunkConfig::default()).is_empty());
    }

    #[test]
    fn structural_prefix_title_and_page() {
        assert_eq!(
            structural_context_prefix(Some("Q3 Report"), &[], Some(4)).as_deref(),
            Some("[Q3 Report \u{203a} p.4]")
        );
    }

    #[test]
    fn structural_prefix_title_only() {
        assert_eq!(
            structural_context_prefix(Some("Q3 Report"), &[], None).as_deref(),
            Some("[Q3 Report]")
        );
        // An empty/blank title is treated as absent.
        assert_eq!(structural_context_prefix(Some("  "), &[], None), None);
    }

    #[test]
    fn structural_prefix_page_only() {
        assert_eq!(
            structural_context_prefix(None, &[], Some(9)).as_deref(),
            Some("[p.9]")
        );
    }

    #[test]
    fn structural_prefix_nothing_is_none() {
        assert_eq!(structural_context_prefix(None, &[], None), None);
    }

    #[test]
    fn structural_prefix_full_heading_path() {
        let headings = vec!["Finance".to_owned(), "Q3 Margins".to_owned()];
        assert_eq!(
            structural_context_prefix(Some("Annual Report"), &headings, Some(12)).as_deref(),
            Some("[Annual Report \u{203a} Finance \u{203a} Q3 Margins \u{203a} p.12]")
        );
        // Headings alone still situate an untitled document.
        assert_eq!(
            structural_context_prefix(None, &headings, None).as_deref(),
            Some("[Finance \u{203a} Q3 Margins]")
        );
    }

    #[test]
    fn structural_prefix_dedupes_heading_equal_to_title() {
        // A `# <title>` H1 repeating the document title is not duplicated.
        let headings = vec!["Annual Report".to_owned(), "Finance".to_owned()];
        assert_eq!(
            structural_context_prefix(Some("Annual Report"), &headings, None).as_deref(),
            Some("[Annual Report \u{203a} Finance]")
        );
    }

    #[test]
    fn heading_path_tracks_the_open_hierarchy() {
        let md = "# Title\nintro\n## Alpha\nbody a\n### Deep\nbody d\n## Beta\nbody b\n";
        let at = |needle: &str| md.find(needle).unwrap();
        assert_eq!(heading_path_at(md, at("intro")), vec!["Title"]);
        assert_eq!(heading_path_at(md, at("body a")), vec!["Title", "Alpha"]);
        assert_eq!(
            heading_path_at(md, at("body d")),
            vec!["Title", "Alpha", "Deep"]
        );
        // `## Beta` pops both `Alpha` and `Deep`.
        assert_eq!(heading_path_at(md, at("body b")), vec!["Title", "Beta"]);
        // Offset 0: nothing is open yet.
        assert!(heading_path_at(md, 0).is_empty());
    }

    #[test]
    fn heading_path_ignores_non_headings() {
        let md = "#hashtag not a heading\n####### seven hashes\nplain\ntext here\n";
        assert!(heading_path_at(md, md.len()).is_empty());
    }

    #[test]
    fn llm_mode_parses_and_degrades_to_structural_in_the_pure_path() {
        // #216 Variant B: `llm` parses to its own mode, but the pure/air-gap
        // path (no `contextualize-llm` feature, or no configured endpoint)
        // produces the SAME rows as `Structural` — deterministic and offline.
        assert_eq!(ContextualizeMode::parse("llm"), ContextualizeMode::Llm);
        let content = "# Manual\n## Setup\nInstall the widget.\n";
        let start = content.find("Install").unwrap();
        let chunks = vec![Chunk {
            ordinal: 0,
            byte_start: start,
            byte_end: start + "Install the widget.".len(),
            page: Some(3),
            text: "Install the widget.".to_owned(),
        }];
        let via_llm =
            contextualized_chunks(ContextualizeMode::Llm, Some("Manual"), content, &chunks);
        let via_structural = contextualized_chunks(
            ContextualizeMode::Structural,
            Some("Manual"),
            content,
            &chunks,
        );
        assert_eq!(via_llm, via_structural);
    }

    #[test]
    fn contextualized_chunks_builds_the_storage_split() {
        let content = "## Ops\nthe body text\n";
        let chunks = vec![Chunk {
            ordinal: 0,
            byte_start: content.find("the body").unwrap(),
            byte_end: content.len(),
            page: Some(2),
            text: "the body text\n".to_owned(),
        }];
        let out = contextualized_chunks(
            ContextualizeMode::Structural,
            Some("Manual"),
            content,
            &chunks,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].context.as_deref(),
            Some("[Manual \u{203a} Ops \u{203a} p.2]")
        );
        assert_eq!(out[0].body, "the body text\n", "body stays verbatim");
        assert_eq!(
            out[0].embed_text(),
            "[Manual \u{203a} Ops \u{203a} p.2]\nthe body text\n",
            "the embedder sees context + body"
        );
        // Off mode: no context, byte-for-byte legacy behaviour.
        let off = contextualized_chunks(ContextualizeMode::Off, Some("Manual"), content, &chunks);
        assert_eq!(off[0].context, None);
        assert_eq!(off[0].embed_text(), "the body text\n");
    }

    #[test]
    fn contextualize_mode_parse() {
        assert_eq!(ContextualizeMode::parse("off"), ContextualizeMode::Off);
        assert_eq!(
            ContextualizeMode::parse("structural"),
            ContextualizeMode::Structural
        );
        assert_eq!(
            ContextualizeMode::parse("OFF"),
            ContextualizeMode::Off,
            "case-insensitive"
        );
        assert_eq!(
            ContextualizeMode::parse("bogus"),
            ContextualizeMode::Structural,
            "unknown → default"
        );
    }

    #[test]
    fn overlay_heading_title_reads_h1() {
        let md = "---\ntype: instance\nid: x\n---\n# My Document Title\n";
        assert_eq!(
            overlay_heading_title(md).as_deref(),
            Some("My Document Title")
        );
        let no_heading = "---\ntype: instance\nid: x\n---\njust body\n";
        assert_eq!(overlay_heading_title(no_heading), None);
    }
}
