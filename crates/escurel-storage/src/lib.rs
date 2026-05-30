//! Object storage abstraction for Escurel.
//!
//! Every tenant's bytes live behind one trait, [`LaneStore`], so the
//! filesystem-backed default and the S3-backed variant share the
//! upper layers verbatim. This crate ships [`FsStore`] for dev /
//! tests and [`S3Store`] (behind the `s3` feature) for the
//! production / substrate target.
//!
//! See `docs/spec/storage.md §The LaneStore trait` for the contract.

pub mod fs;
mod key;
#[cfg(feature = "s3")]
pub mod s3;

pub use fs::FsStore;
pub use key::{Key, KeyError};
#[cfg(feature = "s3")]
pub use s3::{S3Store, S3StoreConfig};

use async_trait::async_trait;
use bytes::Bytes;
use thiserror::Error;
use url::Url;

/// Opaque per-store version identifier returned by [`LaneStore::write`].
///
/// FS backend uses the file's modification time in unix nanoseconds.
/// S3 backend will use the object's version-id / etag.
pub type Version = String;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("not found: {0:?}")]
    NotFound(Key),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid file URL for {0:?}")]
    InvalidFileUrl(Key),
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// Byte-level storage abstraction. Implementations must be safe to
/// share across tasks (`Send + Sync`) and live for the program
/// lifetime (`'static`).
///
/// `open_writer` (streaming write) is intentionally absent from this
/// trait until a caller actually needs it.
#[async_trait]
pub trait LaneStore: Send + Sync + 'static {
    /// Read the full body at `key`. Returns [`StoreError::NotFound`]
    /// if the key has no value.
    async fn read(&self, key: &Key) -> Result<Bytes>;

    /// Write `body` to `key` atomically (write-then-publish) and
    /// return the new content's [`Version`].
    async fn write(&self, key: &Key, body: Bytes) -> Result<Version>;

    /// Enumerate keys under `prefix` in unspecified order.
    /// Returns an empty vec if the prefix has no values.
    async fn list(&self, prefix: &Key) -> Result<Vec<Key>>;

    /// Remove the value at `key`. Returns [`StoreError::NotFound`]
    /// if the key has no value.
    async fn delete(&self, key: &Key) -> Result<()>;

    /// URL form of `key`, suitable for handing to DuckDB
    /// (`httpfs` / `file://`) without copying through this
    /// process.
    fn url(&self, key: &Key) -> Result<Url>;

    /// Human-readable backend kind for admin lane introspection
    /// (`"fs"`, `"s3"`). Adapters that don't override report
    /// `"unknown"`.
    fn backend(&self) -> &'static str {
        "unknown"
    }

    /// Byte length of the value at `key` without transferring the
    /// body. Returns [`StoreError::NotFound`] if the key has no value.
    /// The default reads the full body; backends override with a
    /// cheaper metadata / HEAD call.
    async fn size(&self, key: &Key) -> Result<u64> {
        Ok(self.read(key).await?.len() as u64)
    }
}
