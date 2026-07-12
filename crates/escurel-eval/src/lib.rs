//! Retrieval-quality + latency/QPS evaluation harness for Escurel.
//!
//! Loads a labeled IR dataset (BEIR format — see [`dataset`]), ingests the
//! corpus once into a persistent DuckDB ([`ingest`]), runs
//! [`escurel_index::Indexer::search`] under each retrieval configuration
//! ([`config`]), and reports nDCG / recall / MRR / MAP ([`metrics`]) plus
//! latency percentiles and concurrent QPS ([`latency`]) as a [`report::EvalReport`].
//!
//! The metric + dataset + report layers are pure and offline (CI runs them with
//! the deterministic [`escurel_embed::HashEmbedder`]); the real 768-d
//! EmbeddingGemma + cross-encoder paths are behind the `candle` / `rerank`
//! features, exercised by the operator `run` command, not CI.

pub mod config;
pub mod dataset;
pub mod error;
pub mod gate;
pub mod ingest;
pub mod latency;
pub mod metrics;
pub mod report;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use escurel_embed::{Embedder, Reranker};

use crate::config::RunConfig;
use crate::dataset::Dataset;
use crate::error::EvalError;
use crate::report::{ConfigResult, EvalReport};

/// QPS measurement parameters.
#[derive(Debug, Clone, Copy)]
pub struct QpsParams {
    pub workers: usize,
    pub duration: Duration,
}

/// Run the whole config matrix against a corpus and return the report.
///
/// The corpus is embedded + indexed **once** into `db_path` (unless
/// `skip_ingest`, which reopens an already-built index), then every config
/// reuses that single DuckDB connection — reconfigured via the `Indexer`
/// builders between configs — so we never open the same DuckDB file twice (the
/// single-writer trap) and never re-embed.
///
/// `reranker` is required for the `Rerank`/`TwoPassRerank` configs; when it is
/// `None`, those configs are skipped (the caller logs the skip).
#[allow(clippy::too_many_arguments)]
pub async fn run_matrix(
    dataset: &Dataset,
    db_path: &Path,
    store_dir: &Path,
    embedder: Arc<dyn Embedder>,
    reranker: Option<Arc<dyn Reranker>>,
    configs: &[RunConfig],
    skill: &str,
    k: usize,
    qps: Option<QpsParams>,
    skip_ingest: bool,
    contextualize: escurel_index::backend::document::ContextualizeMode,
) -> Result<EvalReport, EvalError> {
    if !skip_ingest {
        ingest::ingest_corpus(
            db_path,
            store_dir,
            Arc::clone(&embedder),
            &dataset.corpus,
            skill,
            contextualize,
        )
        .await?;
    }

    let queries = Arc::new(dataset.queries.clone());
    let mut indexer = ingest::open_indexer(db_path, store_dir, Arc::clone(&embedder), false)?;
    let mut results = Vec::new();

    for cfg in configs {
        if cfg.needs_reranker() && reranker.is_none() {
            tracing::warn!(
                config = cfg.label(),
                "no reranker available; skipping config"
            );
            continue;
        }
        indexer = cfg.apply(indexer, reranker.clone());

        let (ranked_per_query, latency) =
            latency::run_queries(&indexer, &dataset.queries, k).await?;

        let qps_stats = if let Some(p) = qps {
            // Move the indexer behind an Arc for the concurrent pass, then
            // reclaim it (all workers have joined, so the refcount is 1).
            let arc = Arc::new(indexer);
            let stats = latency::measure_qps(
                Arc::clone(&arc),
                Arc::clone(&queries),
                k,
                p.workers,
                p.duration,
            )
            .await?;
            indexer = Arc::try_unwrap(arc)
                .map_err(|_| EvalError::Config("qps workers outlived the pass".into()))?;
            Some(stats)
        } else {
            None
        };

        results.push(ConfigResult::score(
            cfg.label(),
            &ranked_per_query,
            dataset,
            latency,
            qps_stats,
        ));
    }

    Ok(EvalReport {
        dataset: dataset.name.clone(),
        model_id: embedder.model_id(),
        dim: embedder.dim(),
        corpus_docs: dataset.corpus.len(),
        queries: dataset.queries.len(),
        k,
        results,
    })
}
