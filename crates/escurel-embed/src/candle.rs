//! Local-model embedder backed by candle.
//!
//! Behind the `candle` Cargo feature so the heavy candle +
//! tokenizers + hf-hub deps don't drag into every build. CPU
//! inference is the default; CUDA / Metal feature flags arrive
//! after the CPU path is solid.
//!
//! The trait surface is identical to `ZeroEmbedder`,
//! `HashEmbedder`, and `GeminiEmbedder`; the indexer doesn't
//! know which one it has.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use candle_core::{Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config, DTYPE, HiddenAct};
use hf_hub::api::tokio::Api;
use tokenizers::{PaddingParams, PaddingStrategy, Tokenizer, TruncationParams};

use crate::{EmbedError, Embedder};

/// candle-backed embedder. Loads a BERT-family sentence-transformer
/// (e.g. `sentence-transformers/all-MiniLM-L6-v2`, EmbeddingGemma
/// once `gemma3` lands in candle-transformers) and runs CPU
/// inference.
#[derive(Clone)]
pub struct CandleEmbedder {
    inner: Arc<Inner>,
}

struct Inner {
    model: BertModel,
    tokenizer: Tokenizer,
    device: Device,
    declared_dim: usize,
    model_id: String,
}

impl std::fmt::Debug for CandleEmbedder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CandleEmbedder")
            .field("declared_dim", &self.inner.declared_dim)
            .field("device", &format_args!("{:?}", self.inner.device))
            .finish_non_exhaustive()
    }
}

impl CandleEmbedder {
    /// Build from a HuggingFace Hub repo id. Downloads
    /// `config.json` + `tokenizer.json` + `model.safetensors`
    /// into the local `~/.cache/huggingface/hub/` cache on first
    /// call; subsequent calls hit the cache.
    ///
    /// `expected_dim` is rejected at load time if the loaded
    /// model's hidden size disagrees. Substrate production loads
    /// from a baked path via [`Self::from_local`] instead so no
    /// network egress happens at runtime.
    pub async fn from_hf_hub(repo_id: &str, expected_dim: usize) -> Result<Self, EmbedError> {
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

        Self::from_local(
            &config_path,
            &tokenizer_path,
            &weights_path,
            expected_dim,
            repo_id,
        )
    }

    /// Build from three local files. Substrate production calls
    /// this against the golden-image bake at
    /// `/opt/escurel/models/...` so no network egress happens at
    /// runtime.
    pub fn from_local(
        config_path: &Path,
        tokenizer_path: &Path,
        weights_path: &Path,
        expected_dim: usize,
        model_id: &str,
    ) -> Result<Self, EmbedError> {
        let device = Device::Cpu;

        let config_json = std::fs::read_to_string(config_path)
            .map_err(|e| EmbedError::Backend(format!("read config.json: {e}")))?;
        let mut config: Config = serde_json::from_str(&config_json)
            .map_err(|e| EmbedError::Backend(format!("parse config.json: {e}")))?;
        // sentence-transformers configs sometimes pin
        // `hidden_act = "gelu_pytorch_tanh"` which candle's bert
        // doesn't accept; coerce to a supported value at load time.
        config.hidden_act = HiddenAct::Gelu;

        let loaded_dim = config.hidden_size;
        if loaded_dim != expected_dim {
            return Err(EmbedError::DimensionMismatch {
                expected: expected_dim,
                got: loaded_dim,
            });
        }

        // Load safetensors via the safe loader (no mmap).
        let tensors = candle_core::safetensors::load(weights_path, &device)
            .map_err(|e| EmbedError::Backend(format!("load safetensors: {e}")))?;
        let vb = VarBuilder::from_tensors(tensors, DTYPE, &device);

        let model = BertModel::load(vb, &config)
            .map_err(|e| EmbedError::Backend(format!("build BertModel: {e}")))?;

        let mut tokenizer = Tokenizer::from_file(tokenizer_path)
            .map_err(|e| EmbedError::Backend(format!("load tokenizer.json: {e}")))?;
        tokenizer.with_padding(Some(PaddingParams {
            strategy: PaddingStrategy::BatchLongest,
            ..Default::default()
        }));
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: 512,
                ..Default::default()
            }))
            .map_err(|e| EmbedError::Backend(format!("tokenizer truncation: {e}")))?;

        Ok(Self {
            inner: Arc::new(Inner {
                model,
                tokenizer,
                device,
                declared_dim: loaded_dim,
                model_id: model_id.to_owned(),
            }),
        })
    }
}

#[async_trait]
impl Embedder for CandleEmbedder {
    fn dim(&self) -> usize {
        self.inner.declared_dim
    }

    fn model_id(&self) -> String {
        self.inner.model_id.clone()
    }

    async fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        // candle's forward is sync + CPU-bound; offload off the
        // async runtime so we don't stall other tasks during the
        // inference window. spawn_blocking needs `'static` data,
        // hence the owned-string copy + Arc<Inner> clone.
        let owned: Vec<String> = texts.iter().map(|s| (*s).to_owned()).collect();
        let inner = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
            inner.embed_blocking(&refs)
        })
        .await
        .map_err(|e| EmbedError::Backend(format!("spawn_blocking join: {e}")))?
    }
}

impl Inner {
    fn embed_blocking(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        let encodings = self
            .tokenizer
            .encode_batch(texts.to_vec(), true)
            .map_err(|e| EmbedError::Backend(format!("tokenize: {e}")))?;

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
        let token_type_ids = input_ids
            .zeros_like()
            .map_err(|e| EmbedError::Backend(format!("token_type_ids: {e}")))?;

        let hidden = self
            .model
            .forward(&input_ids, &token_type_ids, Some(&attention_mask))
            .map_err(|e| EmbedError::Backend(format!("forward: {e}")))?;

        let pooled = mean_pool(&hidden, &attention_mask)
            .map_err(|e| EmbedError::Backend(format!("mean_pool: {e}")))?;
        let normalised =
            l2_normalize(&pooled).map_err(|e| EmbedError::Backend(format!("l2_normalize: {e}")))?;

        let vectors: Vec<Vec<f32>> = normalised
            .to_vec2()
            .map_err(|e| EmbedError::Backend(format!("to_vec2: {e}")))?;
        for v in &vectors {
            if v.len() != self.declared_dim {
                return Err(EmbedError::DimensionMismatch {
                    expected: self.declared_dim,
                    got: v.len(),
                });
            }
        }
        Ok(vectors)
    }
}

fn mean_pool(hidden: &Tensor, mask: &Tensor) -> candle_core::Result<Tensor> {
    // hidden: [batch, seq, hidden]; mask: [batch, seq] → [batch, seq, 1].
    let mask = mask.to_dtype(hidden.dtype())?.unsqueeze(2)?;
    let summed = hidden.broadcast_mul(&mask)?.sum(1)?;
    let counts = mask.sum(1)?.clamp(1.0_f32, f32::MAX)?;
    summed.broadcast_div(&counts)
}

fn l2_normalize(t: &Tensor) -> candle_core::Result<Tensor> {
    let norm = t.sqr()?.sum_keepdim(1)?.sqrt()?;
    t.broadcast_div(&norm)
}
