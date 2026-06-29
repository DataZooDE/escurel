//! Harness error type.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum EvalError {
    #[error("io error at {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("malformed {file} line {line}: {reason}")]
    Dataset {
        file: String,
        line: usize,
        reason: String,
    },
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("duckdb error: {0}")]
    Duckdb(#[from] duckdb::Error),
    #[error("migration error: {0}")]
    Migration(#[from] escurel_index::schema::MigrationError),
    #[error("indexer error: {0}")]
    Indexer(#[from] escurel_index::IndexerError),
    #[error("embedder error: {0}")]
    Embed(#[from] escurel_embed::EmbedError),
    #[error("{0}")]
    Config(String),
}
