//! Deterministic hash-based embedder for tests and local dev.

use async_trait::async_trait;
use sha2::{Digest, Sha256};

use crate::{EmbedError, Embedder};

/// `Embedder` that produces a deterministic vector from each
/// input text's SHA-256 hash.
///
/// Same text → same vector across runs and hosts. Different
/// texts → different vectors (modulo the trivial hash-collision
/// probability). Vectors are L2-normalised so cosine distance is
/// well-defined and ranks meaningfully under DuckDB `vss`.
///
/// Useful for tests that need to exercise the search path's
/// vector half (`vss` HNSW + RRF fusion) without pulling a real
/// model in. **Not** a substitute for a real semantic embedder —
/// the resulting "neighbourhood" structure is hashing noise, not
/// language meaning. The ADR-0001 retrieval-quality gate
/// (`docs/adr/0001-duckdb-only-storage.md §Pre-deployment gate`)
/// must run against a real embedder (M2.2).
#[derive(Debug, Clone)]
pub struct HashEmbedder {
    dim: usize,
}

impl HashEmbedder {
    #[must_use]
    pub fn new(dim: usize) -> Self {
        Self { dim }
    }
}

impl Default for HashEmbedder {
    /// Defaults to 768 — the EmbeddingGemma dimension Escurel ships.
    fn default() -> Self {
        Self::new(768)
    }
}

#[async_trait]
impl Embedder for HashEmbedder {
    fn dim(&self) -> usize {
        self.dim
    }

    fn model_id(&self) -> String {
        "hash".to_owned()
    }

    async fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        Ok(texts.iter().map(|t| hash_to_vec(t, self.dim)).collect())
    }
}

fn hash_to_vec(text: &str, dim: usize) -> Vec<f32> {
    let hash = sha256_bytes(text);
    let mut v: Vec<f32> = (0..dim)
        .map(|i| f32::from(hash[i % 32]) / 255.0 - 0.5)
        .collect();
    l2_normalize(&mut v);
    v
}

fn sha256_bytes(text: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    let out = hasher.finalize();
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&out);
    arr
}

fn l2_normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}
