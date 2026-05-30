//! Reranker seam.
//!
//! A reranker re-scores a small candidate set against the query with
//! a cross-encoder — the second-stage refinement after first-stage
//! retrieval (vector + FTS) has narrowed the field. Spec decision 5
//! (`docs/spec/README.md §Locked decisions`) names the bundled
//! EmbeddingGemma CE head as the default reranker, with
//! `bge-reranker-large` as an opt-in `--features rerank` upgrade.
//!
//! This module establishes the trait seam so the retrieval path can
//! depend on `Arc<dyn Reranker>` ahead of the real model. [`NoopReranker`]
//! is the identity default — it preserves first-stage order, so wiring
//! it in is a behaviour-preserving no-op until a real head lands.

use async_trait::async_trait;

use crate::EmbedError;

/// A candidate to rerank: an opaque id (the caller's handle, e.g. a
/// `block_id`) plus the text the cross-encoder scores against the
/// query.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub id: String,
    pub text: String,
}

/// A reranked candidate: its id and the new relevance score (higher =
/// more relevant). Results are returned in descending-score order.
#[derive(Debug, Clone, PartialEq)]
pub struct Ranked {
    pub id: String,
    pub score: f32,
}

/// A cross-encoder reranker.
#[async_trait]
pub trait Reranker: Send + Sync + 'static {
    /// Rerank `candidates` against `query`, returning them in
    /// descending relevance order with the cross-encoder's scores.
    /// The returned `Vec` contains exactly the input candidates,
    /// reordered.
    async fn rerank(
        &self,
        query: &str,
        candidates: &[Candidate],
    ) -> Result<Vec<Ranked>, EmbedError>;
}

/// Identity reranker: preserves the first-stage order and assigns
/// descending placeholder scores. The default until the EmbeddingGemma
/// CE head (spec decision 5) is wired in — wiring it into the search
/// path changes nothing observable.
#[derive(Debug, Default, Clone)]
pub struct NoopReranker;

#[async_trait]
impl Reranker for NoopReranker {
    async fn rerank(
        &self,
        _query: &str,
        candidates: &[Candidate],
    ) -> Result<Vec<Ranked>, EmbedError> {
        let n = candidates.len().max(1) as f32;
        Ok(candidates
            .iter()
            .enumerate()
            .map(|(i, c)| Ranked {
                id: c.id.clone(),
                // Strictly descending so the order is preserved by any
                // stable score-sort downstream.
                score: 1.0 - (i as f32) / n,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(id: &str, text: &str) -> Candidate {
        Candidate {
            id: id.to_owned(),
            text: text.to_owned(),
        }
    }

    #[tokio::test]
    async fn noop_preserves_input_order_with_descending_scores() {
        let candidates = [cand("a", "alpha"), cand("b", "beta"), cand("c", "gamma")];
        let ranked = NoopReranker.rerank("q", &candidates).await.unwrap();
        let ids: Vec<&str> = ranked.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["a", "b", "c"]);
        // Scores strictly descend, so a stable sort is a no-op.
        assert!(ranked.windows(2).all(|w| w[0].score > w[1].score));
    }

    #[tokio::test]
    async fn noop_on_empty_input_is_empty() {
        let ranked = NoopReranker.rerank("q", &[]).await.unwrap();
        assert!(ranked.is_empty());
    }
}
