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

/// `reqwest` has no default request timeout — an unbounded `Client::new()`
/// here means a hung/slow call to Google's API blocks the calling request
/// forever, with no error and no way out short of the process being killed
/// externally. Hit this for real: a `POST /ingest/upload` against `lab`
/// never returned, and `/healthz` (a hardcoded 200, no locks) started
/// failing minutes later until kubelet force-restarted the pod. 30s is
/// generous for embedding a batch of up to `MAX_BATCH` texts — longer than
/// `remote_backend.rs`'s 10s precedent for a simpler external call, because
/// this one can legitimately carry more payload.
const EMBED_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

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
            client: reqwest::Client::builder()
                .timeout(EMBED_TIMEOUT)
                .build()
                .unwrap_or_default(),
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

    /// Override the request timeout ([`EMBED_TIMEOUT`] by default). Exposed
    /// so a test can prove a hung upstream call actually errors out instead
    /// of blocking forever, without a real 30s-long test.
    #[must_use]
    pub fn with_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .unwrap_or_default();
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

/// Whether an upstream failure is worth waiting out.
///
/// Matched on the message because that is what the transport hands back — the
/// status is already formatted into it by `embed_batch_once`, and threading a
/// typed status through `EmbedError` for this one decision would change a
/// public error shape every backend shares.
fn is_retryable(message: &str) -> bool {
    message.contains("HTTP 429")
        || message.contains("RESOURCE_EXHAUSTED")
        // The gateway is talking to a service that is up but unwell: worth a
        // wait, unlike a 4xx which will answer the same way for ever.
        || message.contains("HTTP 500")
        || message.contains("HTTP 502")
        || message.contains("HTTP 503")
        || message.contains("HTTP 504")
    // Deliberately NOT transport errors. `embed_batch_once` already bounds
    // each call with its own timeout, and retrying a timeout multiplies that
    // bound by the attempt count — `slow_upstream_response_times_out_...`
    // catches exactly that: a 200ms budget became 16s the moment send
    // failures were retried here.
}

/// How many times a rate-limited batch is retried before giving up.
///
/// Five attempts at 1s, 2s, 4s, 8s is about fifteen seconds of patience. The
/// thing being waited out is a per-minute quota, so seconds are the right
/// unit; a minute of retrying would turn one slow boot into a very slow one.
const RATE_LIMIT_ATTEMPTS: u32 = 5;

impl GeminiEmbedder {
    /// Embed one batch, waiting out a rate limit rather than failing on it.
    ///
    /// **429 is the error this will see most, and the least alarming one it
    /// can get**: it means the key works and is busy. Without this, one of
    /// them anywhere in a boot-time re-index aborts the whole start:
    /// `ESCUREL_REBUILD_INDEX_ON_BOOT` defaults to `always` in the container and
    /// re-embeds the corpus on every start, so a single batch coming back
    /// `RESOURCE_EXHAUSTED` fails the boot with `building indexer: embedder
    /// error` on a dependency that was merely busy (issue #449).
    ///
    /// It is worse than it sounds on this deployment: the same Gemini key
    /// serves the runner's harness, so a burst of drafting makes a gateway
    /// restart MORE likely to fail — the two compete for one quota, and the
    /// restart is when the gateway is least able to tolerate it.
    ///
    /// Only 429 and 5xx are retried. A 400, a 401 or a dimension mismatch
    /// will answer identically however long you wait, and retrying those
    /// would turn a clear configuration error into a slow one. Timeouts are
    /// not retried either: each call already carries its own deadline, and
    /// retrying multiplies it.
    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        let mut backoff = std::time::Duration::from_secs(1);
        for attempt in 1..=RATE_LIMIT_ATTEMPTS {
            match self.embed_batch_once(texts).await {
                Err(EmbedError::Backend(msg))
                    if attempt < RATE_LIMIT_ATTEMPTS && is_retryable(&msg) =>
                {
                    tracing::warn!(
                        attempt,
                        backoff_ms = backoff.as_millis() as u64,
                        error = %msg,
                        "gemini embedder is rate-limited or unavailable; waiting"
                    );
                    tokio::time::sleep(backoff).await;
                    backoff *= 2;
                }
                other => return other,
            }
        }
        unreachable!("the loop returns on the final attempt")
    }

    /// Embed a single ≤[`MAX_BATCH`] batch via one `batchEmbedContents` call.
    async fn embed_batch_once(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
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
