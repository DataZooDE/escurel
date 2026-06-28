//! Integration tests for `CrossEncoderReranker` — real candle, real
//! tokenizer, real XLM-RoBERTa cross-encoder. Gated on the `rerank`
//! feature.
//!
//! Run: `cargo test -p escurel-embed --features rerank --test cross_encoder`
//!
//! The test downloads `BAAI/bge-reranker-base` from HuggingFace Hub on
//! first run (~1.1 GB) and caches it under `~/.cache/huggingface/hub/`.
//! Subsequent runs hit the cache. No mocks: this is the real model
//! loader + real tokenizer + real CPU forward pass over `(query,
//! passage)` pairs.
//!
//! Why bge-reranker-base and not bge-reranker-v2-m3: it is the smallest
//! member of the same `XLMRobertaForSequenceClassification` family that
//! production uses (~278 M params vs ~568 M), so it exercises every
//! code path at a fraction of the download/compute. CI builds default
//! features only, so this never runs in CI — exactly like the
//! `candle_embedder` test.

#![cfg(feature = "rerank")]

use std::sync::OnceLock;

use escurel_embed::{Candidate, CrossEncoderReranker, Reranker};
use tokio::sync::Mutex;

const RERANKER_REPO: &str = "BAAI/bge-reranker-base";

/// Serialise hf-hub downloads across the parallel test run (its
/// per-blob file lock fails on a race). After the first download the
/// cache is warm and subsequent loads are effectively free.
fn download_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

async fn load() -> CrossEncoderReranker {
    let _guard = download_lock().lock().await;
    CrossEncoderReranker::from_hf_hub(RERANKER_REPO)
        .await
        .expect("load BAAI/bge-reranker-base from HF Hub (downloads on first run)")
}

fn cand(id: &str, text: &str) -> Candidate {
    Candidate {
        id: id.to_owned(),
        text: text.to_owned(),
    }
}

#[tokio::test]
async fn reranks_relevant_passage_above_unrelated_one() {
    let reranker = load().await;

    // The query is about pandas' diet. The clearly-relevant passage
    // must outscore the unrelated one regardless of input order.
    let query = "What do giant pandas eat?";
    let candidates = [
        cand(
            "unrelated",
            "The Eiffel Tower is a wrought-iron lattice tower in Paris.",
        ),
        cand(
            "relevant",
            "Giant pandas feed almost exclusively on bamboo, eating up to 12 kg a day.",
        ),
    ];

    let ranked = reranker.rerank(query, &candidates).await.expect("rerank");

    // Same set, reordered.
    assert_eq!(ranked.len(), 2);
    // The bamboo passage is first.
    assert_eq!(
        ranked[0].id, "relevant",
        "cross-encoder must rank the on-topic passage first; got {ranked:?}"
    );
    assert!(
        ranked[0].score > ranked[1].score,
        "relevant score must exceed unrelated; got {ranked:?}"
    );
}

#[tokio::test]
async fn preserves_candidate_set_and_returns_descending_scores() {
    let reranker = load().await;
    let candidates: Vec<Candidate> = (0..5)
        .map(|i| {
            cand(
                &format!("c{i}"),
                &format!("passage number {i} about logistics"),
            )
        })
        .collect();

    let ranked = reranker
        .rerank("logistics planning", &candidates)
        .await
        .expect("rerank");

    // Exactly the input ids, none added or dropped.
    let mut got: Vec<&str> = ranked.iter().map(|r| r.id.as_str()).collect();
    got.sort_unstable();
    assert_eq!(got, ["c0", "c1", "c2", "c3", "c4"]);
    // Descending relevance order.
    assert!(ranked.windows(2).all(|w| w[0].score >= w[1].score));
}

#[tokio::test]
async fn empty_candidates_is_empty() {
    let reranker = load().await;
    let ranked = reranker.rerank("anything", &[]).await.expect("rerank");
    assert!(ranked.is_empty());
}
