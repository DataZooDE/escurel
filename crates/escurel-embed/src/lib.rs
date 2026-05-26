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
#[cfg(feature = "gemini")]
mod gemini;
mod hash;
mod reloadable;
mod zero;

#[cfg(feature = "candle")]
pub use crate::candle::CandleEmbedder;
#[cfg(feature = "gemini")]
pub use gemini::GeminiEmbedder;
pub use hash::HashEmbedder;
pub use reloadable::ReloadableEmbedder;
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

    /// Embed a batch of texts. The returned `Vec<Vec<f32>>` has
    /// exactly `texts.len()` rows, each of length [`Self::dim`].
    async fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError>;
}
