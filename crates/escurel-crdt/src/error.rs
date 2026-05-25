//! Crate-wide error type. Kept narrow: every variant names a real
//! failure mode at this boundary so callers can match without
//! wildcard arms.

use thiserror::Error;

/// Errors returned by [`crate::LiveDoc`] and
/// [`crate::CrdtBackend`] implementations.
#[derive(Debug, Error)]
pub enum Error {
    /// Loro engine rejected the op or snapshot bytes.
    #[error("loro engine error: {0}")]
    Loro(String),

    /// DuckDB / SQL error while persisting an op or snapshot.
    #[error("duckdb error: {0}")]
    Duckdb(#[from] duckdb::Error),

    /// The internal actor task has terminated (channel closed).
    /// Happens after [`crate::LiveDoc::close`] or if the actor
    /// panicked. Callers should re-open the page.
    #[error("livedoc actor is closed")]
    Closed,
}

impl From<loro::LoroError> for Error {
    fn from(e: loro::LoroError) -> Self {
        Self::Loro(format!("{e:?}"))
    }
}

impl From<loro::LoroEncodeError> for Error {
    fn from(e: loro::LoroEncodeError) -> Self {
        Self::Loro(format!("encode: {e:?}"))
    }
}
