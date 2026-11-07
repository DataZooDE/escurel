//! OpenAI-compatible HTTP embedder (`POST {base}/embeddings`).
//!
//! Behind the `openai` Cargo feature. Serves any endpoint that speaks the
//! OpenAI embeddings wire shape — Alibaba Cloud Model Studio's compatible
//! mode (`text-embedding-v4`) is the one it was built for.
//!
//! **Identity.** [`Embedder::model_id`] is `"<provider>:<model>"`, e.g.
//! `alibaba:text-embedding-v4`. Vectors from two models are not comparable,
//! and the same model name at two providers is not promised to be the same
//! model; the index records this identity and re-embeds when it changes
//! rather than mixing spaces.
//!
//! **Secrets.** The key is sent only as a bearer header (never in the URL,
//! unlike Gemini's `?key=`), never over plain HTTP off the host, and is
//! redacted from `Debug`.

use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use crate::{EmbedError, Embedder};

/// Same reasoning as the Gemini embedder's bound: `reqwest` has no default
/// timeout, and a hung embed call blocks the request that needed it.
const EMBED_TIMEOUT: Duration = Duration::from_secs(30);
/// Largest response body read (a 10-text batch of 2048-dim floats is ~0.5 MB).
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
/// Attempts for a rate-limited or unavailable batch (1s, 2s, 4s, 8s waits).
const RATE_LIMIT_ATTEMPTS: u32 = 5;

/// Embedder for an OpenAI-compatible `/embeddings` endpoint.
#[derive(Clone)]
pub struct OpenAiEmbedder {
    provider: String,
    base_url: String,
    api_key: String,
    model: String,
    dim: usize,
    max_batch: usize,
    send_dimensions: bool,
    retry_backoff: Duration,
    client: reqwest::Client,
}

impl std::fmt::Debug for OpenAiEmbedder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiEmbedder")
            .field("provider", &self.provider)
            .field("base_url", &self.base_url)
            .field("api_key", &"<redacted>")
            .field("model", &self.model)
            .field("dim", &self.dim)
            .field("max_batch", &self.max_batch)
            .finish_non_exhaustive()
    }
}

impl OpenAiEmbedder {
    /// Build an embedder for `provider` (a label that becomes part of the
    /// model identity) at `base_url` (the `…/v1` base).
    ///
    /// # Errors
    /// [`EmbedError::Backend`] for an empty key or model, a base URL that is
    /// not a URL, carries credentials, or is plain HTTP off the host.
    pub fn new(
        provider: impl Into<String>,
        base_url: &str,
        api_key: impl Into<String>,
        model: impl Into<String>,
        dim: usize,
    ) -> Result<Self, EmbedError> {
        let base_url = base_url.trim().trim_end_matches('/').to_owned();
        // The URL is never quoted back: the inputs refused here (userinfo, a
        // query) are the ones that carry secrets, and the refusal is logged.
        let url = reqwest::Url::parse(&base_url).map_err(|e| {
            EmbedError::Backend(format!("the embedding base URL is not a URL: {e}"))
        })?;
        // Literal loopback addresses only: a NAME can resolve off the host.
        let loopback = matches!(url.host_str(), Some("127.0.0.1" | "[::1]"));
        if !(url.scheme() == "https" || (url.scheme() == "http" && loopback)) {
            return Err(EmbedError::Backend(
                "the embedding base URL must be https (plain http only to 127.0.0.1 or [::1])"
                    .to_owned(),
            ));
        }
        if !url.username().is_empty() || url.password().is_some() || url.query().is_some() {
            return Err(EmbedError::Backend(
                "the embedding base URL must not carry credentials or a query".to_owned(),
            ));
        }
        let api_key = api_key.into().trim().to_owned();
        if api_key.is_empty() {
            return Err(EmbedError::Backend("embedding API key is empty".to_owned()));
        }
        let model = model.into().trim().to_owned();
        if model.is_empty() {
            return Err(EmbedError::Backend(
                "no embedding model is configured".to_owned(),
            ));
        }
        Ok(Self {
            provider: provider.into(),
            base_url,
            api_key,
            model,
            dim,
            max_batch: 10,
            send_dimensions: true,
            retry_backoff: Duration::from_secs(1),
            client: client(EMBED_TIMEOUT)?,
        })
    }

    /// Texts per request (Model Studio's `text-embedding-v4` takes 10).
    #[must_use]
    pub fn with_max_batch(mut self, n: usize) -> Self {
        self.max_batch = n.max(1);
        self
    }

    /// Whether to send `dimensions` (models without Matryoshka support may
    /// reject it; the returned length is checked either way).
    #[must_use]
    pub fn with_send_dimensions(mut self, send: bool) -> Self {
        self.send_dimensions = send;
        self
    }

    /// First wait after a 429/5xx, doubled per retry.
    #[must_use]
    pub fn with_retry_backoff(mut self, backoff: Duration) -> Self {
        self.retry_backoff = backoff;
        self
    }

    /// Override the request timeout.
    ///
    /// # Errors
    /// [`EmbedError::Backend`] when the HTTP client cannot be built.
    pub fn with_timeout(mut self, timeout: Duration) -> Result<Self, EmbedError> {
        self.client = client(timeout)?;
        Ok(self)
    }

    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        let mut backoff = self.retry_backoff;
        for attempt in 1..=RATE_LIMIT_ATTEMPTS {
            match self.embed_batch_once(texts).await {
                Err(Retry::Yes(msg)) if attempt < RATE_LIMIT_ATTEMPTS => {
                    tracing::warn!(
                        attempt,
                        backoff_ms = backoff.as_millis() as u64,
                        error = %msg,
                        "openai-compatible embedder is rate-limited or unavailable; waiting"
                    );
                    tokio::time::sleep(backoff).await;
                    backoff = backoff.saturating_mul(2);
                }
                Err(Retry::Yes(msg) | Retry::No(msg)) => return Err(EmbedError::Backend(msg)),
                Err(Retry::Dim(e)) => return Err(e),
                Ok(v) => return Ok(v),
            }
        }
        unreachable!("the loop returns on the final attempt")
    }

    async fn embed_batch_once(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, Retry> {
        let mut body = json!({
            "model": self.model,
            "input": texts,
            "encoding_format": "float",
        });
        if self.send_dimensions {
            body["dimensions"] = json!(self.dim);
        }
        let resp = self
            .client
            .post(format!("{}/embeddings", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            // Not retried: a timeout multiplied by the attempt count is the
            // failure the Gemini embedder's test caught.
            .map_err(|e| Retry::No(format!("{} embeddings send: {e}", self.provider)))?;
        let status = resp.status();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| Retry::No(format!("{} embeddings read: {e}", self.provider)))?;
        if bytes.len() > MAX_RESPONSE_BYTES {
            return Err(Retry::No(format!(
                "{} embeddings response exceeded {MAX_RESPONSE_BYTES} bytes",
                self.provider
            )));
        }
        if !status.is_success() {
            let text = String::from_utf8_lossy(&bytes);
            let msg = format!(
                "{} embeddings HTTP {status}: {}",
                self.provider,
                // A provider that echoes the bearer must not publish it.
                text.replace(&self.api_key, "<redacted>")
                    .chars()
                    .take(500)
                    .collect::<String>()
            );
            let retry = status.as_u16() == 429 || matches!(status.as_u16(), 500 | 502 | 503 | 504);
            return Err(if retry {
                Retry::Yes(msg)
            } else {
                Retry::No(msg)
            });
        }
        let mut parsed: Response = serde_json::from_slice(&bytes)
            .map_err(|e| Retry::No(format!("{} embeddings JSON: {e}", self.provider)))?;
        if parsed.data.len() != texts.len() {
            return Err(Retry::No(format!(
                "{} embeddings returned {} vectors for {} texts",
                self.provider,
                parsed.data.len(),
                texts.len()
            )));
        }
        // Each vector must name its input exactly once: a duplicate or a
        // gap would attach vectors to the wrong texts with no error at all.
        parsed.data.sort_by_key(|d| d.index);
        if parsed.data.iter().enumerate().any(|(i, d)| d.index != i) {
            return Err(Retry::No(format!(
                "{} embeddings returned indices that are not 0..{}",
                self.provider,
                texts.len()
            )));
        }
        let mut out = Vec::with_capacity(parsed.data.len());
        for d in parsed.data {
            if d.embedding.len() != self.dim {
                return Err(Retry::Dim(EmbedError::DimensionMismatch {
                    expected: self.dim,
                    got: d.embedding.len(),
                }));
            }
            out.push(d.embedding);
        }
        Ok(out)
    }
}

/// The HTTP client: bounded, and never following a redirect — a redirect
/// would carry the bearer to wherever it points.
fn client(timeout: Duration) -> Result<reqwest::Client, EmbedError> {
    reqwest::Client::builder()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| EmbedError::Backend(format!("embedding HTTP client: {e}")))
}

enum Retry {
    Yes(String),
    No(String),
    Dim(EmbedError),
}

#[derive(Deserialize)]
struct Response {
    data: Vec<Item>,
}

#[derive(Deserialize)]
struct Item {
    index: usize,
    embedding: Vec<f32>,
}

#[async_trait]
impl Embedder for OpenAiEmbedder {
    fn dim(&self) -> usize {
        self.dim
    }

    fn model_id(&self) -> String {
        format!("{}:{}", self.provider, self.model)
    }

    async fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        let mut out = Vec::with_capacity(texts.len());
        for batch in texts.chunks(self.max_batch) {
            out.extend(self.embed_batch(batch).await?);
        }
        Ok(out)
    }
}
