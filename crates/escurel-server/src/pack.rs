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

// The pure signing/verification primitives live in escurel-types::pack
// (next to `PackManifest`) so offline consumers — the CLI's local
// `pack verify` — share one implementation without depending on this
// crate. Re-exported here so existing server code and tests keep the
// `crate::pack::verify_pack` spelling.
pub use escurel_types::pack::{content_hash, sign_manifest, verify_pack};

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

/// Decode a pack tarball into `(relative path, content)` entries,
/// validating every path fail-closed (agy review: the import side must
/// refuse zip-slip shapes even in a correctly-signed bundle):
/// no absolute paths, no `.`/`..` segments, no backslashes, no empty
/// segments, `.md` files only.
pub fn unpack_entries(bytes: &[u8]) -> Result<Vec<(String, String)>, String> {
    let gz = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(gz);
    let mut out = Vec::new();
    let entries = archive
        .entries()
        .map_err(|e| format!("pack_malformed: not a gzip tarball: {e}"))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| format!("pack_malformed: bad tar entry: {e}"))?;
        if entry.header().entry_type().is_dir() {
            continue;
        }
        if !entry.header().entry_type().is_file() {
            return Err("pack_malformed: non-regular tar entry (link/device) refused".to_owned());
        }
        let path = entry
            .path()
            .map_err(|e| format!("pack_malformed: undecodable entry path: {e}"))?
            .display()
            .to_string();
        if !is_safe_entry_path(&path) {
            return Err(format!(
                "pack_malformed: entry path `{path}` is not a safe relative .md path"
            ));
        }
        let mut content = String::new();
        std::io::Read::read_to_string(&mut entry, &mut content)
            .map_err(|e| format!("pack_malformed: entry `{path}` is not utf-8 text: {e}"))?;
        out.push((path, content));
    }
    Ok(out)
}

/// The entry-path predicate behind [`unpack_entries`]'s fail-closed
/// validation: relative, forward-slash-only, no `.`/`..`/empty
/// segments, `.md` files only. Kept as a pure function so the zip-slip
/// shapes are directly testable — the `tar` crate's own builder refuses
/// to *write* `..` paths, but an attacker's tarball isn't written by
/// our builder.
fn is_safe_entry_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && path.ends_with(".md")
        && path
            .split('/')
            .all(|seg| !seg.is_empty() && seg != "." && seg != "..")
}

/// Stamp `layer: <layer>` into a page's frontmatter for landing as a
/// base page. Fail-closed (agy review hardened this):
/// * a UTF-8 BOM is stripped; CRLF content is refused with an
///   actionable message (the canonical corpus is LF — the exporter
///   never emits CRLF, so this only fires on hand-built packs);
/// * the page must PARSE as escurel markdown (`escurel_md::parse`) —
///   so "frontmatter present" is decided by the real parser, not by
///   scanning for `\n---` (which a multi-line string value could
///   spoof), and a pre-declared `layer` key is found wherever YAML
///   puts it (quoted, indented, …), not by a `starts_with` scan;
/// * pack pages must be layer-free — layer is a property of where a
///   page sits, and the importer stamps it.
pub fn stamp_layer(content: &str, layer: &str) -> Result<String, String> {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    if content.contains('\r') {
        return Err(
            "pack_malformed: page uses CRLF line endings; the canonical corpus is \
             LF — normalise the pack contents"
                .to_owned(),
        );
    }
    let parsed = escurel_md::parse(content)
        .map_err(|e| format!("pack_malformed: page does not parse as escurel markdown: {e}"))?;
    if parsed.frontmatter.fields.get("layer").is_some() {
        return Err(
            "pack_malformed: page already declares a `layer:` key — pack pages are \
             layer-free (the importer stamps the layer)"
                .to_owned(),
        );
    }
    let Some(rest) = content.strip_prefix("---\n") else {
        return Err("pack_malformed: page has no frontmatter fence".to_owned());
    };
    Ok(format!("---\nlayer: {layer}\n{rest}"))
}

/// Whether `s` is a safe pack identity token (`id` / `vertical`):
/// lowercase alphanumerics plus `.`/`_`/`-`, non-empty, ≤ 64 chars,
/// starting alphanumeric. Load-bearing (agy review): the manifest id is
/// interpolated into the stamped `layer:` frontmatter line and into the
/// landing page-id prefix, so whitespace/newlines/slashes would smuggle
/// YAML keys or path segments.
#[must_use]
pub fn is_safe_pack_token(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
}

#[cfg(test)]
mod tests {
    use escurel_types::PackManifest;

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
    fn unpack_round_trips_and_rejects_non_md_entries() {
        let pages = vec![(
            "skills/a.md".to_owned(),
            "---\nid: a\n---\nbody\n".to_owned(),
        )];
        let tarball = build_tarball(&pages).unwrap();
        assert_eq!(unpack_entries(&tarball).unwrap(), pages);

        // Builder-constructible unsafe shape: wrong extension.
        let t = build_tarball(&[("note.txt".to_owned(), "x".to_owned())]).unwrap();
        assert!(unpack_entries(&t).unwrap_err().contains("pack_malformed"));
    }

    #[test]
    fn entry_path_predicate_rejects_zip_slip_shapes() {
        // The `tar` builder refuses to WRITE `..` paths, but an
        // attacker's tarball isn't written by our builder — the read
        // side must hold on its own.
        for evil in [
            "../escape.md",
            "/abs.md",
            "a/../b.md",
            "./a.md",
            "a//b.md",
            "a\\b.md",
            "note.txt",
            "",
        ] {
            assert!(!is_safe_entry_path(evil), "must reject: {evil:?}");
        }
        for fine in ["skills/a.md", "instances/pallet/edge-1.md", "a.md"] {
            assert!(is_safe_entry_path(fine), "must allow: {fine}");
        }
    }

    #[test]
    fn stamp_layer_inserts_once_and_refuses_predeclared() {
        let page = "---\ntype: skill\nid: a\n---\nbody\n";
        let stamped = stamp_layer(page, "base@p@v1").unwrap();
        assert!(
            stamped.starts_with("---\nlayer: base@p@v1\ntype: skill\nid: a\n---"),
            "{stamped}"
        );
        assert!(stamp_layer("no fence", "base@p@v1").is_err());
        assert!(
            stamp_layer(
                "---\ntype: skill\nlayer: overlay\nid: a\n---\n",
                "base@p@v1"
            )
            .is_err()
        );
    }

    #[test]
    fn stamp_layer_is_robust_to_the_review_shapes() {
        // agy MUST-FIX 1/2/4: BOM stripped; CRLF refused with a clear
        // message; a `layer` key found by the PARSER (indented / quoted
        // variants a `starts_with` scan missed); a `\n---` inside a
        // multi-line frontmatter string does NOT truncate the check.
        let bom = "\u{feff}---\ntype: skill\nid: a\n---\nbody\n";
        assert!(stamp_layer(bom, "base@p@v1").is_ok(), "BOM is stripped");

        let crlf = "---\r\ntype: skill\r\nid: a\r\n---\r\nbody\r\n";
        let err = stamp_layer(crlf, "base@p@v1").unwrap_err();
        assert!(err.contains("CRLF"), "{err}");

        let quoted_layer = "---\ntype: skill\nid: a\n\"layer\": overlay\n---\n";
        assert!(
            stamp_layer(quoted_layer, "base@p@v1").is_err(),
            "a quoted layer key is still a layer key"
        );

        let embedded_fence =
            "---\ntype: skill\nid: a\ndescription: |\n  looks like\n  ---\n  a fence\n---\nbody\n";
        let stamped = stamp_layer(embedded_fence, "base@p@v1").unwrap();
        assert!(stamped.contains("looks like"), "{stamped}");
    }

    #[test]
    fn pack_tokens_reject_injection_shapes() {
        for evil in [
            "evil\ninjected: true",
            "up/../and-out",
            "has space",
            "Uppercase",
            "",
            "-leading-dash",
        ] {
            assert!(!is_safe_pack_token(evil), "must reject: {evil:?}");
        }
        for fine in ["logistics-midmarket", "crm-core", "dental.v2", "a"] {
            assert!(is_safe_pack_token(fine), "must allow: {fine}");
        }
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
