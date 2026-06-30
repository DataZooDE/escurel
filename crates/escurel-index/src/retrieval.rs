//! Retrieval-stage configuration (the second-stage rerank knobs).
//!
//! The first stage (vss + fts + RRF fusion) always runs. When rerank
//! is enabled, the search path fetches a larger fused candidate pool
//! ([`RetrievalConfig::rerank_candidates`]), hands it to the injected
//! [`escurel_embed::Reranker`] *after* the per-lane ACL filter, then
//! truncates to the caller's `k`. Disabled is the default and is
//! byte-for-byte today's behaviour (the stage is skipped entirely).

/// The full vector dimension stored in `blocks.dense_vec`. A Matryoshka
/// coarse pass truncates to a prefix of this. Mirrors
/// [`crate::indexer::BLOCKS_DENSE_VEC_DIM`].
const FULL_DIM: usize = crate::indexer::BLOCKS_DENSE_VEC_DIM;

/// Knobs for the retrieval pipeline: the post-fusion rerank stage and the
/// Matryoshka two-pass vector search. Built by the server from the
/// `[retrieval]` config section and handed to the [`crate::Indexer`]
/// (alongside a reranker via [`crate::Indexer::with_reranker`], or on its
/// own via [`crate::Indexer::with_retrieval`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetrievalConfig {
    rerank_enabled: bool,
    rerank_candidates: usize,
    two_pass: bool,
    coarse_dim: usize,
    coarse_candidates: usize,
}

impl Default for RetrievalConfig {
    fn default() -> Self {
        Self::disabled()
    }
}

impl RetrievalConfig {
    /// Rerank off, single-pass vector search — byte-for-byte today's behaviour.
    pub fn disabled() -> Self {
        Self {
            rerank_enabled: false,
            rerank_candidates: 0,
            two_pass: false,
            coarse_dim: 128,
            coarse_candidates: 500,
        }
    }

    /// Rerank on, scoring the top `rerank_candidates` fused hits. The
    /// count is clamped to at least 1. (Two-pass stays off; compose with
    /// [`Self::with_two_pass`].)
    pub fn enabled(rerank_candidates: usize) -> Self {
        Self {
            rerank_enabled: true,
            rerank_candidates: rerank_candidates.max(1),
            ..Self::disabled()
        }
    }

    /// Enable the Matryoshka two-pass vector search: a coarse ANN shortlist
    /// of `coarse_candidates` blocks scored on the truncated `coarse_dim`
    /// prefix of the stored vector, then exact full-dimension rescoring of
    /// that shortlist (issue #218). `coarse_dim` is clamped to
    /// `[1, FULL_DIM]` and `coarse_candidates` to at least 1. Builder-style,
    /// so it composes with [`Self::enabled`].
    #[must_use]
    pub fn with_two_pass(mut self, coarse_dim: usize, coarse_candidates: usize) -> Self {
        self.two_pass = true;
        self.coarse_dim = coarse_dim.clamp(1, FULL_DIM);
        self.coarse_candidates = coarse_candidates.max(1);
        self
    }

    /// Whether the rerank stage runs.
    pub fn rerank_enabled(&self) -> bool {
        self.rerank_enabled
    }

    /// How many fused candidates feed the reranker.
    pub fn rerank_candidates(&self) -> usize {
        self.rerank_candidates
    }

    /// Whether the two-pass (coarse-then-full) vector search runs. A
    /// `coarse_dim` of [`FULL_DIM`] makes the coarse pass identical to the
    /// full pass, so two-pass is treated as off in that degenerate case.
    pub fn two_pass(&self) -> bool {
        self.two_pass && self.coarse_dim < FULL_DIM
    }

    /// Truncated dimension used for the coarse pass.
    pub fn coarse_dim(&self) -> usize {
        self.coarse_dim
    }

    /// Shortlist size handed from the coarse pass to the full-dim rescoring.
    pub fn coarse_candidates(&self) -> usize {
        self.coarse_candidates
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
        // page_id/anchor collisions. The passage text is the full block body
        // (bulk-fetched), so the cross-encoder scores the whole passage rather
        // than the 200-char snippet lead.
        let passages = self.rerank_passages(&hits).await?;
        let candidates: Vec<Candidate> = passages
            .into_iter()
            .enumerate()
            .map(|(i, text)| Candidate {
                id: i.to_string(),
                text,
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

    /// The passage text each hit contributes to the cross-encoder: the full
    /// block `body`, bulk-fetched in one query for the whole candidate set.
    /// Falls back to the hydrated `snippet` for any hit with no resolvable
    /// block (e.g. the `sql_view` lane, whose hits carry no block body).
    ///
    /// Scoring the full body — not the 200-char snippet lead — lets the
    /// cross-encoder see the whole passage; the reranker's tokenizer truncates
    /// to its own max length, so the per-pair cost stays bounded.
    async fn rerank_passages(&self, hits: &[SearchHit]) -> Result<Vec<String>, IndexerError> {
        use std::collections::HashMap;

        // block_id is "<page_id>:<anchor>" for a block-grain hit.
        let block_id = |h: &SearchHit| h.anchor.as_deref().map(|a| format!("{}:{}", h.page_id, a));
        let block_ids: Vec<String> = hits.iter().filter_map(block_id).collect();

        let mut body_by_block: HashMap<String, String> = HashMap::new();
        if !block_ids.is_empty() {
            let placeholders = std::iter::repeat_n("?", block_ids.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql =
                format!("SELECT block_id, body FROM blocks WHERE block_id IN ({placeholders})");
            let conn = self.conn.lock().await;
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(duckdb::params_from_iter(block_ids.iter()), |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (bid, body) = row?;
                body_by_block.insert(bid, body);
            }
        }

        Ok(hits
            .iter()
            .map(|h| {
                block_id(h)
                    .and_then(|bid| body_by_block.get(&bid).cloned())
                    .unwrap_or_else(|| h.snippet.clone())
            })
            .collect())
    }
}
