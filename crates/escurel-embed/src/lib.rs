//! Embedding abstraction for Escurel.
//!
//! `Embedder` is the seam the indexer talks to when it needs to
//! turn block bodies into vectors for the `blocks.dense_vec`
//! column. This crate ships:
//!
//! - [`Embedder`] trait — async, batched, dimension-aware.
//! - [`ZeroEmbedder`] — a stub that returns zero vectors of the
//!   configured dimension. Useful for tests, for local
//!   development without a model, and as the default while the
//!   real candle-backed `EmbeddingGemma` impl (M2.2) and Gemini
//!   API impl (M2.3) are wired in.
//! - [`Reranker`] trait + [`NoopReranker`] — the second-stage
//!   cross-encoder seam (spec decision 5). The Noop default
//!   preserves first-stage order until a real CE head lands.
//!
//! ## Why a trait
//!
//! Spec decision 5 (`docs/spec/README.md §Locked decisions`)
//! enumerates the implementations we need over time:
//! EmbeddingGemma via candle (default), Gemini HTTP (optional),
//! a sidecar adapter (post-v1). All three share this trait. The
//! per-tenant embed worker pool (decision 11, planned for M3)
//! wraps an `Arc<dyn Embedder>` behind a bounded queue.

#[cfg(feature = "candle")]
mod candle;
#[cfg(feature = "rerank")]
mod cross_encoder;
#[cfg(feature = "gemini")]
mod gemini;
mod hash;
mod reloadable;
mod reranker;
mod zero;

#[cfg(feature = "candle")]
pub use crate::candle::CandleEmbedder;
#[cfg(feature = "rerank")]
pub use cross_encoder::CrossEncoderReranker;
#[cfg(feature = "gemini")]
pub use gemini::GeminiEmbedder;
pub use hash::HashEmbedder;
pub use reloadable::ReloadableEmbedder;
pub use reranker::{Candidate, NoopReranker, Ranked, Reranker};
pub use zero::ZeroEmbedder;

use async_trait::async_trait;
use thiserror::Error;

/// Errors returned by [`Embedder`] implementations.
#[derive(Debug, Error)]
pub enum EmbedError {
    /// The underlying model produced a vector whose length did not
    /// match the embedder's declared [`Embedder::dim`].
    #[error("embedder produced a vector of length {got}, expected {expected}")]
    DimensionMismatch { expected: usize, got: usize },

    /// The model load or invocation failed for an implementation-
    /// specific reason. Real impls (candle, Gemini) downcast as needed.
    #[error("embedder backend error: {0}")]
    Backend(String),
}

/// An embedding model.
///
/// `embed` is batched so callers can amortise per-call overhead
/// (especially for HTTP-backed impls); single-text callers pass
/// a one-element slice.
#[async_trait]
pub trait Embedder: Send + Sync + 'static {
    /// The dimensionality of every vector this embedder produces.
    /// `escurel-index` declares the `blocks.dense_vec` column as
    /// `FLOAT[768]` (EmbeddingGemma default) and rejects any
    /// embedder whose `dim()` does not match at indexer setup.
    fn dim(&self) -> usize;

    /// A stable identity for the embedding *model* this embedder uses, e.g.
    /// `gemini-embedding-001`, `google/embeddinggemma-300m`, `hash`, `zero`.
    /// Together with [`Self::dim`] it pins the embedding space: vectors from
    /// two embedders are interchangeable only when both `model_id()` and
    /// `dim()` match. The offline batch loader records this in its artifact
    /// manifest and the transfer refuses a tenant whose embedder disagrees
    /// (mixing embedding spaces silently destroys retrieval).
    ///
    /// Owned (not `&str`) so wrappers like `ReloadableEmbedder` can delegate
    /// through their swap guard. Called rarely (manifest write / validation).
    fn model_id(&self) -> String;

    /// Embed a batch of texts. The returned `Vec<Vec<f32>>` has
    /// exactly `texts.len()` rows, each of length [`Self::dim`].
    async fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError>;
}

#[cfg(test)]
mod model_id_tests {
    use super::*;

    #[test]
    fn stub_embedders_report_stable_distinct_model_ids() {
        // The transfer pins the embedding space on (model_id, dim); these must
        // be stable + distinct so a mismatched artifact is rejected.
        assert_eq!(ZeroEmbedder::new(768).model_id(), "zero");
        assert_eq!(HashEmbedder::new(768).model_id(), "hash");
        // Two equivalent embedders agree (idempotent identity).
        assert_eq!(
            HashEmbedder::default().model_id(),
            HashEmbedder::new(768).model_id()
        );
        assert_ne!(
            ZeroEmbedder::new(768).model_id(),
            HashEmbedder::new(768).model_id()
        );
    }

    #[cfg(feature = "gemini")]
    #[test]
    fn gemini_model_id_is_the_model_name() {
        let e = GeminiEmbedder::new("k".to_owned());
        assert_eq!(e.model_id(), "gemini-embedding-001");
        assert_eq!(
            e.with_model("text-embedding-004").model_id(),
            "text-embedding-004"
        );
    }
}
