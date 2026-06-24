//! Google Gemini HTTP-backed embedder.
//!
//! Behind the `gemini` Cargo feature so reqwest doesn't drag into
//! the default build. Useful for tenants that want hosted-model
//! quality without running candle locally; not air-gapped.
//!
//! API: <https://ai.google.dev/api/embeddings#method:-models.batchEmbedContents>.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use crate::{EmbedError, Embedder};

/// Gemini's `batchEmbedContents` rejects a batch with more than 100 requests
/// (`INVALID_ARGUMENT: at most 100 requests can be in one batch`). A single
/// document can chunk into more than that, so we split into ≤100-text calls.
const MAX_BATCH: usize = 100;

/// HTTP-backed embedder calling `models.batchEmbedContents` on the
/// Gemini API.
///
/// Configuration is built via [`GeminiEmbedder::new`] (API key only)
/// plus optional [`GeminiEmbedder::with_base_url`] /
/// [`GeminiEmbedder::with_model`] / [`GeminiEmbedder::with_dim`].
/// `base_url` lets tests point at a mock server.
#[derive(Debug, Clone)]
pub struct GeminiEmbedder {
    api_key: String,
    base_url: String,
    model: String,
    dim: usize,
    client: reqwest::Client,
}

impl GeminiEmbedder {
    /// Build a `GeminiEmbedder` with the given API key. Defaults:
    /// - base URL: `https://generativelanguage.googleapis.com`
    /// - model: `gemini-embedding-001`
    /// - dim: 768 (the EmbeddingGemma dimension Escurel ships)
    #[must_use]
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: "https://generativelanguage.googleapis.com".to_owned(),
            model: "gemini-embedding-001".to_owned(),
            dim: 768,
            client: reqwest::Client::new(),
        }
    }

    #[must_use]
    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    #[must_use]
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    #[must_use]
    pub fn with_dim(mut self, dim: usize) -> Self {
        self.dim = dim;
        self
    }
}

#[async_trait]
impl Embedder for GeminiEmbedder {
    fn dim(&self) -> usize {
        self.dim
    }

    fn model_id(&self) -> String {
        self.model.clone()
    }

    async fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        // Gemini caps a batch at 100 requests; split larger inputs (one doc can
        // chunk into hundreds) into ≤100-text calls, preserving order.
        let mut out = Vec::with_capacity(texts.len());
        for batch in texts.chunks(MAX_BATCH) {
            out.extend(self.embed_batch(batch).await?);
        }
        Ok(out)
    }
}

impl GeminiEmbedder {
    /// Embed a single ≤[`MAX_BATCH`] batch via one `batchEmbedContents` call.
    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        let requests: Vec<_> = texts
            .iter()
            .map(|t| {
                json!({
                    "model": format!("models/{}", self.model),
                    "content": { "parts": [{ "text": t }] },
                    "outputDimensionality": self.dim,
                })
            })
            .collect();
        let body = json!({ "requests": requests });

        let url = format!(
            "{}/v1beta/models/{}:batchEmbedContents?key={}",
            self.base_url, self.model, self.api_key,
        );

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| EmbedError::Backend(format!("gemini HTTP send: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(EmbedError::Backend(format!("gemini HTTP {status}: {body}")));
        }

        let parsed: BatchResponse = resp
            .json()
            .await
            .map_err(|e| EmbedError::Backend(format!("gemini JSON parse: {e}")))?;

        // Validate every returned vector has the configured dim
        // before handing off to the indexer (which would otherwise
        // reject with EmbedderDimMismatch later).
        for (i, e) in parsed.embeddings.iter().enumerate() {
            if e.values.len() != self.dim {
                return Err(EmbedError::DimensionMismatch {
                    expected: self.dim,
                    got: e.values.len(),
                });
            }
            let _ = i;
        }

        Ok(parsed.embeddings.into_iter().map(|e| e.values).collect())
    }
}

#[derive(Debug, Deserialize)]
struct BatchResponse {
    embeddings: Vec<EmbeddingItem>,
}

#[derive(Debug, Deserialize)]
struct EmbeddingItem {
    values: Vec<f32>,
}
