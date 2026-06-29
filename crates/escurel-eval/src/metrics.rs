//! Information-retrieval metrics — pure, dependency-free, unit-tested.
//!
//! Each function scores ONE query: a ranked list of doc ids (best first, as
//! returned by [`escurel_index::Indexer::search`]'s `page_id`s) against that
//! query's relevance judgments ([`RelMap`]: doc id → graded gain, where 0 means
//! irrelevant). The `mean_*` helpers average a metric over a whole query set.
//!
//! Conventions match `trec_eval` / BEIR:
//! - **recall@k** = (# relevant docs in the top-k) / (total relevant for the query).
//! - **nDCG@k** = DCG@k / IDCG@k, with graded gains and a `log2(rank+1)` discount
//!   (rank 1-based), IDCG from the gains sorted descending.
//! - **MRR** = 1 / (rank of the first relevant doc), 0 if none.
//! - **average precision** = mean of precision@k taken at each relevant hit;
//!   its mean over the query set is MAP.

use std::collections::HashMap;

/// Graded relevance for a single query: doc id → gain (0 = irrelevant).
pub type RelMap = HashMap<String, u32>;

/// How many of the query's relevant docs appear in the top-`k`, over the total
/// number of relevant docs. Returns 0.0 when the query has no relevant docs.
#[must_use]
pub fn recall_at_k(ranked: &[String], rel: &RelMap, k: usize) -> f64 {
    let total_relevant = rel.values().filter(|&&g| g > 0).count();
    if total_relevant == 0 {
        return 0.0;
    }
    let hits = ranked
        .iter()
        .take(k)
        .filter(|id| rel.get(*id).is_some_and(|&g| g > 0))
        .count();
    hits as f64 / total_relevant as f64
}

/// Normalized discounted cumulative gain at `k` (graded). 0.0 when the query
/// has no relevant docs (IDCG would be 0).
#[must_use]
pub fn ndcg_at_k(ranked: &[String], rel: &RelMap, k: usize) -> f64 {
    let dcg = dcg_at_k(ranked.iter().map(|id| gain(rel, id)), k);
    let mut ideal: Vec<f64> = rel.values().map(|&g| f64::from(g)).collect();
    ideal.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
    let idcg = dcg_at_k(ideal.into_iter(), k);
    if idcg == 0.0 { 0.0 } else { dcg / idcg }
}

/// Reciprocal rank of the first relevant doc (0.0 if none in `ranked`).
#[must_use]
pub fn mrr(ranked: &[String], rel: &RelMap) -> f64 {
    for (i, id) in ranked.iter().enumerate() {
        if rel.get(id).is_some_and(|&g| g > 0) {
            return 1.0 / (i as f64 + 1.0);
        }
    }
    0.0
}

/// Average precision: the mean of precision@rank taken at each relevant hit,
/// divided by the total number of relevant docs (binary relevance, gain > 0).
/// 0.0 when the query has no relevant docs.
#[must_use]
pub fn average_precision(ranked: &[String], rel: &RelMap) -> f64 {
    let total_relevant = rel.values().filter(|&&g| g > 0).count();
    if total_relevant == 0 {
        return 0.0;
    }
    let mut hits = 0usize;
    let mut sum_precisions = 0.0;
    for (i, id) in ranked.iter().enumerate() {
        if rel.get(id).is_some_and(|&g| g > 0) {
            hits += 1;
            sum_precisions += hits as f64 / (i as f64 + 1.0);
        }
    }
    sum_precisions / total_relevant as f64
}

/// Mean nDCG@`k` over a query set. Each entry pairs a query's ranked ids with
/// its relevance map. Empty input → 0.0.
#[must_use]
pub fn mean_ndcg_at_k(runs: &[(Vec<String>, RelMap)], k: usize) -> f64 {
    mean(runs, |ranked, rel| ndcg_at_k(ranked, rel, k))
}

/// Mean recall@`k` over a query set.
#[must_use]
pub fn mean_recall_at_k(runs: &[(Vec<String>, RelMap)], k: usize) -> f64 {
    mean(runs, |ranked, rel| recall_at_k(ranked, rel, k))
}

/// Mean reciprocal rank over a query set.
#[must_use]
pub fn mean_mrr(runs: &[(Vec<String>, RelMap)]) -> f64 {
    mean(runs, mrr)
}

/// Mean average precision (MAP) over a query set.
#[must_use]
pub fn mean_average_precision(runs: &[(Vec<String>, RelMap)]) -> f64 {
    mean(runs, average_precision)
}

fn gain(rel: &RelMap, id: &str) -> f64 {
    rel.get(id).map_or(0.0, |&g| f64::from(g))
}

/// DCG over the first `k` gains with a `log2(rank + 1)` discount (rank 1-based).
fn dcg_at_k(gains: impl Iterator<Item = f64>, k: usize) -> f64 {
    gains
        .take(k)
        .enumerate()
        .map(|(i, g)| g / ((i as f64 + 2.0).log2()))
        .sum()
}

fn mean(runs: &[(Vec<String>, RelMap)], f: impl Fn(&[String], &RelMap) -> f64) -> f64 {
    if runs.is_empty() {
        return 0.0;
    }
    let sum: f64 = runs.iter().map(|(ranked, rel)| f(ranked, rel)).sum();
    sum / runs.len() as f64
}
