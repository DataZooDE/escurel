//! `escurel-eval` — run the retrieval-config matrix over a BEIR dataset and
//! report nDCG / recall / MRR / MAP + latency p50/p95/p99 + concurrent QPS.
//!
//! Build with `--features candle,rerank` for real 768-d BERT vectors + the
//! cross-encoder reranker; without those features it falls back to the
//! deterministic offline `HashEmbedder` (results are NOT semantically
//! meaningful — for plumbing only) and skips the rerank configs.
//!
//! ```text
//! cargo run -p escurel-eval --features candle,rerank --release -- \
//!   --dataset datasets/scifact --skill paper \
//!   --embed-model BAAI/bge-base-en-v1.5 \
//!   --reranker BAAI/bge-reranker-base --k 100 \
//!   --qps-workers 16 --qps-secs 30 --format json
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use escurel_embed::{Embedder, Reranker};
use escurel_eval::config::RunConfig;
use escurel_eval::dataset::Dataset;
use escurel_eval::error::EvalError;
use escurel_eval::gate::{Thresholds, evaluate};
use escurel_eval::{QpsParams, run_matrix};
use serde_json::json;

#[derive(Parser, Debug)]
#[command(
    name = "escurel-eval",
    about = "Escurel retrieval-quality + latency/QPS eval"
)]
struct Cli {
    /// BEIR dataset directory (corpus.jsonl, queries.jsonl, qrels/test.tsv).
    #[arg(long)]
    dataset: PathBuf,
    /// Skill stamped on every ingested doc.
    #[arg(long, default_value = "paper")]
    skill: String,
    /// Persistent DuckDB path. Default: `<dataset>/.eval/index.duckdb`.
    #[arg(long)]
    db: Option<PathBuf>,
    /// FsStore dir. Default: `<dataset>/.eval/store`.
    #[arg(long)]
    store: Option<PathBuf>,
    /// 768-d BERT-family embedding model (HF repo id). Only used with
    /// `--features candle`. `CandleEmbedder` loads BERT models; Gemma3
    /// (EmbeddingGemma) is not yet supported by candle-transformers.
    #[arg(long, default_value = "BAAI/bge-base-en-v1.5")]
    embed_model: String,
    /// Cross-encoder reranker (HF repo id). Only used with `--features rerank`.
    #[arg(long, default_value = "BAAI/bge-reranker-base")]
    reranker: String,
    /// Hits per query.
    #[arg(long, default_value_t = 100)]
    k: usize,
    /// Two-pass coarse prefix dim.
    #[arg(long, default_value_t = 128)]
    coarse_dim: usize,
    /// Two-pass coarse shortlist size.
    #[arg(long, default_value_t = 500)]
    coarse_candidates: usize,
    /// Rerank candidate pool.
    #[arg(long, default_value_t = 100)]
    rerank_candidates: usize,
    /// Concurrent QPS workers (0 = skip the QPS pass).
    #[arg(long, default_value_t = 0)]
    qps_workers: usize,
    /// QPS measurement duration, seconds.
    #[arg(long, default_value_t = 0)]
    qps_secs: u64,
    /// Reopen a previously built index instead of re-embedding.
    #[arg(long, default_value_t = false)]
    skip_ingest: bool,
    /// Contextual Retrieval mode at ingest (#216): `off` | `structural`.
    /// Run once with each and compare reports to measure the delta.
    #[arg(long, default_value = "off")]
    contextualize: String,
    /// Output format.
    #[arg(long, default_value = "table")]
    format: Format,
    /// Optional thresholds file; exit non-zero if any check fails.
    #[arg(long)]
    gate: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum Format {
    Table,
    Json,
}

#[tokio::main]
async fn main() {
    if let Err(e) = run(Cli::parse()).await {
        eprintln!("{}", json!({ "error": format!("{e:#}") }));
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<(), EvalError> {
    let dataset = Dataset::load(&cli.dataset)?;
    let eval_dir = cli.dataset.join(".eval");
    let db_path = cli.db.unwrap_or_else(|| eval_dir.join("index.duckdb"));
    let store_dir = cli.store.unwrap_or_else(|| eval_dir.join("store"));
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| EvalError::Io {
            path: parent.display().to_string(),
            source,
        })?;
    }

    let embedder = build_embedder(&cli.embed_model).await?;
    let reranker = build_reranker(&cli.reranker).await?;

    // Matrix: always single-pass + two-pass; add the rerank variants when a
    // reranker is available.
    let mut configs = vec![
        RunConfig::SinglePass,
        RunConfig::TwoPass {
            coarse_dim: cli.coarse_dim,
            coarse_candidates: cli.coarse_candidates,
        },
    ];
    if reranker.is_some() {
        configs.push(RunConfig::Rerank {
            candidates: cli.rerank_candidates,
        });
        configs.push(RunConfig::TwoPassRerank {
            coarse_dim: cli.coarse_dim,
            coarse_candidates: cli.coarse_candidates,
            candidates: cli.rerank_candidates,
        });
    } else {
        eprintln!("note: no reranker (build with --features rerank); skipping rerank configs");
    }

    let qps = (cli.qps_workers > 0 && cli.qps_secs > 0).then_some(QpsParams {
        workers: cli.qps_workers,
        duration: Duration::from_secs(cli.qps_secs),
    });

    let report = run_matrix(
        &dataset,
        &db_path,
        &store_dir,
        embedder,
        reranker,
        &configs,
        &cli.skill,
        cli.k,
        qps,
        cli.skip_ingest,
        escurel_index::backend::document::ContextualizeMode::parse(&cli.contextualize),
    )
    .await?;

    match cli.format {
        Format::Json => println!(
            "{}",
            serde_json::to_string_pretty(&report.to_json()).unwrap_or_default()
        ),
        Format::Table => print!("{}", report.to_table()),
    }

    if let Some(path) = cli.gate {
        let raw = std::fs::read_to_string(&path).map_err(|source| EvalError::Io {
            path: path.display().to_string(),
            source,
        })?;
        let thresholds = Thresholds::parse(&raw).map_err(EvalError::Config)?;
        let outcome = evaluate(&report, &thresholds);
        for c in &outcome.checks {
            eprintln!(
                "[{}] {} {}: {:.4} vs {:.4}",
                if c.ok { "PASS" } else { "FAIL" },
                c.config,
                c.metric,
                c.value,
                c.threshold
            );
        }
        if !outcome.passed() {
            return Err(EvalError::Config("gate failed".into()));
        }
    }

    Ok(())
}

#[cfg(feature = "candle")]
async fn build_embedder(model: &str) -> Result<Arc<dyn Embedder>, EvalError> {
    // 768-d is mandatory (blocks.dense_vec is FLOAT[768]) and CandleEmbedder
    // loads BERT-family models — so a 768-d BERT encoder, e.g. bge-base-en-v1.5.
    let e = escurel_embed::CandleEmbedder::from_hf_hub(model, 768).await?;
    Ok(Arc::new(e))
}

#[cfg(not(feature = "candle"))]
async fn build_embedder(_model: &str) -> Result<Arc<dyn Embedder>, EvalError> {
    eprintln!(
        "warning: built without --features candle; using HashEmbedder \
         (deterministic, NOT semantically meaningful — plumbing only)"
    );
    Ok(Arc::new(escurel_embed::HashEmbedder::default()))
}

#[cfg(feature = "rerank")]
async fn build_reranker(model: &str) -> Result<Option<Arc<dyn Reranker>>, EvalError> {
    let r = escurel_embed::CrossEncoderReranker::from_hf_hub(model).await?;
    Ok(Some(Arc::new(r)))
}

#[cfg(not(feature = "rerank"))]
async fn build_reranker(_model: &str) -> Result<Option<Arc<dyn Reranker>>, EvalError> {
    Ok(None)
}
