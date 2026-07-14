//! Skill-pack wire types (REQ-PACK-01/02): the manifest identifying a
//! versioned, signed, tarball-serialised bundle of skill pages — the
//! Company Model's unit of distribution between a hub and its spokes —
//! plus the pure signing/verification primitives over it.
//!
//! The crypto lives HERE (next to [`PackManifest`]) rather than in
//! escurel-server so that offline consumers — the CLI's local
//! `pack verify`, a future hub tool — share one implementation without
//! pulling in the server. escurel-server re-exports these from its
//! `pack` module.

use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

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
/// `deny_unknown_fields` is load-bearing (agy review): the signature is
/// computed over the re-serialised struct, so silently *dropping*
/// unknown wire fields would let an attacker append arbitrary JSON to a
/// signed manifest without invalidating it. A manifest from a newer
/// format rejects on an old node instead — fail-closed, versioned via
/// `format_version`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
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

/// `sha256:<hex>` over the tarball bytes.
#[must_use]
pub fn content_hash(tarball: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(tarball))
}

/// The canonical signing payload: the manifest JSON with `signature`
/// emptied. Field order is fixed by the struct, so the bytes are
/// deterministic.
fn signing_payload(manifest: &PackManifest) -> Vec<u8> {
    let unsigned = PackManifest {
        signature: String::new(),
        ..manifest.clone()
    };
    serde_json::to_vec(&unsigned).expect("manifest serializes")
}

/// Sign `manifest` (its `signature` field is ignored) with the shared
/// pack secret: `sha256=<hex HMAC-SHA256>` over the canonical
/// manifest-sans-signature JSON.
#[must_use]
pub fn sign_manifest(manifest: &PackManifest, secret: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("hmac accepts any key len");
    mac.update(&signing_payload(manifest));
    format!("sha256={:x}", mac.finalize().into_bytes())
}

/// Verify a pack before trusting it (REQ-PACK-02, fail-closed): the
/// manifest signature must be authentic under `secret` and
/// `content_hash` must cover `tarball`. Returns the typed reason on
/// the first failure. Signature comparison is constant-time (the HMAC
/// verify), mirroring the webhook receiver.
pub fn verify_pack(manifest: &PackManifest, tarball: &[u8], secret: &str) -> Result<(), String> {
    let Some(sig_hex) = manifest.signature.strip_prefix("sha256=") else {
        return Err("pack_signature_invalid: manifest signature is not `sha256=<hex>`".to_owned());
    };
    let Ok(sig_bytes) = hex_decode(sig_hex) else {
        return Err("pack_signature_invalid: manifest signature is not valid hex".to_owned());
    };
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("hmac accepts any key len");
    mac.update(&signing_payload(manifest));
    if mac.verify_slice(&sig_bytes).is_err() {
        return Err(
            "pack_signature_invalid: manifest signature does not verify under the \
             configured pack secret"
                .to_owned(),
        );
    }
    // The signature covers content_hash, so only now is the hash trusted.
    let actual = content_hash(tarball);
    if actual != manifest.content_hash {
        return Err(format!(
            "pack_signature_invalid: tarball hash `{actual}` does not match the \
             signed manifest content_hash `{}` (bundle tampered or truncated)",
            manifest.content_hash
        ));
    }
    Ok(())
}

/// Lowercase/uppercase-hex decode without a new dependency.
fn hex_decode(s: &str) -> Result<Vec<u8>, ()> {
    if !s.len().is_multiple_of(2) {
        return Err(());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| ()))
        .collect()
}
