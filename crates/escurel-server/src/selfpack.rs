//! Self-packaging bundle codec (ADR-0011).
//!
//! Folds a markdown corpus into a copy of the `escurel-server` binary:
//! a deterministic `tar.gz` (via [`crate::pack::build_tarball`], which
//! pins tar metadata so the output is byte-reproducible) appended after
//! the executable's EOF, followed by a fixed 16-byte trailer
//! (`MAGIC` + `u64`-LE bundle length). At startup the binary reads its
//! own image and recovers the bundle by seeking back from the tail; no
//! trailer ⇒ no bundle ⇒ today's behaviour, unchanged.
//!
//! This module is the pure codec — build / append / read / unpack /
//! list, with the pack secret-scrub wired in. The `pack`/`unpack`/`info`
//! CLI and the first-boot seed hook wire it into the binary (ADR-0011
//! increment 2).

use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;

/// Trailer magic — the last 16 bytes of a bundled binary are
/// `MAGIC (8) || bundle_len: u64 LE (8)`.
pub const MAGIC: &[u8; 8] = b"ESCPACK1";
const TRAILER_LEN: usize = 16;

#[derive(Debug, thiserror::Error)]
pub enum SelfpackError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("corpus file `{0}` is not UTF-8 text (the bundle carries a markdown corpus)")]
    NotUtf8(String),
    #[error("refusing to pack: {0}")]
    Secret(String),
}

/// Recursively collect `(lane-relative path, UTF-8 content)` for every
/// file under `dir`, sorted by path (the order `build_tarball` expects).
fn collect_corpus(dir: &Path) -> Result<Vec<(String, String)>, SelfpackError> {
    fn walk(root: &Path, cur: &Path, out: &mut Vec<(String, String)>) -> Result<(), SelfpackError> {
        for entry in std::fs::read_dir(cur)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out)?;
            } else if path.is_file() {
                let rel = path
                    .strip_prefix(root)
                    .expect("child of root")
                    .to_string_lossy()
                    .replace('\\', "/");
                let bytes = std::fs::read(&path)?;
                let content =
                    String::from_utf8(bytes).map_err(|_| SelfpackError::NotUtf8(rel.clone()))?;
                out.push((rel, content));
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out)?;
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// Build the deterministic bundle (`tar.gz`) of the corpus at `dir`.
///
/// Every file is run through the pack secret scrubber
/// ([`escurel_index::pack::pack_scrub_rejection`]); a credential-shaped
/// hit refuses the whole pack unless `allow_secrets` (tests only).
pub fn build_bundle(dir: &Path, allow_secrets: bool) -> Result<Vec<u8>, SelfpackError> {
    let pages = collect_corpus(dir)?;
    if !allow_secrets {
        for (path, content) in &pages {
            if let Some(reason) = escurel_index::pack::pack_scrub_rejection(path, content) {
                return Err(SelfpackError::Secret(reason));
            }
        }
    }
    Ok(crate::pack::build_tarball(&pages)?)
}

/// `base_exe || bundle || MAGIC || bundle_len(u64 LE)`.
#[must_use]
pub fn append_bundle(base_exe: &[u8], bundle: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(base_exe.len() + bundle.len() + TRAILER_LEN);
    out.extend_from_slice(base_exe);
    out.extend_from_slice(bundle);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(bundle.len() as u64).to_le_bytes());
    out
}

/// Recover the appended bundle from a binary image, or `None` when the
/// binary carries no (valid, in-bounds) bundle — the fallback path.
#[must_use]
pub fn read_bundle(exe: &[u8]) -> Option<&[u8]> {
    if exe.len() < TRAILER_LEN {
        return None;
    }
    let (body, trailer) = exe.split_at(exe.len() - TRAILER_LEN);
    if &trailer[..8] != MAGIC {
        return None;
    }
    let len = u64::from_le_bytes(trailer[8..16].try_into().ok()?) as usize;
    // Fail-safe on a truncated / corrupt tail: the claimed length must fit.
    body.len().checked_sub(len).map(|start| &body[start..])
}

/// The base executable image with any appended bundle+trailer stripped —
/// so re-`pack`ing an already-bundled binary replaces the bundle rather
/// than nesting one. Returns the whole image when there is no bundle.
#[must_use]
pub fn base_image(exe: &[u8]) -> &[u8] {
    match read_bundle(exe) {
        Some(bundle) => &exe[..exe.len() - TRAILER_LEN - bundle.len()],
        None => exe,
    }
}

/// Read the running executable and return its bundle, if any.
pub fn bundle_in_current_exe() -> Result<Option<Vec<u8>>, SelfpackError> {
    let path = std::env::current_exe()?;
    let bytes = std::fs::read(path)?;
    Ok(read_bundle(&bytes).map(<[u8]>::to_vec))
}

/// Extract a bundle's corpus into `dest` (created if absent).
pub fn unpack(bundle: &[u8], dest: &Path) -> Result<(), SelfpackError> {
    std::fs::create_dir_all(dest)?;
    let mut archive = tar::Archive::new(GzDecoder::new(bundle));
    archive.unpack(dest)?;
    Ok(())
}

/// List the bundle's entries as `(path, size_bytes)` — for `info`.
pub fn list_bundle(bundle: &[u8]) -> Result<Vec<(String, u64)>, SelfpackError> {
    let mut archive = tar::Archive::new(GzDecoder::new(bundle));
    let mut out = Vec::new();
    for entry in archive.entries()? {
        let entry = entry?;
        let size = entry.header().size()?;
        let path: PathBuf = entry.path()?.into_owned();
        out.push((path.to_string_lossy().replace('\\', "/"), size));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(dir: &Path, rel: &str, body: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    fn sample_corpus() -> TempDir {
        let d = TempDir::new().unwrap();
        write(
            d.path(),
            "skills/goal.md",
            "---\ntype: skill\nid: goal\n---\n# goal\n",
        );
        write(
            d.path(),
            "instances/goal/g1.md",
            "---\ntype: instance\nskill: goal\nid: g1\n---\n# g1\n",
        );
        d
    }

    #[test]
    fn bundle_round_trips_through_a_fake_binary() {
        let corpus = sample_corpus();
        let bundle = build_bundle(corpus.path(), false).unwrap();

        // Append to a stand-in "binary" and recover it from the tail.
        let fake_exe = b"\x7fELF ... pretend this is escurel-server ...";
        let bundled = append_bundle(fake_exe, &bundle);
        assert_eq!(read_bundle(&bundled), Some(bundle.as_slice()));
        // The base image is untouched at the front.
        assert!(bundled.starts_with(fake_exe));

        // Unpack and confirm the corpus comes back verbatim.
        let out = TempDir::new().unwrap();
        unpack(read_bundle(&bundled).unwrap(), out.path()).unwrap();
        let got = std::fs::read_to_string(out.path().join("skills/goal.md")).unwrap();
        assert!(got.contains("id: goal"), "{got}");
        assert!(out.path().join("instances/goal/g1.md").exists());
    }

    #[test]
    fn a_binary_without_a_trailer_has_no_bundle() {
        assert_eq!(
            read_bundle(b"an ordinary unbundled escurel-server image"),
            None
        );
        assert_eq!(read_bundle(b"tiny"), None);
    }

    #[test]
    fn list_reports_the_entries() {
        let corpus = sample_corpus();
        let bundle = build_bundle(corpus.path(), false).unwrap();
        let entries: Vec<String> = list_bundle(&bundle)
            .unwrap()
            .into_iter()
            .map(|(p, _)| p)
            .collect();
        assert!(entries.iter().any(|p| p == "skills/goal.md"), "{entries:?}");
        assert!(
            entries.iter().any(|p| p == "instances/goal/g1.md"),
            "{entries:?}"
        );
    }

    #[test]
    fn the_bundle_is_byte_reproducible() {
        let corpus = sample_corpus();
        let a = build_bundle(corpus.path(), false).unwrap();
        let b = build_bundle(corpus.path(), false).unwrap();
        assert_eq!(a, b, "same corpus must pack to identical bytes");
    }

    #[test]
    fn a_credential_shaped_file_refuses_the_pack() {
        let d = TempDir::new().unwrap();
        write(d.path(), "skills/ok.md", "# fine\n");
        write(
            d.path(),
            "instances/leak.md",
            "dsn: postgres://u:hunter2@db/prod\n",
        );
        let err = build_bundle(d.path(), false).unwrap_err();
        assert!(matches!(err, SelfpackError::Secret(_)), "{err:?}");
        // …and the override lets tests through.
        assert!(build_bundle(d.path(), true).is_ok());
    }
}
