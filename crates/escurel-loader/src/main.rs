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
#[cfg(feature = "gemini")]
use escurel_embed::GeminiEmbedder;
use escurel_embed::{Embedder, HashEmbedder, ZeroEmbedder};
#[cfg(not(feature = "kreuzberg"))]
use escurel_index::backend::PlainTextExtractor;
use escurel_index::backend::{ChunkConfig, ExtractConfig, Extractor, OcrPolicy};
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
        /// Embedding model id (embedder=gemini). Recorded in the manifest as
        /// model_id and validated by `transfer --expect-model`.
        #[arg(long, default_value = "gemini-embedding-001")]
        embed_model: String,
        /// Embedding dimension (must equal the schema's FLOAT[768]).
        #[arg(long, default_value_t = 768)]
        embed_dim: usize,
        /// Gemini API key (embedder=gemini). Falls back to GEMINI_API_KEY.
        #[arg(long, env = "ESCUREL_GEMINI_API_KEY")]
        api_key: Option<String>,
        /// Optional metadata sidecar (JSON object keyed by source file name →
        /// extra instance frontmatter, e.g. nummer/titel/wp/doctype/stand).
        #[arg(long)]
        metadata: Option<PathBuf>,
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
    /// Gemini HTTP embedder — embeds with the SAME model a Gemini-backed live
    /// tenant uses, so the transfer carries production-quality vectors. Needs a
    /// key (`--api-key` / `ESCUREL_GEMINI_API_KEY` / `GEMINI_API_KEY`) + network,
    /// but no escurel-server and no per-tenant quota. (feature `gemini`)
    #[cfg(feature = "gemini")]
    Gemini,
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

// `embed_model`/`embed_dim`/`api_key` are only consumed by the Gemini arm.
#[cfg_attr(not(feature = "gemini"), allow(unused_variables))]
fn embedder_for(
    kind: EmbedderKind,
    embed_model: &str,
    embed_dim: usize,
    api_key: Option<String>,
) -> Result<Arc<dyn Embedder>> {
    Ok(match kind {
        EmbedderKind::Hash => Arc::new(HashEmbedder::default()),
        EmbedderKind::Zero => Arc::new(ZeroEmbedder::default()),
        #[cfg(feature = "gemini")]
        EmbedderKind::Gemini => {
            let key = api_key
                .or_else(|| std::env::var("GEMINI_API_KEY").ok())
                .filter(|k| !k.is_empty())
                .context(
                    "gemini embedder needs --api-key or ESCUREL_GEMINI_API_KEY / GEMINI_API_KEY",
                )?;
            Arc::new(
                GeminiEmbedder::new(key)
                    .with_model(embed_model.to_owned())
                    .with_dim(embed_dim),
            )
        }
    })
}

/// The document extractor: Kreuzberg (PDF/DOCX/PPTX/XLSX + text) when built with
/// the `kreuzberg` feature, else plain text only — mirrors the server
/// (`escurel-server/src/mcp.rs`).
fn extractor() -> Arc<dyn Extractor> {
    #[cfg(feature = "kreuzberg")]
    {
        Arc::new(escurel_index::backend::KreuzbergExtractor)
    }
    #[cfg(not(feature = "kreuzberg"))]
    {
        Arc::new(PlainTextExtractor)
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
            embed_model,
            embed_dim,
            api_key,
            metadata,
            chunk_max,
            chunk_overlap,
        } => {
            let embedder = embedder_for(embedder, &embed_model, embed_dim, api_key)?;
            // Load the optional metadata sidecar (JSON object → per-file extra
            // frontmatter).
            let meta_map = match metadata {
                Some(p) => {
                    let raw = std::fs::read(&p).with_context(|| format!("read sidecar {p:?}"))?;
                    serde_json::from_slice::<serde_json::Map<String, serde_json::Value>>(&raw)
                        .with_context(|| format!("parse sidecar {p:?} as a JSON object"))?
                }
                None => serde_json::Map::new(),
            };
            let report = LoaderBuilder::new(out, skill, extractor(), embedder)
                .with_extract_config(ExtractConfig {
                    ocr: OcrPolicy::Off,
                    chunk: ChunkConfig {
                        max_chars: chunk_max,
                        overlap: chunk_overlap,
                    },
                })
                .with_metadata(meta_map)
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
