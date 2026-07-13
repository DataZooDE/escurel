//! Skill-pack bundling, signing, and verification (REQ-PACK-01/02).
//!
//! A pack is a **deterministic** gzip-framed tar of a skill subtree
//! (paths as `collect_pack_pages` emits them) plus a
//! [`PackManifest`] binding identity → bytes: `content_hash` is
//! `sha256:<hex>` over the tarball, `signature` is `sha256=<hex>`
//! HMAC-SHA256 with the shared pack secret (`ESCUREL_PACK_SECRET`)
//! over the canonical manifest-sans-signature JSON. The HMAC pattern
//! generalises ADR-0003's webhook signing to at-rest bundles: hub and
//! spokes are firm-operated, so a shared secret is the v1 trust model
//! (asymmetric signatures are a follow-on, not a prerequisite).
//!
//! Determinism matters: a pack is content-addressed, so re-exporting
//! unchanged content must yield byte-identical bundles (the rebuild /
//! derivability story, INV-DERIV). Entries are pre-sorted by the
//! collector; headers pin mtime/uid/gid to zero and mode to 0644.

use flate2::Compression;
use flate2::write::GzEncoder;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

use escurel_types::PackManifest;

type HmacSha256 = Hmac<Sha256>;

/// The pack tarball/manifest layout version. Bump on any layout change.
pub const PACK_FORMAT_VERSION: u32 = 1;

/// Build the deterministic gzip tarball of `pages` (`(relative path,
/// content)`, already sorted by path).
pub fn build_tarball(pages: &[(String, String)]) -> std::io::Result<Vec<u8>> {
    let gz = GzEncoder::new(Vec::new(), Compression::default());
    let mut tar = tar::Builder::new(gz);
    for (path, content) in pages {
        let bytes = content.as_bytes();
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
        header.set_cksum();
        tar.append_data(&mut header, path, bytes)?;
    }
    let gz = tar.into_inner()?;
    gz.finish()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(hash: &str, secret: &str) -> PackManifest {
        let mut m = PackManifest {
            format_version: PACK_FORMAT_VERSION,
            id: "logistics-midmarket".into(),
            version: 7,
            vertical: "logistics-midmarket".into(),
            publisher: "hub.test".into(),
            page_count: 1,
            content_hash: hash.into(),
            signature: String::new(),
        };
        m.signature = sign_manifest(&m, secret);
        m
    }

    #[test]
    fn round_trip_verifies() {
        let tarball = build_tarball(&[("skills/a.md".into(), "---\nid: a\n---\n".into())]).unwrap();
        let m = manifest(&content_hash(&tarball), "s3cret");
        assert_eq!(verify_pack(&m, &tarball, "s3cret"), Ok(()));
    }

    #[test]
    fn tampered_byte_fails_closed() {
        // AT-PACK-2 at the function boundary; the end-to-end import path
        // re-covers this over `/mcp` in the subscribe/import PR.
        let tarball = build_tarball(&[("skills/a.md".into(), "---\nid: a\n---\n".into())]).unwrap();
        let m = manifest(&content_hash(&tarball), "s3cret");
        let mut tampered = tarball.clone();
        let mid = tampered.len() / 2;
        tampered[mid] ^= 0x01;
        let err = verify_pack(&m, &tampered, "s3cret").unwrap_err();
        assert!(err.contains("pack_signature_invalid"), "{err}");
    }

    #[test]
    fn forged_manifest_fails_closed() {
        // Attacker rewrites content_hash to match a tampered bundle but
        // cannot re-sign without the secret.
        let tarball = build_tarball(&[("skills/a.md".into(), "---\nid: a\n---\n".into())]).unwrap();
        let mut tampered = tarball.clone();
        tampered[0] ^= 0x01;
        let mut m = manifest(&content_hash(&tarball), "s3cret");
        m.content_hash = content_hash(&tampered);
        let err = verify_pack(&m, &tampered, "s3cret").unwrap_err();
        assert!(err.contains("does not verify"), "{err}");
    }

    #[test]
    fn wrong_secret_fails_closed() {
        let tarball = build_tarball(&[("skills/a.md".into(), "x".into())]).unwrap();
        let m = manifest(&content_hash(&tarball), "s3cret");
        assert!(verify_pack(&m, &tarball, "other").is_err());
    }

    #[test]
    fn tarball_is_deterministic() {
        let pages = vec![
            ("instances/a/x.md".to_owned(), "one".to_owned()),
            ("skills/a.md".to_owned(), "two".to_owned()),
        ];
        assert_eq!(
            build_tarball(&pages).unwrap(),
            build_tarball(&pages).unwrap()
        );
    }
}
