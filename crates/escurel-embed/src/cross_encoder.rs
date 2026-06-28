//! Local cross-encoder reranker backed by candle.
//!
//! Behind the `rerank` Cargo feature (same heavy candle + tokenizers +
//! hf-hub dep set as [`crate::CandleEmbedder`]) so it doesn't drag into
//! every build. CPU inference is the default; GPU feature flags arrive
//! after the CPU path is solid, mirroring the embedder.
//!
//! The model is an **XLM-RoBERTa sequence classifier with a single
//! output label** — the architecture of `BAAI/bge-reranker-v2-m3` and
//! the rest of the bge-reranker family. Unlike the bi-encoder embedder
//! (which encodes query and passage separately), a cross-encoder scores
//! the `(query, passage)` pair **jointly**: both are concatenated into
//! one sequence, and the classifier head over the leading `<s>` token
//! emits one relevance logit. Higher logit = more relevant.
//!
//! It implements [`Reranker`], so the retrieval path consumes it through
//! `Arc<dyn Reranker>` exactly like [`crate::NoopReranker`].

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use candle_core::{Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::xlm_roberta::{Config, XLMRobertaForSequenceClassification};
use hf_hub::api::tokio::Api;
use tokenizers::{PaddingParams, PaddingStrategy, Tokenizer, TruncationParams};

use crate::{Candidate, EmbedError, Ranked, Reranker};

/// Max combined `(query, passage)` length fed to the cross-encoder.
/// bge rerankers are trained at 512; longer pairs are truncated.
const MAX_PAIR_TOKENS: usize = 512;

/// candle-backed cross-encoder reranker. Loads an
/// `XLMRobertaForSequenceClassification` (num_labels = 1) and runs CPU
/// inference over `(query, passage)` pairs.
#[derive(Clone)]
pub struct CrossEncoderReranker {
    inner: Arc<Inner>,
}

struct Inner {
    model: XLMRobertaForSequenceClassification,
    tokenizer: Tokenizer,
    device: Device,
    model_id: String,
}

impl std::fmt::Debug for CrossEncoderReranker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CrossEncoderReranker")
            .field("model_id", &self.inner.model_id)
            .field("device", &format_args!("{:?}", self.inner.device))
            .finish_non_exhaustive()
    }
}

impl CrossEncoderReranker {
    /// Build from a HuggingFace Hub repo id (e.g.
    /// `BAAI/bge-reranker-v2-m3`). Downloads `config.json` +
    /// `tokenizer.json` + `model.safetensors` into the local
    /// `~/.cache/huggingface/hub/` cache on first call; subsequent calls
    /// hit the cache. Substrate production loads from a baked path via
    /// [`Self::from_local`] so no network egress happens at runtime.
    pub async fn from_hf_hub(repo_id: &str) -> Result<Self, EmbedError> {
        let api = Api::new().map_err(|e| EmbedError::Backend(format!("hf-hub Api init: {e}")))?;
        let repo = api.model(repo_id.to_owned());

        let config_path = repo
            .get("config.json")
            .await
            .map_err(|e| EmbedError::Backend(format!("fetch config.json: {e}")))?;
        let tokenizer_path = repo
            .get("tokenizer.json")
            .await
            .map_err(|e| EmbedError::Backend(format!("fetch tokenizer.json: {e}")))?;
        let weights_path = repo
            .get("model.safetensors")
            .await
            .map_err(|e| EmbedError::Backend(format!("fetch model.safetensors: {e}")))?;

        Self::from_local(&config_path, &tokenizer_path, &weights_path, repo_id)
    }

    /// Build from three local files. Substrate production calls this
    /// against the golden-image bake so no network egress happens at
    /// runtime.
    pub fn from_local(
        config_path: &Path,
        tokenizer_path: &Path,
        weights_path: &Path,
        model_id: &str,
    ) -> Result<Self, EmbedError> {
        let device = Device::Cpu;

        let config_json = std::fs::read_to_string(config_path)
            .map_err(|e| EmbedError::Backend(format!("read config.json: {e}")))?;
        let config: Config = serde_json::from_str(&config_json)
            .map_err(|e| EmbedError::Backend(format!("parse config.json: {e}")))?;
        let pad_id = config.pad_token_id;

        let tensors = candle_core::safetensors::load(weights_path, &device)
            .map_err(|e| EmbedError::Backend(format!("load safetensors: {e}")))?;
        let vb = VarBuilder::from_tensors(tensors, candle_core::DType::F32, &device);

        // num_labels = 1: the reranker emits a single relevance logit.
        let model = XLMRobertaForSequenceClassification::new(1, &config, vb)
            .map_err(|e| EmbedError::Backend(format!("build reranker model: {e}")))?;

        let mut tokenizer = Tokenizer::from_file(tokenizer_path)
            .map_err(|e| EmbedError::Backend(format!("load tokenizer.json: {e}")))?;
        // Right-pad to the batch's longest pair with the model's pad
        // token; attention masking neutralises the padding.
        tokenizer.with_padding(Some(PaddingParams {
            strategy: PaddingStrategy::BatchLongest,
            pad_id,
            ..Default::default()
        }));
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: MAX_PAIR_TOKENS,
                ..Default::default()
            }))
            .map_err(|e| EmbedError::Backend(format!("tokenizer truncation: {e}")))?;

        Ok(Self {
            inner: Arc::new(Inner {
                model,
                tokenizer,
                device,
                model_id: model_id.to_owned(),
            }),
        })
    }

    /// The model identity this reranker was loaded with.
    pub fn model_id(&self) -> &str {
        &self.inner.model_id
    }
}

#[async_trait]
impl Reranker for CrossEncoderReranker {
    async fn rerank(
        &self,
        query: &str,
        candidates: &[Candidate],
    ) -> Result<Vec<Ranked>, EmbedError> {
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        // candle forward is sync + CPU-bound; offload off the async
        // runtime so other tasks aren't stalled during inference.
        // spawn_blocking needs `'static`, hence the owned copies.
        let query = query.to_owned();
        let candidates = candidates.to_vec();
        let inner = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || inner.rerank_blocking(&query, &candidates))
            .await
            .map_err(|e| EmbedError::Backend(format!("spawn_blocking join: {e}")))?
    }
}

impl Inner {
    fn rerank_blocking(
        &self,
        query: &str,
        candidates: &[Candidate],
    ) -> Result<Vec<Ranked>, EmbedError> {
        // Each candidate becomes a (query, passage) pair; the tokenizer
        // joins them with the special tokens (`<s> query </s></s> passage </s>`).
        let pairs: Vec<(String, String)> = candidates
            .iter()
            .map(|c| (query.to_owned(), c.text.clone()))
            .collect();
        let encodings = self
            .tokenizer
            .encode_batch(pairs, true)
            .map_err(|e| EmbedError::Backend(format!("tokenize pairs: {e}")))?;

        let max_len = encodings
            .iter()
            .map(tokenizers::Encoding::len)
            .max()
            .unwrap_or(0);
        let batch = encodings.len();

        let mut ids: Vec<u32> = Vec::with_capacity(batch * max_len);
        let mut mask: Vec<u32> = Vec::with_capacity(batch * max_len);
        for enc in &encodings {
            ids.extend_from_slice(enc.get_ids());
            mask.extend_from_slice(enc.get_attention_mask());
        }
        let input_ids = Tensor::from_vec(ids, (batch, max_len), &self.device)
            .map_err(|e| EmbedError::Backend(format!("input_ids tensor: {e}")))?;
        let attention_mask = Tensor::from_vec(mask, (batch, max_len), &self.device)
            .map_err(|e| EmbedError::Backend(format!("attention_mask tensor: {e}")))?;
        // XLM-RoBERTa uses a single token-type segment (type_vocab_size = 1).
        let token_type_ids = input_ids
            .zeros_like()
            .map_err(|e| EmbedError::Backend(format!("token_type_ids: {e}")))?;

        // logits: [batch, 1] — one relevance score per pair.
        let logits = self
            .model
            .forward(&input_ids, &attention_mask, &token_type_ids)
            .map_err(|e| EmbedError::Backend(format!("forward: {e}")))?;
        let scores: Vec<f32> = logits
            .squeeze(1)
            .map_err(|e| EmbedError::Backend(format!("squeeze logits: {e}")))?
            .to_vec1()
            .map_err(|e| EmbedError::Backend(format!("to_vec1: {e}")))?;

        let mut ranked: Vec<Ranked> = candidates
            .iter()
            .zip(scores)
            .map(|(c, score)| Ranked {
                id: c.id.clone(),
                score,
            })
            .collect();
        // Descending relevance; stable on ties keeps first-stage order.
        ranked.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Ok(ranked)
    }
}
