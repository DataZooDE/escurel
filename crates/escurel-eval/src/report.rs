//! The evaluation report: per-config metrics + latency + QPS, as serializable
//! structs with JSON and human-table renderings.

use serde::Serialize;

use crate::dataset::Dataset;
use crate::latency::{LatencyStats, QpsStats};
use crate::metrics::{RelMap, mean_average_precision, mean_mrr, mean_ndcg_at_k, mean_recall_at_k};

/// Metrics for one retrieval config over the whole query set.
#[derive(Debug, Clone, Serialize)]
pub struct ConfigResult {
    pub config: String,
    pub ndcg_at_10: f64,
    pub ndcg_at_100: f64,
    pub recall_at_10: f64,
    pub recall_at_100: f64,
    pub mrr: f64,
    pub map: f64,
    pub latency: LatencyStats,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qps: Option<QpsStats>,
}

impl ConfigResult {
    /// Score one config's per-query rankings against the dataset's qrels.
    /// `ranked_per_query` is aligned with `dataset.queries`.
    #[must_use]
    pub fn score(
        config: &str,
        ranked_per_query: &[Vec<String>],
        dataset: &Dataset,
        latency: LatencyStats,
        qps: Option<QpsStats>,
    ) -> Self {
        let runs: Vec<(Vec<String>, RelMap)> = ranked_per_query
            .iter()
            .zip(dataset.queries.iter())
            .map(|(ranked, q)| (ranked.clone(), dataset.rel(&q.id)))
            .collect();
        Self {
            config: config.to_owned(),
            ndcg_at_10: mean_ndcg_at_k(&runs, 10),
            ndcg_at_100: mean_ndcg_at_k(&runs, 100),
            recall_at_10: mean_recall_at_k(&runs, 10),
            recall_at_100: mean_recall_at_k(&runs, 100),
            mrr: mean_mrr(&runs),
            map: mean_average_precision(&runs),
            latency,
            qps,
        }
    }
}

/// The full report for one dataset + embedder over the config matrix.
#[derive(Debug, Clone, Serialize)]
pub struct EvalReport {
    pub dataset: String,
    pub model_id: String,
    pub dim: usize,
    pub corpus_docs: usize,
    pub queries: usize,
    pub k: usize,
    pub results: Vec<ConfigResult>,
}

impl EvalReport {
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }

    /// A fixed-width table for the terminal.
    #[must_use]
    pub fn to_table(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "dataset={} model={} dim={} corpus={} queries={} k={}\n",
            self.dataset, self.model_id, self.dim, self.corpus_docs, self.queries, self.k
        ));
        out.push_str(&format!(
            "{:<16} {:>8} {:>9} {:>9} {:>10} {:>7} {:>7} {:>8} {:>8} {:>9}\n",
            "config",
            "nDCG@10",
            "nDCG@100",
            "recall@10",
            "recall@100",
            "MRR",
            "MAP",
            "p50_ms",
            "p95_ms",
            "qps",
        ));
        for r in &self.results {
            let qps = r.qps.as_ref().map_or(0.0, |q| q.qps);
            out.push_str(&format!(
                "{:<16} {:>8.4} {:>9.4} {:>9.4} {:>10.4} {:>7.4} {:>7.4} {:>8.2} {:>8.2} {:>9.1}\n",
                r.config,
                r.ndcg_at_10,
                r.ndcg_at_100,
                r.recall_at_10,
                r.recall_at_100,
                r.mrr,
                r.map,
                r.latency.p50_ms,
                r.latency.p95_ms,
                qps,
            ));
        }
        out
    }
}
