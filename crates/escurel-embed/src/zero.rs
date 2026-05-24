//! Stub embedder that returns zero vectors of a configured dimension.

use async_trait::async_trait;

use crate::{EmbedError, Embedder};

/// `Embedder` that returns an all-zero vector for every input.
///
/// Useful for:
/// - Tests that want to round-trip through `update_page` without
///   pulling in a real model.
/// - Local development without candle or network egress.
/// - The default while EmbeddingGemma (M2.2) and Gemini (M2.3)
///   are wired in.
///
/// Zero vectors are pathological for nearest-neighbour search —
/// they are mutually equidistant under cosine distance — so the
/// retrieval-quality gate (`docs/adr/0001-duckdb-only-storage.md
/// §Pre-deployment gate`) needs a real embedder. This is fine for
/// the indexer's write path which just needs *something* of the
/// right shape in `blocks.dense_vec`.
#[derive(Debug, Clone)]
pub struct ZeroEmbedder {
    dim: usize,
}

impl ZeroEmbedder {
    /// Build a `ZeroEmbedder` with the given vector dimension.
    /// The default for Escurel is 768 (EmbeddingGemma).
    #[must_use]
    pub fn new(dim: usize) -> Self {
        Self { dim }
    }
}

impl Default for ZeroEmbedder {
    /// Defaults to 768 — the EmbeddingGemma dimension Escurel ships.
    fn default() -> Self {
        Self::new(768)
    }
}

#[async_trait]
impl Embedder for ZeroEmbedder {
    fn dim(&self) -> usize {
        self.dim
    }

    async fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        Ok((0..texts.len()).map(|_| vec![0.0_f32; self.dim]).collect())
    }
}
