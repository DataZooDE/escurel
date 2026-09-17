//! Resolving an operation's [`ResultRef`] to a safe, readable location
//! (async-ops Phase 4 / B′).
//!
//! A scenario what-if operation materialises its result as parquet on the data
//! store; escurel reads it back via the existing `read_parquet` connector. The
//! danger is the *reference*: a path or URL from a caller/harness could point at
//! `../../etc`, another tenant's data, or a remote `s3://`/`http://` endpoint.
//!
//! [`ResultRef`] is a closed enum with no path/URL field, so those are
//! unrepresentable at the type level; this module adds the runtime half:
//!
//! 1. **Bounded id.** The `scenario_id` must be a slug (`[A-Za-z0-9_.-]`, not
//!    `.`/`..`), so it cannot contain a path separator or a scheme's `:`.
//! 2. **Server-owned path.** The location is `<root>/<tenant>/<scenario_id>/`,
//!    derived by the server — never taken from the reference — and verified to
//!    stay under `root` (defence in depth against a surprising `join`).
//! 3. **Torn-publish gate.** The directory is only readable once its
//!    `manifest.json` is present. The producer writes parquet to a temp prefix
//!    then atomically publishes the files + the manifest, so a reader either
//!    sees the whole set (manifest present) or treats it as not-yet-ready — it
//!    never reads a half-written directory.
//!
//! Producing the parquet (the anofox-scenario harness) and the atomic publish
//! onto Hetzner Object Storage are the deploy/extension half, out of scope here;
//! this is the gateway's read-side validation.

use std::path::{Path, PathBuf};

pub use escurel_types::ResultRef;

/// The manifest file whose presence marks a result directory as fully published.
pub const RESULT_MANIFEST: &str = "manifest.json";

/// Why a [`ResultRef`] could not be resolved to a readable location.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResultRefError {
    /// The reference's id (or the tenant) is not a bounded slug — it could
    /// carry a path separator, `..`, or a scheme. Rejected before any fs touch.
    #[error("result_ref: invalid id (must be a bounded [A-Za-z0-9_.-] slug)")]
    InvalidId,
    /// The resolved directory escaped the data root (defence in depth).
    #[error("result_ref: resolved location escaped the data root")]
    OutsideRoot,
    /// The directory has no `manifest.json` yet — not published, or torn
    /// mid-publish. A reader must treat this as not-ready, never read it.
    #[error("result_ref: result not yet published (no manifest)")]
    NotReady,
}

/// A resolved, published result directory, safe to read parquet from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedResult {
    /// The absolute directory under the data root holding the result parquet.
    pub dir: PathBuf,
}

impl ResolvedResult {
    /// The `read_parquet` glob for this result's directory (`<dir>/*.parquet`).
    /// The directory is server-owned and validated, so the glob is safe to
    /// splice into a `read_parquet('…')` call.
    #[must_use]
    pub fn parquet_glob(&self) -> String {
        format!("{}/*.parquet", self.dir.display())
    }
}

/// A bounded result/tenant id: 1-128 chars of `[A-Za-z0-9_.-]`, and not `.` or
/// `..`. Because `/` and `:` are excluded, a path traversal (`../x`) or a
/// remote scheme (`s3://…`) can never pass.
fn is_bounded_id(s: &str) -> bool {
    if s.is_empty() || s.len() > 128 || s == "." || s == ".." {
        return false;
    }
    s.bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

/// Resolve a [`ResultRef`] under `root` for `tenant` to a readable location, or
/// an error. Validates the id, derives a server-owned path that must stay under
/// `root`, and enforces the torn-publish manifest gate. Performs no read.
pub fn resolve_result_ref(
    root: &Path,
    tenant: &str,
    result_ref: &ResultRef,
) -> Result<ResolvedResult, ResultRefError> {
    // Every variant resolves to a server-owned directory under `root`; the only
    // difference is how many bounded segments name it. Validate them all, build
    // the path from validated segments, then apply the shared root + manifest
    // gates once.
    let segments: Vec<&str> = match result_ref {
        ResultRef::ScenarioParquet { scenario_id } => vec![scenario_id.as_str()],
        ResultRef::ResultTable { producer, id } => vec![producer.as_str(), id.as_str()],
    };
    if !is_bounded_id(tenant) || !segments.iter().all(|s| is_bounded_id(s)) {
        return Err(ResultRefError::InvalidId);
    }
    let mut dir = root.join(tenant);
    for seg in segments {
        dir.push(seg);
    }
    // Defence in depth: the server built this path from validated segments,
    // but assert it did not escape `root` regardless.
    if !dir.starts_with(root) {
        return Err(ResultRefError::OutsideRoot);
    }
    if !dir.join(RESULT_MANIFEST).is_file() {
        return Err(ResultRefError::NotReady);
    }
    Ok(ResolvedResult { dir })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scn(id: &str) -> ResultRef {
        ResultRef::ScenarioParquet {
            scenario_id: id.to_owned(),
        }
    }

    fn tbl(producer: &str, id: &str) -> ResultRef {
        ResultRef::ResultTable {
            producer: producer.to_owned(),
            id: id.to_owned(),
        }
    }

    /// The generalized `ResultTable{producer,id}` variant (Phase-4 slice 3c):
    /// any producer (scenario/forecast/…) names its result by a bounded id
    /// under a producer-namespaced dir. It is validated and gated EXACTLY like
    /// `ScenarioParquet` — a bad producer or id is rejected before any fs touch,
    /// and the torn-publish manifest gate still holds.
    #[test]
    fn a_result_table_resolves_under_a_producer_namespace_and_is_traversal_safe() {
        let root = tempfile::tempdir().expect("tempdir");
        // A forged producer, id, or tenant is rejected before touching the fs.
        for (p, id) in [
            ("../x", "r1"),
            ("scenario", "../r"),
            ("s3:", "r1"),
            ("", "r1"),
            ("scn", ""),
        ] {
            assert_eq!(
                resolve_result_ref(root.path(), "acme", &tbl(p, id)),
                Err(ResultRefError::InvalidId),
                "producer={p:?} id={id:?} must be rejected"
            );
        }
        assert_eq!(
            resolve_result_ref(root.path(), "../other", &tbl("scenario", "r1")),
            Err(ResultRefError::InvalidId)
        );

        // A well-formed reference to an unpublished dir is not-ready (gate holds).
        let dir = root.path().join("acme").join("scenario").join("r1");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("part-0.parquet"), b"x").unwrap();
        assert_eq!(
            resolve_result_ref(root.path(), "acme", &tbl("scenario", "r1")),
            Err(ResultRefError::NotReady)
        );
        // The manifest opens the gate; the glob stays under the root, namespaced.
        std::fs::write(dir.join(RESULT_MANIFEST), b"{}").unwrap();
        let resolved =
            resolve_result_ref(root.path(), "acme", &tbl("scenario", "r1")).expect("resolves");
        assert_eq!(resolved.dir, dir);
        assert!(resolved.parquet_glob().ends_with("/scenario/r1/*.parquet"));
        assert!(resolved.dir.starts_with(root.path()));
    }

    #[test]
    fn a_traversal_or_scheme_id_is_rejected_before_touching_the_fs() {
        let root = Path::new("/nonexistent-root");
        for bad in ["../etc", "..", ".", "a/b", "s3://bucket", "x:y", ""] {
            assert_eq!(
                resolve_result_ref(root, "acme", &scn(bad)),
                Err(ResultRefError::InvalidId),
                "id {bad:?} must be rejected"
            );
        }
        // A forged tenant is rejected the same way.
        assert_eq!(
            resolve_result_ref(root, "../other", &scn("scn1")),
            Err(ResultRefError::InvalidId)
        );
    }

    #[test]
    fn a_directory_without_a_manifest_is_not_ready_torn_publish_gate() {
        let root = tempfile::tempdir().expect("tempdir");
        let dir = root.path().join("acme").join("scn1");
        std::fs::create_dir_all(&dir).unwrap();
        // A parquet-ish file exists but the manifest does NOT — a torn/partial
        // publish. The reader must refuse, not read a half-written set.
        std::fs::write(dir.join("part-0.parquet"), b"not-real-parquet").unwrap();
        assert_eq!(
            resolve_result_ref(root.path(), "acme", &scn("scn1")),
            Err(ResultRefError::NotReady)
        );
    }

    /// The full round-trip: a published result directory (real parquet + a
    /// manifest) resolves and its glob reads back through DuckDB's
    /// `read_parquet` — the "escurel reads the what-if result back as a
    /// proposal" half of the Phase-4 DoD (the anofox-scenario harness that
    /// PRODUCES the parquet + the atomic Object-Storage publish are the
    /// deploy/extension half; here a test writes a stub parquet).
    #[test]
    fn escurel_reads_the_published_parquet_back() {
        let root = tempfile::tempdir().expect("tempdir");
        let dir = root.path().join("acme").join("scn2");
        std::fs::create_dir_all(&dir).unwrap();
        let conn = duckdb::Connection::open_in_memory().expect("duckdb");
        let part = dir.join("part-0.parquet");
        conn.execute_batch(&format!(
            "COPY (SELECT * FROM (VALUES (1, 'a'), (2, 'b')) t(id, name)) \
             TO '{}' (FORMAT parquet);",
            part.display()
        ))
        .expect("write parquet");
        // The manifest is written LAST — the torn-publish gate opens only now.
        std::fs::write(dir.join(RESULT_MANIFEST), b"{\"rows\":2}").unwrap();

        let resolved = resolve_result_ref(root.path(), "acme", &scn("scn2")).expect("resolves");
        let n: i64 = conn
            .query_row(
                &format!(
                    "SELECT count(*) FROM read_parquet('{}')",
                    resolved.parquet_glob()
                ),
                [],
                |r| r.get(0),
            )
            .expect("read_parquet");
        assert_eq!(n, 2, "escurel reads the published scenario parquet back");
    }

    #[test]
    fn a_published_directory_resolves_to_a_safe_glob() {
        let root = tempfile::tempdir().expect("tempdir");
        let dir = root.path().join("acme").join("scn1");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("part-0.parquet"), b"x").unwrap();
        std::fs::write(dir.join(RESULT_MANIFEST), b"{}").unwrap();
        let resolved = resolve_result_ref(root.path(), "acme", &scn("scn1")).expect("resolves");
        assert_eq!(resolved.dir, dir);
        assert!(resolved.parquet_glob().ends_with("/scn1/*.parquet"));
        assert!(resolved.dir.starts_with(root.path()));
    }
}
