//! Object storage abstraction for Escurel.
//!
//! Every tenant's bytes live behind one trait, [`LaneStore`], so the
//! filesystem-backed default and the S3-backed variant share the
//! upper layers verbatim. This crate ships [`FsStore`] for dev /
//! tests and [`S3Store`] (behind the `s3` feature) for the
//! production / substrate target.
//!
//! See `docs/spec/storage.md §The LaneStore trait` for the contract.

pub mod blob;
pub mod fs;
mod key;
#[cfg(feature = "s3")]
pub mod s3;

pub use blob::{BLOB_PREFIX, BlobId, INBOX_PREFIX};
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
    #[error("blob exceeds the {limit}-byte per-blob quota (was {actual})")]
    BlobTooLarge { limit: u64, actual: u64 },
    #[error("invalid blob id: {0}")]
    InvalidBlobId(String),
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

    // ── Content-addressed blobs (Document/RAG backend) ────────────────
    // Default impls ride the read/write/list primitives, so every backend
    // gets them for free. Keyed by sha256 (see [`blob`]).

    /// Store `body` as a canonical content-addressed blob and return its
    /// [`BlobId`]. Idempotent — the same bytes always map to the same key.
    /// `max_bytes` is the per-blob size quota (`None` = unlimited);
    /// oversize bodies are rejected with [`StoreError::BlobTooLarge`]
    /// before any write (REQ-NF-07).
    async fn put_blob(&self, tenant: &str, body: Bytes, max_bytes: Option<u64>) -> Result<BlobId> {
        if let Some(limit) = max_bytes
            && body.len() as u64 > limit
        {
            return Err(StoreError::BlobTooLarge {
                limit,
                actual: body.len() as u64,
            });
        }
        let id = BlobId::of(&body);
        let key = blob_key(tenant, BLOB_PREFIX, &id)?;
        self.write(&key, body).await?;
        Ok(id)
    }

    /// Deposit `body` into the inbox (staging) area before processing —
    /// the canonical original lands here first so an upload is never lost
    /// (REQ-DOC-02/04). Same content-addressing + size quota as [`put_blob`].
    async fn put_inbox_blob(
        &self,
        tenant: &str,
        body: Bytes,
        max_bytes: Option<u64>,
    ) -> Result<BlobId> {
        if let Some(limit) = max_bytes
            && body.len() as u64 > limit
        {
            return Err(StoreError::BlobTooLarge {
                limit,
                actual: body.len() as u64,
            });
        }
        let id = BlobId::of(&body);
        let key = blob_key(tenant, INBOX_PREFIX, &id)?;
        self.write(&key, body).await?;
        Ok(id)
    }

    /// Read a canonical blob by id.
    async fn get_blob(&self, tenant: &str, id: &BlobId) -> Result<Bytes> {
        self.read(&blob_key(tenant, BLOB_PREFIX, id)?).await
    }

    /// Read an inbox blob by id.
    async fn get_inbox_blob(&self, tenant: &str, id: &BlobId) -> Result<Bytes> {
        self.read(&blob_key(tenant, INBOX_PREFIX, id)?).await
    }

    /// Promote an inbox blob to the canonical area (after a successful
    /// materialise). Idempotent; the inbox copy is removed.
    async fn promote_inbox_blob(&self, tenant: &str, id: &BlobId) -> Result<()> {
        let body = self.get_inbox_blob(tenant, id).await?;
        self.write(&blob_key(tenant, BLOB_PREFIX, id)?, body)
            .await?;
        self.delete(&blob_key(tenant, INBOX_PREFIX, id)?).await
    }

    /// Delete a canonical blob by id. Used by `rebuild` to reclaim orphan
    /// blobs no overlay references.
    async fn delete_blob(&self, tenant: &str, id: &BlobId) -> Result<()> {
        self.delete(&blob_key(tenant, BLOB_PREFIX, id)?).await
    }

    /// List canonical blob ids for a tenant.
    async fn list_blobs(&self, tenant: &str) -> Result<Vec<BlobId>> {
        let prefix = Key::new(tenant, BLOB_PREFIX).map_err(key_err)?;
        let keys = self.list(&prefix).await?;
        Ok(keys
            .iter()
            .filter_map(|k| {
                // Skip the inbox subtree; keep only blobs/<hex>.
                let path = k.path();
                let name = path.strip_prefix("blobs/")?;
                if name.contains('/') {
                    return None; // inbox/<hex> etc.
                }
                BlobId::parse(&format!("sha256:{name}"))
            })
            .collect())
    }
}

/// The `Key` for a blob id under `prefix` (`blobs` or `blobs/inbox`).
fn blob_key(tenant: &str, prefix: &str, id: &BlobId) -> Result<Key> {
    Key::new(tenant, format!("{prefix}/{}", id.hex())).map_err(key_err)
}

fn key_err(e: KeyError) -> StoreError {
    StoreError::InvalidBlobId(e.to_string())
}
