//! In-memory Loro CRDT engine + `LiveDoc` actor for escurel M4.
//!
//! This crate is the persistence boundary for live-collaboration
//! sessions on a page. A [`LiveDoc`] owns one Tokio task that wraps
//! a [`loro::LoroDoc`] and serialises every op through an mpsc
//! channel, so multiple concurrent
//! `open_session` / `apply_op` callers per page can never race the
//! in-memory engine. Each accepted op is mirrored into the
//! `crdt_ops` DuckDB table; closing the doc with `commit=true`
//! takes a snapshot row in `crdt_snapshots`.
//!
//! The DuckDB schema lives in `escurel-index`'s [`Migrator::up`];
//! see `docs/spec/storage.md §CRDT persistence` (and the inline
//! comments on [`DuckdbCrdtBackend`]). The transport layer that
//! routes `open_session` / `apply_op` / `close_session` to a
//! [`LiveDoc`] lands in a later PR (M4.2+).
//!
//! ## Locked decisions
//!
//! * **Version derivation: op count for v1.** The protocol's
//!   `merged_version` / `head_version` / `final_version` strings
//!   are `"v<n>"`, mirroring what `update_page` already returns
//!   (see `escurel-server::tools::update_page`). Real HLC versions
//!   arrive in M4.6.
//! * **No mocks at the boundary.** Tests run a real Loro engine
//!   against a real DuckDB file via `Arc<Mutex<Connection>>`.

pub mod backend;
pub mod codec;
pub mod error;
pub mod livedoc;
pub mod pg;
pub mod reconciler;

pub use backend::{CrdtBackend, DuckdbCrdtBackend, OpAuthor};
pub use codec::{body_from_snapshot, snapshot_bytes_from_markdown, three_way_merge};
pub use error::Error;
pub use livedoc::{LiveDoc, hydrate_content};
pub use pg::{
    CRDT_OPS_PG_TABLE, CRDT_PG_ALIAS, CRDT_SNAPSHOTS_PG_TABLE, add_crdt_ops_pg_principal_sql,
    attach_crdt_pg, attach_crdt_pg_sql, create_crdt_ops_pg_table_sql,
    create_crdt_snapshots_pg_table_sql,
};
pub use reconciler::{CitationLookup, Decision, ExternalEditReconciler};

/// Raw Loro op bytes — the wire payload that `apply_op` / `open_session`
/// shuttle around — together with the principal that submitted them.
///
/// The bytes are opaque from this crate's perspective: we forward them to
/// [`loro::LoroDoc::import`].
///
/// # Why the principal rides here rather than being read out of the bytes
///
/// Loro op bytes embed a **peer id**, and a peer id identifies a *device*:
/// one browser tab, one CLI process. Two ops emitted by the same tab under
/// two different tokens carry the same peer id and have two different
/// authors, so no amount of decoding the payload answers "who edited this"
/// (escurel#357 / CR-6). The answer is only available at the gateway, from
/// the verified token — so it is attached to the op there and travels with
/// it to the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Op {
    bytes: Vec<u8>,
    principal: Option<String>,
}

impl Op {
    /// Construct a new `Op` from raw bytes, with no recorded author.
    ///
    /// Callers that know the verified principal — every gateway write path —
    /// chain [`Op::by`].
    #[must_use]
    pub fn new(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            principal: None,
        }
    }

    /// Attribute this op to `principal`, the subject the gateway verified.
    #[must_use]
    pub fn by(mut self, principal: impl Into<String>) -> Self {
        self.principal = Some(principal.into());
        self
    }

    /// The principal that submitted this op, if the submitting path knew one.
    #[must_use]
    pub fn principal(&self) -> Option<&str> {
        self.principal.as_deref()
    }

    /// Borrow the underlying bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Consume into the underlying byte buffer.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

impl From<Vec<u8>> for Op {
    fn from(v: Vec<u8>) -> Self {
        Self::new(v)
    }
}

/// Raw Loro snapshot bytes (output of
/// [`loro::LoroDoc::export`] with `ExportMode::Snapshot`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot(pub Vec<u8>);

impl Snapshot {
    /// Construct a new `Snapshot` from raw bytes.
    #[must_use]
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// Borrow the underlying bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl From<Vec<u8>> for Snapshot {
    fn from(v: Vec<u8>) -> Self {
        Self(v)
    }
}

/// A page version string of the form `"v<n>"`.
///
/// For v1 we derive `n` from the op count rather than a real HLC
/// (see crate docs). The type is a newtype rather than `String` so
/// downstream code calls [`Version::as_str`] explicitly — it must
/// not be conflated with arbitrary user-supplied strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version(String);

impl Version {
    /// Build a version from the op-count `n`. `n == 0` is `"v0"`
    /// (the empty initial state); the first accepted op produces
    /// `"v1"`.
    #[must_use]
    pub fn from_op_count(n: u64) -> Self {
        Self(format!("v{n}"))
    }

    /// Borrow the version as `&str` for logging or wire formats.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Parse the numeric op-count back out of a `"v<n>"` string — the
    /// inverse of [`Version::from_op_count`]. Returns `None` for anything
    /// that isn't `v` followed by a non-negative integer. The
    /// `update_page` auto-merge (#246) uses this to map a client's
    /// `base_version` back to the hlc whose snapshot it branched from.
    #[must_use]
    pub fn parse_op_count(s: &str) -> Option<u64> {
        s.strip_prefix('v').and_then(|n| n.parse::<u64>().ok())
    }
}

impl From<Version> for String {
    fn from(v: Version) -> Self {
        v.0
    }
}
