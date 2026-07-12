//! Variant B of Contextual Retrieval (#216): an LLM writes a one-sentence
//! situating context per chunk at ingest, instead of the deterministic
//! structural `[title › heading › p.N]` prefix.
//!
//! This is a **network path**, at odds with escurel's deterministic/air-gap
//! default, so the whole module is behind the `contextualize-llm` Cargo
//! feature and off by default. When the feature is disabled — or the endpoint
//! is unset, or any call fails — ingest degrades to [`ContextualizeMode::
//! Structural`](super::document::ContextualizeMode::Structural), so builds and
//! rebuilds stay deterministic and offline.
//!
//! The wire shape mirrors the `gemini.rs` embed client's reqwest pattern: a
//! Google `:generateContent`-style POST, or any endpoint that accepts
//! `{contents:[{parts:[{text}]}]}` and returns
//! `{candidates:[{content:{parts:[{text}]}}]}`.

use crate::backend::document::structural_context_prefix;

/// A per-chunk LLM situating-context generator.
#[derive(Clone)]
pub struct LlmContextualizer {
    endpoint: String,
    api_key: String,
    client: reqwest::Client,
}

impl std::fmt::Debug for LlmContextualizer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmContextualizer")
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}

impl LlmContextualizer {
    /// Build a contextualizer against `endpoint` (a full generateContent URL)
    /// authenticated with `api_key` (sent as `?key=` and `x-goog-api-key`).
    #[must_use]
    pub fn new(endpoint: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            api_key: api_key.into(),
            client: reqwest::Client::new(),
        }
    }

    /// Ask the model to situate `chunk` within its document in one short
    /// sentence. Returns the bracketed context (`[<sentence>]`) or `None` on
    /// any error — the caller then falls back to the structural prefix.
    pub async fn context_prefix(
        &self,
        title: Option<&str>,
        headings: &[String],
        page: Option<u32>,
        chunk: &str,
    ) -> Option<String> {
        let structural = structural_context_prefix(title, headings, page).unwrap_or_default();
        let prompt = format!(
            "You situate a text chunk within its document for retrieval. Given \
             the document context `{structural}` and the chunk below, write ONE \
             short sentence (max 25 words) naming what entity/section/time the \
             chunk is about. Reply with only that sentence.\n\nCHUNK:\n{chunk}"
        );
        let body = serde_json::json!({
            "contents": [{ "parts": [{ "text": prompt }] }],
            "generationConfig": { "temperature": 0.0, "maxOutputTokens": 64 }
        });
        let resp = self
            .client
            .post(&self.endpoint)
            .query(&[("key", self.api_key.as_str())])
            .header("x-goog-api-key", &self.api_key)
            .json(&body)
            .send()
            .await
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let v: serde_json::Value = resp.json().await.ok()?;
        let text = v["candidates"][0]["content"]["parts"][0]["text"]
            .as_str()?
            .trim();
        if text.is_empty() {
            None
        } else {
            Some(format!("[{text}]"))
        }
    }
}
