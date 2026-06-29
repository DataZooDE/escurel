//! The retrieval-config matrix: each variant maps to an `Indexer` built with
//! the right `RetrievalConfig` / reranker.
//!
//! - `SinglePass` — `RetrievalConfig::disabled()` (today's full-dim search).
//! - `TwoPass` — `disabled().with_two_pass(..)` (issue #218).
//! - `Rerank` — `enabled(n)` + a cross-encoder (issue #215).
//! - `TwoPassRerank` — both.
//!
//! The `#215` delta is `SinglePass` vs `Rerank`; the `#218` delta is
//! `SinglePass` vs `TwoPass`.

use std::sync::Arc;

use escurel_embed::Reranker;
use escurel_index::{Indexer, RetrievalConfig};

#[derive(Debug, Clone, Copy)]
pub enum RunConfig {
    SinglePass,
    TwoPass {
        coarse_dim: usize,
        coarse_candidates: usize,
    },
    Rerank {
        candidates: usize,
    },
    TwoPassRerank {
        coarse_dim: usize,
        coarse_candidates: usize,
        candidates: usize,
    },
}

impl RunConfig {
    /// Stable label used as the JSON key + table row.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::SinglePass => "single_pass",
            Self::TwoPass { .. } => "two_pass",
            Self::Rerank { .. } => "rerank",
            Self::TwoPassRerank { .. } => "two_pass_rerank",
        }
    }

    /// Whether this config needs a cross-encoder reranker injected.
    #[must_use]
    pub fn needs_reranker(&self) -> bool {
        matches!(self, Self::Rerank { .. } | Self::TwoPassRerank { .. })
    }

    fn retrieval(&self) -> RetrievalConfig {
        match *self {
            Self::SinglePass => RetrievalConfig::disabled(),
            Self::TwoPass {
                coarse_dim,
                coarse_candidates,
            } => RetrievalConfig::disabled().with_two_pass(coarse_dim, coarse_candidates),
            Self::Rerank { candidates } => RetrievalConfig::enabled(candidates),
            Self::TwoPassRerank {
                coarse_dim,
                coarse_candidates,
                candidates,
            } => RetrievalConfig::enabled(candidates).with_two_pass(coarse_dim, coarse_candidates),
        }
    }

    /// Apply this config to a freshly opened `Indexer`. A reranker is required
    /// for the `Rerank`/`TwoPassRerank` variants and ignored otherwise.
    #[must_use]
    pub fn apply(&self, indexer: Indexer, reranker: Option<Arc<dyn Reranker>>) -> Indexer {
        let retrieval = self.retrieval();
        match (self.needs_reranker(), reranker) {
            (true, Some(r)) => indexer.with_reranker(r, retrieval),
            // No reranker available → fall back to first-stage only for this
            // config (the caller decides whether to skip rerank rows entirely).
            _ => indexer.with_retrieval(retrieval),
        }
    }
}
