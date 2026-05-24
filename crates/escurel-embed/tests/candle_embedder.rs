//! Integration tests for `CandleEmbedder` — real candle, real
//! tokenizer, real model. Gated on the `candle` feature.
//!
//! Run: `cargo test -p escurel-embed --features candle --test candle_embedder`
//!
//! The test downloads `sentence-transformers/all-MiniLM-L6-v2`
//! from HuggingFace Hub on first run (~22 MB) and caches it
//! under `~/.cache/huggingface/hub/`. Subsequent runs hit the
//! cache. No mocks: this is the real model loader + real
//! tokenizer + real CPU forward pass.
//!
//! Why MiniLM-L6 and not EmbeddingGemma: it's the smallest
//! well-known sentence embedder (22 MB safetensors, 384 dim,
//! BERT family); EmbeddingGemma is ~600 MB. The trait
//! contract is identical so the test exercises every code path
//! a 768-dim EmbeddingGemma run would (loading, tokenizing,
//! forward, mean-pool, L2-normalise). Production substrate
//! deployments bake EmbeddingGemma at
//! `/opt/escurel/models/embeddinggemma-300m/` (substrate.md §6)
//! and configure CandleEmbedder against that path.

#![cfg(feature = "candle")]

use std::sync::OnceLock;

use escurel_embed::{CandleEmbedder, Embedder};
use tokio::sync::Mutex;

const MINILM_REPO: &str = "sentence-transformers/all-MiniLM-L6-v2";
const MINILM_DIM: usize = 384;

/// Cargo runs tests in parallel; hf-hub's per-blob file lock
/// fails if two tests race to populate the same cache entry. We
/// serialise every test's load attempts through this Mutex so
/// only one hits the lock at a time. After the first download
/// the local cache is populated and subsequent loads are
/// effectively free.
fn download_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

async fn load_minilm(expected_dim: usize) -> Result<CandleEmbedder, escurel_embed::EmbedError> {
    let _guard = download_lock().lock().await;
    CandleEmbedder::from_hf_hub(MINILM_REPO, expected_dim).await
}

#[tokio::test]
async fn loads_minilm_from_hf_hub_and_embeds_one_text() {
    let e = load_minilm(MINILM_DIM)
        .await
        .expect("load all-MiniLM-L6-v2 from HF Hub (downloads on first run)");

    assert_eq!(e.dim(), MINILM_DIM);

    let out = e.embed(&["hello world"]).await.expect("embed");
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].len(), MINILM_DIM);

    // Non-degenerate: not all zeros (would mean the forward pass
    // produced nothing meaningful).
    let norm: f32 = out[0].iter().map(|x| x * x).sum::<f32>().sqrt();
    assert!(
        norm > 0.5,
        "embedded vector must have non-trivial L2 norm; got {norm}",
    );
}

#[tokio::test]
async fn batch_embed_returns_one_vector_per_input() {
    let e = load_minilm(MINILM_DIM).await.expect("load");
    let texts = ["first sentence", "second sentence", "third sentence"];
    let out = e.embed(&texts).await.expect("batch embed");
    assert_eq!(out.len(), 3);
    for v in &out {
        assert_eq!(v.len(), MINILM_DIM);
    }
}

#[tokio::test]
async fn similar_texts_embed_closer_than_unrelated() {
    // Semantic check: with a real sentence transformer, two
    // paraphrases must have higher cosine similarity than two
    // unrelated sentences. The MiniLM model is small but its
    // canonical evaluation is exactly this property.
    let e = load_minilm(MINILM_DIM).await.expect("load");
    let vecs = e
        .embed(&[
            "The cat sat on the mat.",
            "A feline rested on the rug.",
            "Compiler warnings are deprecated.",
        ])
        .await
        .expect("embed three");

    let sim_paraphrase = cosine(&vecs[0], &vecs[1]);
    let sim_unrelated = cosine(&vecs[0], &vecs[2]);
    assert!(
        sim_paraphrase > sim_unrelated,
        "paraphrase similarity ({sim_paraphrase:.3}) must exceed \
         unrelated similarity ({sim_unrelated:.3}) — otherwise the \
         model isn't producing meaningful embeddings",
    );
}

#[tokio::test]
async fn empty_batch_returns_empty_vec() {
    let e = load_minilm(MINILM_DIM).await.expect("load");
    let out = e.embed(&[]).await.expect("empty");
    assert!(out.is_empty());
}

#[tokio::test]
async fn dim_mismatch_at_construction_errors() {
    // The caller declared 768 but the loaded model is 384.
    let err = load_minilm(768)
        .await
        .expect_err("dim mismatch must error at load");
    let msg = format!("{err}");
    assert!(
        msg.contains("384") || msg.contains("768"),
        "error must mention the mismatch: {msg}",
    );
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}
