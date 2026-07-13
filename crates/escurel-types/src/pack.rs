//! Skill-pack wire types (REQ-PACK-01/02): the manifest identifying a
//! versioned, signed, tarball-serialised bundle of skill pages — the
//! Company Model's unit of distribution between a hub and its spokes.

use serde::{Deserialize, Serialize};

/// `export_pack` request: which subtree to bundle, under which pack
/// identity (REQ-PACK-01).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ExportPackRequest {
    /// Must match the gateway's served tenant.
    pub tenant_id: String,
    /// Pack identity, e.g. `logistics-midmarket`.
    pub id: String,
    /// Monotonic pack version.
    pub version: u32,
    /// The vertical this pack belongs to (REQ-PACK-03).
    pub vertical: String,
    /// Publisher identity, e.g. `hub.stuttgart-ai`.
    pub publisher: String,
    /// Skill ids whose pages form the pack subtree.
    pub skills: Vec<String>,
    /// Also bundle each skill's instance pages (edge-case libraries).
    pub include_instances: bool,
}

/// The pack manifest — identity, integrity, and trust for one pack
/// bundle. Serialised as `pack.manifest.json` next to the tarball on
/// disk and returned inline by `export_pack`.
///
/// Signing contract (REQ-PACK-02): `signature` is
/// `sha256=<hex HMAC-SHA256>` computed with the shared pack secret over
/// the canonical JSON bytes of this struct **with `signature` set to
/// the empty string** (field order is fixed by the struct, so the
/// payload is deterministic). `content_hash` is `sha256:<hex>` over the
/// tarball bytes, binding manifest to bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PackManifest {
    /// Manifest/tarball layout version. Bump on any layout change
    /// (mirrors `tenant_export`'s format version discipline).
    pub format_version: u32,
    /// Pack identity, e.g. `logistics-midmarket`.
    pub id: String,
    /// Monotonic pack version; a spoke pins `base@<id>@v<version>`.
    pub version: u32,
    /// The vertical this pack belongs to (REQ-PACK-03). Load-bearing:
    /// convergence holds only within a vertical, so an importer warns or
    /// refuses on a vertical mismatch.
    pub vertical: String,
    /// Publisher identity, e.g. `hub.stuttgart-ai`.
    pub publisher: String,
    /// Number of pages in the bundle.
    pub page_count: u32,
    /// `sha256:<hex>` over the tarball bytes.
    pub content_hash: String,
    /// `sha256=<hex>` HMAC-SHA256 over the manifest-sans-signature JSON.
    pub signature: String,
}
