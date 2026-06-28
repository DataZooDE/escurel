//! Retrieval-stage configuration (the second-stage rerank knobs).
//!
//! The first stage (vss + fts + RRF fusion) always runs. When rerank
//! is enabled, the search path fetches a larger fused candidate pool
//! ([`RetrievalConfig::rerank_candidates`]), hands it to the injected
//! [`escurel_embed::Reranker`] *after* the per-lane ACL filter, then
//! truncates to the caller's `k`. Disabled is the default and is
//! byte-for-byte today's behaviour (the stage is skipped entirely).

/// Knobs for the post-fusion rerank stage. Built by the server from the
/// `[retrieval]` config section and handed to the [`crate::Indexer`]
/// alongside the concrete reranker via [`crate::Indexer::with_reranker`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetrievalConfig {
    rerank_enabled: bool,
    rerank_candidates: usize,
}

impl Default for RetrievalConfig {
    fn default() -> Self {
        Self::disabled()
    }
}

impl RetrievalConfig {
    /// Rerank off — first-stage fused order is returned as-is.
    pub fn disabled() -> Self {
        Self {
            rerank_enabled: false,
            rerank_candidates: 0,
        }
    }

    /// Rerank on, scoring the top `rerank_candidates` fused hits. The
    /// count is clamped to at least 1.
    pub fn enabled(rerank_candidates: usize) -> Self {
        Self {
            rerank_enabled: true,
            rerank_candidates: rerank_candidates.max(1),
        }
    }

    /// Whether the rerank stage runs.
    pub fn rerank_enabled(&self) -> bool {
        self.rerank_enabled
    }

    /// How many fused candidates feed the reranker.
    pub fn rerank_candidates(&self) -> usize {
        self.rerank_candidates
    }
}

use escurel_embed::Candidate;

use crate::{Indexer, IndexerError, SearchHit};

impl Indexer {
    /// Re-score `hits` against `query` with the injected cross-encoder
    /// and return them in the reranker's descending-relevance order.
    ///
    /// This runs **after** the per-lane fail-closed ACL filter and RRF
    /// fusion (INV-ACL-FUSION): it only ever **reorders** its input, so
    /// the returned set is exactly the input set — it can never surface
    /// a row the caller was not already entitled to see. The caller
    /// truncates to its `k` afterwards.
    ///
    /// A no-op (returns `hits` untouched) when rerank is disabled or
    /// there are fewer than two candidates to reorder. If a buggy
    /// reranker drops or duplicates ids, the untouched hits are appended
    /// in their original order so the set is preserved regardless.
    pub async fn rerank_hits(
        &self,
        query: &str,
        hits: Vec<SearchHit>,
    ) -> Result<Vec<SearchHit>, IndexerError> {
        if !self.retrieval.rerank_enabled() || hits.len() < 2 {
            return Ok(hits);
        }

        // Positional ids keep the id-space unique and let us map the
        // reranker's output back to the exact source hit regardless of
        // page_id/anchor collisions.
        let candidates: Vec<Candidate> = hits
            .iter()
            .enumerate()
            .map(|(i, h)| Candidate {
                id: i.to_string(),
                text: rerank_passage(h),
            })
            .collect();

        let ranked = self.reranker.rerank(query, &candidates).await?;

        let mut out = Vec::with_capacity(hits.len());
        let mut placed = vec![false; hits.len()];
        for r in &ranked {
            let Ok(idx) = r.id.parse::<usize>() else {
                continue;
            };
            match placed.get_mut(idx) {
                Some(slot) if !*slot => {
                    *slot = true;
                    let mut h = hits[idx].clone();
                    h.score = f64::from(r.score);
                    out.push(h);
                }
                _ => {}
            }
        }
        // Set-preservation safety net: any hit the reranker failed to
        // return is appended in its original position order.
        for (i, was_placed) in placed.iter().enumerate() {
            if !*was_placed {
                out.push(hits[i].clone());
            }
        }
        Ok(out)
    }
}

/// The passage text a hit contributes to the cross-encoder. The block
/// snippet (the lead of the block body) is what the first stage already
/// hydrated; it bounds the per-pair token cost without a refetch.
fn rerank_passage(h: &SearchHit) -> String {
    h.snippet.clone()
}
