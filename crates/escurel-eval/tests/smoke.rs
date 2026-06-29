//! End-to-end smoke test: load the tiny fixture, ingest into a real DuckDB
//! tempfile with the offline `HashEmbedder`, run the SinglePass + TwoPass
//! configs, and assert the report is well-formed and the relevant docs are
//! found. No model download, no `#[ignore]` — runs in CI under default features.
//!
//! The fixture queries share distinctive tokens with their relevant doc, so the
//! BM25 (FTS) lane surfaces the right doc even though `HashEmbedder` vectors are
//! semantically meaningless.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use escurel_embed::{Embedder, HashEmbedder};
use escurel_eval::config::RunConfig;
use escurel_eval::dataset::Dataset;
use escurel_eval::{QpsParams, run_matrix};
use tempfile::TempDir;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/tiny")
}

#[tokio::test]
async fn end_to_end_over_tiny_fixture() {
    let dataset = Dataset::load(&fixture_dir()).expect("load fixture");
    assert_eq!(dataset.corpus.len(), 8);
    assert_eq!(dataset.queries.len(), 4);

    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("eval.duckdb");
    let store_dir = tmp.path().join("store");
    let embedder: Arc<dyn Embedder> = Arc::new(HashEmbedder::default());

    let configs = [
        RunConfig::SinglePass,
        RunConfig::TwoPass {
            coarse_dim: 128,
            coarse_candidates: 500,
        },
    ];

    let report = run_matrix(
        &dataset,
        &db_path,
        &store_dir,
        Arc::clone(&embedder),
        None, // no reranker (rerank configs would be skipped)
        &configs,
        "doc",
        10,
        Some(QpsParams {
            workers: 2,
            duration: Duration::from_millis(80),
        }),
        false,
    )
    .await
    .expect("run matrix");

    assert_eq!(report.model_id, "hash");
    assert_eq!(report.dim, 768);
    assert_eq!(report.corpus_docs, 8);
    assert_eq!(report.queries, 4);
    assert_eq!(report.results.len(), 2, "single_pass + two_pass");

    for r in &report.results {
        // Every metric is a probability-like value.
        for (name, v) in [
            ("ndcg@10", r.ndcg_at_10),
            ("ndcg@100", r.ndcg_at_100),
            ("recall@10", r.recall_at_10),
            ("recall@100", r.recall_at_100),
            ("mrr", r.mrr),
            ("map", r.map),
        ] {
            assert!(
                (0.0..=1.0).contains(&v),
                "{} {name}={v} out of [0,1]",
                r.config
            );
        }
        // Latency was sampled once per query.
        assert_eq!(r.latency.n, 4, "{} latency samples", r.config);
        // All 8 docs fit in k=10, so every query's relevant doc is found.
        assert_eq!(r.recall_at_10, 1.0, "{} recall@10", r.config);
        // The BM25 lane ranks the matching doc near the top.
        assert!(r.ndcg_at_10 >= 0.5, "{} ndcg@10={}", r.config, r.ndcg_at_10);
        // The concurrent QPS pass completed some searches.
        let qps = r.qps.as_ref().expect("qps measured");
        assert!(qps.completed > 0, "{} qps completed 0", r.config);
    }

    // Two-pass with a corpus-covering shortlist preserves single-pass recall.
    assert_eq!(
        report.results[0].recall_at_10,
        report.results[1].recall_at_10
    );

    // JSON renders.
    let json = report.to_json();
    assert_eq!(json["results"].as_array().unwrap().len(), 2);
}
