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
//! ## kreuzberg (PDF/DOCX) — gated on an MSRV decision
//!
//! `KreuzbergExtractor` (PDF/DOCX/PPTX via the in-process kreuzberg crate,
//! `bundled-pdfium`) is **not wired yet**: kreuzberg 4.9.9 requires
//! `rust-version = 1.91`, but this workspace pins `1.88`. Adopting it needs a
//! workspace MSRV bump (see
//! `docs/notes/discovered/2026-06-21-kreuzberg-msrv-191.md`). The trait keeps
//! it swappable (REQ-NF-08, ELv2) once that decision lands.

use std::sync::Arc;

use async_trait::async_trait;
use escurel_storage::BlobId;

use crate::{Indexer, IndexerError};

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
}

impl DocumentIngestWorker {
    #[must_use]
    pub fn new(indexer: Arc<Indexer>, processor: Arc<dyn DocumentProcessor>) -> Self {
        Self { indexer, processor }
    }

    /// Ingest the inbox blob `blob_id` as instance `skill::instance_id`.
    pub async fn ingest(
        &self,
        blob_id: &BlobId,
        mime: &str,
        skill: &str,
        instance_id: &str,
        cfg: &ExtractConfig,
    ) -> Result<IngestOutcome, IndexerError> {
        let page_id = format!("markdown/instances/{skill}/{instance_id}.md");
        let bytes = self.indexer.read_inbox_blob(blob_id).await?;

        // Extract + chunk OFF the write lock.
        match self.processor.process(&bytes, mime, cfg).await {
            Ok(result) => {
                let chunk_texts: Vec<String> =
                    result.chunks.iter().map(|c| c.text.clone()).collect();
                let overlay = document_overlay(
                    skill,
                    instance_id,
                    blob_id,
                    chunk_texts.len(),
                    &self.processor.engine(),
                    "ok",
                    &result.metadata,
                );
                self.indexer
                    .materialize_document(&page_id, &overlay, &chunk_texts)
                    .await?;
                // Promote the inbox blob to the canonical area.
                self.indexer.promote_blob(blob_id).await?;
                Ok(IngestOutcome::Materialised {
                    page_id,
                    chunk_count: chunk_texts.len(),
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
                );
                self.indexer
                    .materialize_document(&page_id, &overlay, &[])
                    .await?;
                Ok(IngestOutcome::ExtractionFailed { page_id, reason })
            }
        }
    }
}

/// Build the document instance's overlay markdown with its `backend_ref`.
fn document_overlay(
    skill: &str,
    id: &str,
    blob_id: &BlobId,
    chunk_count: usize,
    engine: &str,
    status: &str,
    meta: &DocMetadata,
) -> String {
    let title = meta.title.clone().unwrap_or_else(|| id.to_owned());
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
         ---\n\
         # {title}\n",
        blob = blob_id.as_str(),
    )
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
}
