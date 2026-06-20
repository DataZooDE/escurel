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

use async_trait::async_trait;

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
