//! `escurel-loader` — host-side operator CLI for the offline batch loader.
//!
//! Two subcommands, both operating **directly on disk** (no HTTP, no server):
//!
//!   escurel-loader build    --src <dir> --out <loader_dir> --skill <name> [...]
//!   escurel-loader transfer --from <loader_dir> --to <live_data_dir>
//!                           --tenant <t> --expect-model <id> [--on-collision skip]
//!
//! `build` ingests a corpus into a throwaway loader instance at full speed with
//! the chosen offline embedder; `transfer` validates the manifest against the
//! live tenant's embedder identity and merges the result in, carrying the
//! embeddings as data so production never re-embeds.
//!
//! This lives in `escurel-loader` (not `escurel-cli`) on purpose: the transfer
//! must `ATTACH` two DuckDB files co-located on disk, which cannot go through
//! the gateway's HTTP client surface that `escurel-cli` is built on.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use escurel_embed::{Embedder, HashEmbedder, ZeroEmbedder};
use escurel_index::backend::{
    ChunkConfig, ExtractConfig, Extractor, OcrPolicy, PlainTextExtractor,
};
use escurel_index::indexer::OnCollision;
use escurel_loader::{LoaderBuilder, transfer};
use serde_json::json;

#[derive(Parser, Debug)]
#[command(
    name = "escurel-loader",
    about = "Offline batch loader + DuckDB→DuckDB transfer",
    version
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Build a loader artifact from a source directory of documents.
    Build {
        /// Directory of documents to ingest (recursive).
        #[arg(long)]
        src: PathBuf,
        /// Output loader directory (DuckDB + blobs + markdown + manifest.json).
        #[arg(long)]
        out: PathBuf,
        /// Document skill the instances are materialised under.
        #[arg(long)]
        skill: String,
        /// Offline embedder. Must match the live tenant's model + dim (768).
        #[arg(long, value_enum, default_value_t = EmbedderKind::Hash)]
        embedder: EmbedderKind,
        /// Max characters per chunk.
        #[arg(long, default_value_t = 1200)]
        chunk_max: usize,
        /// Character overlap between adjacent chunks.
        #[arg(long, default_value_t = 200)]
        chunk_overlap: usize,
    },
    /// Transfer a loader artifact into a live escurel data dir (no re-embed).
    Transfer {
        /// Loader directory produced by `build`.
        #[arg(long)]
        from: PathBuf,
        /// Live escurel data dir (contains escurel.duckdb + tenant LaneStore).
        #[arg(long)]
        to: PathBuf,
        /// Live tenant id to merge into.
        #[arg(long)]
        tenant: String,
        /// The live tenant's embedder identity (`Embedder::model_id`); the
        /// artifact manifest must match or the transfer aborts.
        #[arg(long)]
        expect_model: String,
        /// What to do when a source page_id already exists in the target.
        #[arg(long, value_enum, default_value_t = CollisionKind::Skip)]
        on_collision: CollisionKind,
    },
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum EmbedderKind {
    /// Deterministic 768-dim hash embedder (offline, no network).
    Hash,
    /// All-zero 768-dim vectors (testing / structural loads only).
    Zero,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum CollisionKind {
    Skip,
    Replace,
    Error,
}

impl From<CollisionKind> for OnCollision {
    fn from(k: CollisionKind) -> Self {
        match k {
            CollisionKind::Skip => OnCollision::Skip,
            CollisionKind::Replace => OnCollision::Replace,
            CollisionKind::Error => OnCollision::Error,
        }
    }
}

fn embedder_for(kind: EmbedderKind) -> Arc<dyn Embedder> {
    match kind {
        EmbedderKind::Hash => Arc::new(HashEmbedder::default()),
        EmbedderKind::Zero => Arc::new(ZeroEmbedder::default()),
    }
}

#[tokio::main]
async fn main() {
    if let Err(e) = run(Cli::parse()).await {
        eprintln!("{}", json!({ "error": format!("{e:#}") }));
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<()> {
    match cli.cmd {
        Cmd::Build {
            src,
            out,
            skill,
            embedder,
            chunk_max,
            chunk_overlap,
        } => {
            let extractor: Arc<dyn Extractor> = Arc::new(PlainTextExtractor);
            let report = LoaderBuilder::new(out, skill, extractor, embedder_for(embedder))
                .with_extract_config(ExtractConfig {
                    ocr: OcrPolicy::Off,
                    chunk: ChunkConfig {
                        max_chars: chunk_max,
                        overlap: chunk_overlap,
                    },
                })
                .build(&src)
                .await
                .context("loader build")?;
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "model_id": report.manifest.model_id,
                    "dim": report.manifest.dim,
                    "schema_version": report.manifest.schema_version,
                    "skill": report.manifest.skill,
                    "doc_count": report.manifest.doc_count,
                    "chunk_count": report.manifest.chunk_count,
                    "failed": report.failed,
                    "skipped": report.skipped,
                }))?
            );
        }
        Cmd::Transfer {
            from,
            to,
            tenant,
            expect_model,
            on_collision,
        } => {
            let report = transfer(&from, &to, &tenant, &expect_model, on_collision.into())
                .await
                .context("transfer")?;
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "tenant": tenant,
                    "model_id": report.manifest.model_id,
                    "blobs_copied": report.files.blobs,
                    "overlays_copied": report.files.overlays,
                    "source_pages": report.merge.source_pages,
                    "collisions": report.merge.collisions,
                }))?
            );
        }
    }
    Ok(())
}
