//! Latency percentiles + concurrent QPS, measured against a live `Indexer`.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use escurel_index::Indexer;
use serde::Serialize;

use crate::dataset::Query;
use crate::error::EvalError;

/// Latency distribution over a set of search calls (milliseconds).
#[derive(Debug, Clone, Serialize)]
pub struct LatencyStats {
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub mean_ms: f64,
    pub n: usize,
}

/// Nearest-rank percentiles over `samples` (sorted in place).
#[must_use]
pub fn percentiles(samples: &mut [f64]) -> LatencyStats {
    if samples.is_empty() {
        return LatencyStats {
            p50_ms: 0.0,
            p95_ms: 0.0,
            p99_ms: 0.0,
            mean_ms: 0.0,
            n: 0,
        };
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let pct = |p: f64| {
        // Nearest-rank: ceil(p * n), 1-based, clamped into range.
        let rank = (p * samples.len() as f64).ceil() as usize;
        samples[rank.clamp(1, samples.len()) - 1]
    };
    let mean = samples.iter().sum::<f64>() / samples.len() as f64;
    LatencyStats {
        p50_ms: pct(0.50),
        p95_ms: pct(0.95),
        p99_ms: pct(0.99),
        mean_ms: mean,
        n: samples.len(),
    }
}

/// Run each query once (sequentially), timing the `search` call. Returns the
/// per-query ranked doc ids (deduped to doc granularity, for the metrics) and
/// the latency distribution — computed in the same sweep.
pub async fn run_queries(
    indexer: &Indexer,
    queries: &[Query],
    k: usize,
) -> Result<(Vec<Vec<String>>, LatencyStats), EvalError> {
    let mut ranked_per_query = Vec::with_capacity(queries.len());
    let mut samples = Vec::with_capacity(queries.len());
    for q in queries {
        let start = Instant::now();
        let hits = indexer.search(&q.text, k, None, None, None, None).await?;
        samples.push(start.elapsed().as_secs_f64() * 1000.0);
        ranked_per_query.push(dedup_doc_ids(&hits));
    }
    Ok((ranked_per_query, percentiles(&mut samples)))
}

/// Sustained throughput: `workers` tasks pull queries round-robin from a shared
/// cursor and issue searches for `duration`, counting completions. The
/// `Indexer`'s connection mutex serializes DuckDB access, so this reports the
/// realistic single-writer QPS.
pub async fn measure_qps(
    indexer: Arc<Indexer>,
    queries: Arc<Vec<Query>>,
    k: usize,
    workers: usize,
    duration: Duration,
) -> Result<QpsStats, EvalError> {
    if queries.is_empty() || workers == 0 {
        return Ok(QpsStats {
            qps: 0.0,
            completed: 0,
            secs: 0.0,
            latency: percentiles(&mut []),
        });
    }
    let cursor = Arc::new(AtomicUsize::new(0));
    let completed = Arc::new(AtomicU64::new(0));
    let deadline = Instant::now() + duration;

    let mut handles = Vec::with_capacity(workers);
    for _ in 0..workers {
        let indexer = Arc::clone(&indexer);
        let queries = Arc::clone(&queries);
        let cursor = Arc::clone(&cursor);
        let completed = Arc::clone(&completed);
        handles.push(tokio::spawn(async move {
            let mut samples: Vec<f64> = Vec::new();
            while Instant::now() < deadline {
                let idx = cursor.fetch_add(1, Ordering::Relaxed) % queries.len();
                let start = Instant::now();
                let r = indexer
                    .search(&queries[idx].text, k, None, None, None, None)
                    .await;
                if r.is_ok() {
                    samples.push(start.elapsed().as_secs_f64() * 1000.0);
                    completed.fetch_add(1, Ordering::Relaxed);
                }
            }
            samples
        }));
    }

    let started = Instant::now();
    let mut all_samples = Vec::new();
    for h in handles {
        if let Ok(s) = h.await {
            all_samples.extend(s);
        }
    }
    let secs = started.elapsed().as_secs_f64();
    let done = completed.load(Ordering::Relaxed);
    Ok(QpsStats {
        qps: if secs > 0.0 { done as f64 / secs } else { 0.0 },
        completed: done,
        secs,
        latency: percentiles(&mut all_samples),
    })
}

/// Throughput summary for one config.
#[derive(Debug, Clone, Serialize)]
pub struct QpsStats {
    pub qps: f64,
    pub completed: u64,
    pub secs: f64,
    pub latency: LatencyStats,
}

/// Search returns block-grain hits; collapse to doc (`page_id`) order, keeping
/// the first occurrence — so a multi-block doc counts once at its best rank.
fn dedup_doc_ids(hits: &[escurel_index::SearchHit]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(hits.len());
    for h in hits {
        if seen.insert(h.page_id.clone()) {
            out.push(h.page_id.clone());
        }
    }
    out
}
