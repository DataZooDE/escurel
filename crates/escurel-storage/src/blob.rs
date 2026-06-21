//! Content-addressed blob storage for retained document originals
//! (Document/RAG backend, REQ-DOC-02 / HLD §9).
//!
//! A blob is keyed by the sha256 of its content, so storing the same bytes
//! twice is idempotent and the id is a tamper-evident handle. Two areas:
//!
//! - `blobs/<hex>` — the **canonical** retained original. Part of the
//!   tenant's canonical corpus (everything else about a document — text,
//!   chunks, embeddings, FTS — is derivable from it on rebuild).
//! - `blobs/inbox/<hex>` — the **staging** area an external uploader
//!   deposits into *before* processing (the deposit step of the event-driven
//!   ingestion path). On successful materialise the blob becomes the
//!   instance's canonical original; on failure it is retained for reprocessing.
//!
//! These ride entirely on the existing [`LaneStore`](crate::LaneStore)
//! read/write/list primitives (default trait methods), so every backend
//! impl gets them for free.

use sha2::{Digest, Sha256};

/// Content-addressed blob identifier, rendered `sha256:<hex>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BlobId(String);

impl BlobId {
    /// Compute the id of some bytes (sha256).
    #[must_use]
    pub fn of(body: &[u8]) -> Self {
        let mut h = Sha256::new();
        h.update(body);
        let digest = h.finalize();
        let mut hex = String::with_capacity(64);
        for b in digest {
            hex.push_str(&format!("{b:02x}"));
        }
        Self(format!("sha256:{hex}"))
    }

    /// Parse a `sha256:<hex>` id, validating the shape. `None` otherwise.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        let hex = s.strip_prefix("sha256:")?;
        if hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            Some(Self(s.to_owned()))
        } else {
            None
        }
    }

    /// The full `sha256:<hex>` string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The hex digest without the `sha256:` prefix (the on-disk filename).
    #[must_use]
    pub fn hex(&self) -> &str {
        self.0.strip_prefix("sha256:").unwrap_or(&self.0)
    }
}

/// The canonical blob area prefix (under the tenant root).
pub const BLOB_PREFIX: &str = "blobs";
/// The inbox (staging) area prefix.
pub const INBOX_PREFIX: &str = "blobs/inbox";
