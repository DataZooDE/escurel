//! [`LaneStore`] over DuckDB's virtual filesystem (feature `duckvfs`).
//!
//! Unlike [`crate::gcs::GcsStore`] and [`crate::s3::S3Store`], which each
//! speak one provider's API, this backend speaks none. It hands paths to
//! DuckDB and lets DuckDB's VFS dispatch on the scheme, so a single
//! implementation covers `gdrive://`, `s3://`, `gs://`, `hf://` and plain
//! local paths — and any filesystem a future extension registers, at no
//! cost here.
//!
//! Google Drive is why it exists. Drive has no S3-compatible surface and no
//! Rust client worth carrying, but `duckdb-gdrive` already implements a
//! complete `FileSystem` over it, including the shared-drive and
//! export-on-read behaviour that makes Drive awkward. Riding that is
//! strictly less code than a native Drive client would be, and it is code
//! that already has tests against the live API.
//!
//! # What this needs from the extension
//!
//! DuckDB's SQL surface can read bytes (`read_blob`) and enumerate
//! (`glob`) but cannot write, delete, rename, or stat a file — `FileSystem`
//! has always had those operations, they were simply never exposed. The
//! `duckdb-gdrive` extension registers the missing four as scheme-generic
//! SQL functions, and this module is their first consumer:
//!
//! | operation | SQL                        | origin              |
//! |-----------|----------------------------|---------------------|
//! | read      | `read_blob(path)`          | DuckDB core         |
//! | list      | `glob(pattern)`            | DuckDB core         |
//! | write     | `write_blob(path, blob)`   | duckdb-gdrive       |
//! | delete    | `remove_file(path)`        | duckdb-gdrive       |
//! | rename    | `move_file(src, dst)`      | duckdb-gdrive       |
//! | size      | `file_size(path)`          | duckdb-gdrive       |
//!
//! # Semantics that differ from the other backends
//!
//! **`Version` is a content hash, not a store version.** [`crate::fs::FsStore`]
//! returns an mtime and `GcsStore` returns the object generation. The VFS
//! exposes neither, so `write` returns `sha256:<hex>` of the bytes it wrote.
//! Callers treat [`Version`] as opaque, and the equality check they use it
//! for still holds — but it identifies *content*, so two writes of identical
//! bytes share a version where the other backends would produce two.
//!
//! **Atomicity is per-scheme.** The trait asks for write-then-publish. On
//! `gdrive://` a single `write_blob` is already atomic: the Drive handle
//! uploads on close and publishes a new revision only once the upload
//! completes, so a reader sees the old bytes or the new ones, never a
//! partial file. On a local path it is *not* — `write_blob` truncates in
//! place. Local paths are a test convenience here (the real backend for
//! them is `FsStore`, which does rename properly), so this is a documented
//! limitation rather than one worth paying a temp-file round trip on Drive
//! to close.
//!
//! # Concurrency
//!
//! `duckdb::Connection` is `Send` but not `Sync`, and every call blocks. So
//! the connection lives behind a `Mutex` and every trait method hops through
//! `spawn_blocking`. One connection is deliberate rather than a pool: Drive
//! round trips dominate by orders of magnitude, and `duckdb-gdrive`'s block
//! and path caches are per-`DatabaseInstance` — sharing one instance is what
//! makes those caches effective.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bytes::Bytes;
use duckdb::Connection;
use duckdb::types::Value;
use sha2::{Digest, Sha256};
use url::Url;

use crate::{Key, LaneStore, Result, StoreError, Version};

/// Everything needed to reach a filesystem through DuckDB.
#[derive(Debug, Clone, Default)]
pub struct DuckVfsStoreConfig {
    /// Root URL every key hangs off, e.g. `gdrive://escurel/lanes` or
    /// `file:///var/lib/escurel`. The scheme decides which DuckDB
    /// filesystem serves the request; no trailing slash is required.
    pub root: String,
    /// Filesystem path to the `gdrive.duckdb_extension` to `LOAD`. Required
    /// for a `gdrive://` root and ignored otherwise — but note that the
    /// `write_blob`/`remove_file`/`move_file`/`file_size` functions live in
    /// that extension, so **every** root needs it loaded until they ship in
    /// DuckDB core. `None` falls back to [`Self::extension_repo`]; if that
    /// is `None` too the `LOAD` is skipped and they are assumed present.
    pub extension_path: Option<String>,
    /// Where to fetch the extension when [`Self::extension_path`] is unset:
    /// `"community"` for the DuckDB community repository, or a repository
    /// URL such as `http://get.erpl.io`.
    ///
    /// This is what makes a `duckvfs` store deployable at all. A path names
    /// a locally built file, which is fine on a developer's machine and
    /// impossible in a container — and falling through to `None` is worse
    /// than it looks: it skips the load rather than failing, so the store
    /// opens cleanly and the first WRITE fails on a missing function.
    ///
    /// Ignored when `extension_path` is set, so an operator pointing at a
    /// local build always gets that build.
    pub extension_repo: Option<String>,
    /// Shared Drive id (`0A…`) for a `gdrive://` root. Becomes the secret's
    /// `DRIVE_ID`, which both roots the path and scopes every listing to
    /// that drive.
    pub drive_id: Option<String>,
    /// OAuth scope for the Drive secret. Defaults to read/write `drive`;
    /// the extension's own default is the narrower `drive.readonly`, which
    /// cannot serve a lane store.
    pub drive_scope: Option<String>,
}

/// A [`LaneStore`] over any filesystem DuckDB can reach.
pub struct DuckVfsStore {
    conn: Arc<Mutex<Connection>>,
    root: String,
}

/// The DuckDB community repository, named rather than spelled as a URL.
pub const COMMUNITY_REPO: &str = "community";

/// Default OAuth scope. `drive.readonly` — the extension's default — is not
/// enough for a store that has to write.
const DEFAULT_DRIVE_SCOPE: &str = "https://www.googleapis.com/auth/drive";

impl DuckVfsStore {
    /// Open an in-memory DuckDB, load the extension, and register the
    /// credential for `root`'s scheme.
    ///
    /// The database is in-memory and holds no state of its own: it is a
    /// handle onto the VFS, not a store. Credentials come from Application
    /// Default Credentials, so this performs no network I/O — a bad
    /// credential or an unreachable root surfaces on first use.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Io`] if DuckDB cannot open, the extension
    /// cannot be loaded, or the secret is rejected.
    pub fn new(cfg: &DuckVfsStoreConfig) -> Result<Self> {
        // Unsigned extensions must be permitted BEFORE the connection is
        // made: a locally-built .duckdb_extension carries no signature, and
        // the flag is read at database-open time, not at LOAD time.
        let config = duckdb::Config::default()
            .allow_unsigned_extensions()
            .map_err(|e| duck_err("config", e))?;
        let conn =
            Connection::open_in_memory_with_flags(config).map_err(|e| duck_err("open", e))?;

        if let Some(path) = &cfg.extension_path {
            // A path, not a name: LOAD '<file>' takes the local build
            // directly, and wins over a repository so an operator pointing
            // at a build always gets that build.
            conn.execute_batch(&format!("LOAD '{}';", escape_sql(path)))
                .map_err(|e| duck_err("load extension", e))?;
        } else if let Some(repo) = &cfg.extension_repo {
            // `community` is a KEYWORD in DuckDB's grammar and must not be
            // quoted; a repository URL must be. Quoting the keyword makes
            // DuckDB look for a repository literally named "community" and
            // fail with a message that says nothing about quoting.
            let install = if repo == COMMUNITY_REPO {
                "INSTALL gdrive FROM community;".to_owned()
            } else {
                format!("INSTALL gdrive FROM '{}';", escape_sql(repo))
            };
            conn.execute_batch(&format!("{install} LOAD gdrive;"))
                .map_err(|e| duck_err("install extension", e))?;
        }

        if cfg.root.starts_with("gdrive://") {
            conn.execute_batch(&gdrive_secret_sql(cfg))
                .map_err(|e| duck_err("create gdrive secret", e))?;
        }

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            root: cfg.root.trim_end_matches('/').to_owned(),
        })
    }

    /// Absolute URL for `key`. Layout matches [`crate::gcs::GcsStore`] and
    /// [`crate::s3::S3Store`] exactly — `{root}/tenants/{tenant}/{path}` —
    /// so a corpus is readable by any of the three and migration between
    /// them is a config change.
    fn object_url(&self, key: &Key) -> String {
        format!("{}/tenants/{}/{}", self.root, key.tenant(), key.path())
    }

    fn tenant_prefix(&self, tenant: &str) -> String {
        format!("{}/tenants/{}", self.root, tenant)
    }

    /// Run `f` against the connection on the blocking pool.
    async fn with_conn<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> Result<T> + Send + 'static,
    {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || {
            // Poisoning means a previous caller panicked mid-query. The
            // connection itself is still usable, so recover rather than
            // propagate a panic that has nothing to do with this caller.
            let guard = conn.lock().unwrap_or_else(|e| e.into_inner());
            f(&guard)
        })
        .await
        .map_err(|e| StoreError::Io(std::io::Error::other(e)))?
    }
}

#[async_trait]
impl LaneStore for DuckVfsStore {
    async fn read(&self, key: &Key) -> Result<Bytes> {
        let url = self.object_url(key);
        let missing = key.clone();
        self.with_conn(move |conn| {
            // read_blob raises on a missing file rather than returning zero
            // rows, so not-found arrives as an error to be classified.
            let mut stmt = conn
                .prepare("SELECT content FROM read_blob(?)")
                .map_err(|e| duck_err("prepare read", e))?;
            let mut rows = stmt
                .query([&url])
                .map_err(|e| classify_missing(e, &missing, "read"))?;
            match rows
                .next()
                .map_err(|e| classify_missing(e, &missing, "read"))?
            {
                Some(row) => {
                    let blob: Vec<u8> = row.get(0).map_err(|e| duck_err("decode blob", e))?;
                    Ok(Bytes::from(blob))
                }
                None => Err(StoreError::NotFound(missing)),
            }
        })
        .await
    }

    async fn write(&self, key: &Key, body: Bytes) -> Result<Version> {
        let url = self.object_url(key);
        let version = content_version(&body);
        self.with_conn(move |conn| {
            conn.execute(
                "SELECT write_blob(?, ?)",
                duckdb::params![&url, Value::Blob(body.to_vec())],
            )
            .map_err(|e| duck_err("write", e))?;
            Ok(version)
        })
        .await
    }

    async fn list(&self, prefix: &Key) -> Result<Vec<Key>> {
        let tenant = prefix.tenant().to_owned();
        // `**` is DuckDB's recursive glob. The tenant root is stripped back
        // off each hit below to recover the key's path.
        let base = self.tenant_prefix(&tenant);
        let pattern = format!("{base}/**");
        let want = prefix.path().to_owned();

        self.with_conn(move |conn| {
            let mut stmt = conn
                .prepare("SELECT file FROM glob(?)")
                .map_err(|e| duck_err("prepare list", e))?;
            let rows: Vec<String> = match stmt.query_map([&pattern], |row| row.get::<_, String>(0))
            {
                Ok(it) => it
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(|e| duck_err("list", e))?,
                // An empty tenant is not an error: glob on a directory that
                // does not exist raises on some filesystems and returns
                // nothing on others, and `list` is specified to return an
                // empty vec. Only that case is swallowed — a real failure
                // above still propagates.
                Err(_) => Vec::new(),
            };

            Ok(rows
                .iter()
                .filter_map(|hit| {
                    let rel = strip_base(hit, &base)?;
                    if !rel.starts_with(&want) {
                        return None;
                    }
                    // A path that fails Key validation is skipped rather
                    // than poisoning the whole listing — same defensive
                    // stance as FsStore's walk.
                    Key::new(tenant.clone(), rel).ok()
                })
                .collect())
        })
        .await
    }

    async fn delete(&self, key: &Key) -> Result<()> {
        let url = self.object_url(key);
        let missing = key.clone();
        self.with_conn(move |conn| {
            // remove_file answers false for absent rather than raising,
            // which is exactly the distinction this method has to report.
            let removed: bool = conn
                .query_row("SELECT remove_file(?)", [&url], |row| row.get(0))
                .map_err(|e| duck_err("delete", e))?;
            if removed {
                Ok(())
            } else {
                Err(StoreError::NotFound(missing))
            }
        })
        .await
    }

    fn url(&self, key: &Key) -> Result<Url> {
        Url::parse(&self.object_url(key)).map_err(|_| StoreError::InvalidFileUrl(key.clone()))
    }

    fn backend(&self) -> &'static str {
        "duckvfs"
    }

    async fn size(&self, key: &Key) -> Result<u64> {
        let url = self.object_url(key);
        let missing = key.clone();
        self.with_conn(move |conn| {
            // file_size reads metadata only — the trait default would read
            // the whole body, which on Drive is a full download per call.
            let size: Option<i64> = conn
                .query_row("SELECT file_size(?)", [&url], |row| row.get(0))
                .map_err(|e| duck_err("size", e))?;
            match size {
                Some(n) if n >= 0 => Ok(n as u64),
                _ => Err(StoreError::NotFound(missing)),
            }
        })
        .await
    }
}

/// Recover a key's tenant-relative path from a `glob` hit.
///
/// `glob` does not echo the pattern's prefix back verbatim: for a `file://`
/// pattern it returns plain local paths with the scheme **stripped**, so
/// matching on the configured root alone silently yields an empty listing —
/// the failure mode this function exists to prevent. Other filesystems do
/// keep their scheme. Rather than guess per scheme, try the root as
/// configured and then the root with its scheme removed.
fn strip_base<'a>(hit: &'a str, base: &str) -> Option<&'a str> {
    if let Some(rel) = hit.strip_prefix(&format!("{base}/")) {
        return Some(rel);
    }
    let bare = base.split_once("://").map_or(base, |(_, rest)| rest);
    hit.strip_prefix(&format!("{bare}/"))
}

/// `sha256:<hex>` of the written bytes. See the module docs on why this
/// stands in for a server-side version token.
fn content_version(body: &[u8]) -> Version {
    let mut hasher = Sha256::new();
    hasher.update(body);
    format!("sha256:{:x}", hasher.finalize())
}

/// The `CREATE SECRET` for a `gdrive://` root.
///
/// `credential_chain` resolves Application Default Credentials, so a
/// `gcloud auth application-default login` on the operator's machine and a
/// workload identity on a GCP host both work with no config difference.
///
/// Two traps are encoded here. `DRIVE_ID` rather than `ROOT_FOLDER_ID`,
/// because a `0A…` id is a Shared Drive root and only `DRIVE_ID` sets the
/// `corpora=drive` listing scope it needs. And `DRIVE_SCOPE` rather than
/// `SCOPE` — `SCOPE` is DuckDB's own reserved path-scoping clause, and using
/// it produces a secret that matches nothing, silently.
fn gdrive_secret_sql(cfg: &DuckVfsStoreConfig) -> String {
    let scope = cfg.drive_scope.as_deref().unwrap_or(DEFAULT_DRIVE_SCOPE);
    let mut sql = String::from(
        "CREATE OR REPLACE SECRET escurel_lanes (TYPE gdrive, PROVIDER credential_chain",
    );
    if let Some(drive_id) = &cfg.drive_id {
        sql.push_str(&format!(", DRIVE_ID '{}'", escape_sql(drive_id)));
    }
    sql.push_str(&format!(", DRIVE_SCOPE '{}');", escape_sql(scope)));
    sql
}

/// Escape single quotes for a SQL string literal. These values come from
/// operator configuration rather than user input, but they are spliced
/// rather than bound (DuckDB does not parameterise `CREATE SECRET` or
/// `LOAD`), so they are escaped anyway.
fn escape_sql(raw: &str) -> String {
    raw.replace('\'', "''")
}

fn duck_err(op: &str, e: impl std::fmt::Display) -> StoreError {
    StoreError::Io(std::io::Error::other(format!("duckvfs {op}: {e}")))
}

/// DuckDB reports a missing file as a generic IO error with no code, so the
/// message is the only signal available. Matching on it is unpleasant but
/// the alternative — treating every read failure as `NotFound` — would hide
/// real errors (a bad credential, a network fault) behind a "no such key"
/// that callers respond to by creating the object.
fn classify_missing(e: duckdb::Error, key: &Key, op: &str) -> StoreError {
    let msg = e.to_string();
    let lower = msg.to_lowercase();
    if lower.contains("no files found")
        || lower.contains("does not exist")
        || lower.contains("no such file")
        || lower.contains("not found")
    {
        StoreError::NotFound(key.clone())
    } else {
        duck_err(op, msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_for(root: &str) -> DuckVfsStoreConfig {
        DuckVfsStoreConfig {
            root: root.to_owned(),
            extension_path: std::env::var("ESCUREL_TEST_GDRIVE_EXTENSION").ok(),
            extension_repo: None,
            drive_id: None,
            drive_scope: None,
        }
    }

    #[test]
    fn object_url_matches_the_gcs_and_s3_layout() {
        // Not cosmetic: a corpus written by one backend has to be readable
        // by the others, which only holds while all three agree on
        // {root}/tenants/{tenant}/{path}.
        let store = DuckVfsStore {
            conn: Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            root: "gdrive://escurel/lanes".to_owned(),
        };
        let key = Key::new("acme", "skills/crm.md").unwrap();
        assert_eq!(
            store.object_url(&key),
            "gdrive://escurel/lanes/tenants/acme/skills/crm.md"
        );
    }

    #[test]
    fn root_trailing_slash_does_not_double_up() {
        let cfg = cfg_for("file:///tmp/escurel/");
        let store = DuckVfsStore::new(&cfg).unwrap();
        let key = Key::new("acme", "a.md").unwrap();
        assert_eq!(
            store.object_url(&key),
            "file:///tmp/escurel/tenants/acme/a.md"
        );
    }

    #[test]
    fn gdrive_secret_uses_drive_id_and_drive_scope() {
        // DRIVE_ID (not ROOT_FOLDER_ID) and DRIVE_SCOPE (not SCOPE) are both
        // silent-failure traps — pin them.
        let cfg = DuckVfsStoreConfig {
            root: "gdrive://escurel".to_owned(),
            extension_path: None,
            extension_repo: None,
            drive_id: Some("0AA5vtjzlyjnoUk9PVA".to_owned()),
            drive_scope: None,
        };
        let sql = gdrive_secret_sql(&cfg);
        assert!(sql.contains("DRIVE_ID '0AA5vtjzlyjnoUk9PVA'"), "{sql}");
        assert!(
            sql.contains("DRIVE_SCOPE 'https://www.googleapis.com/auth/drive'"),
            "{sql}"
        );
        assert!(!sql.contains("ROOT_FOLDER_ID"), "{sql}");
        // "SCOPE '" would also match inside "DRIVE_SCOPE '", so anchor on
        // the separator that precedes a standalone SCOPE clause.
        assert!(!sql.contains(", SCOPE '"), "{sql}");
    }

    #[test]
    fn secret_literals_are_escaped() {
        let cfg = DuckVfsStoreConfig {
            root: "gdrive://escurel".to_owned(),
            extension_path: None,
            extension_repo: None,
            drive_id: Some("it's-bad".to_owned()),
            drive_scope: None,
        };
        assert!(gdrive_secret_sql(&cfg).contains("'it''s-bad'"));
    }

    #[test]
    fn strip_base_handles_glob_dropping_the_scheme() {
        // The regression this guards: glob('file:///tmp/x/**') returns
        // '/tmp/x/a.md', NOT 'file:///tmp/x/a.md'. Matching only on the
        // configured root produced an empty listing for every key — with no
        // error, because there was nothing to fail.
        let base = "file:///tmp/x/tenants/acme";
        assert_eq!(strip_base("/tmp/x/tenants/acme/a.md", base), Some("a.md"));
        // A filesystem that DOES echo the scheme back still works.
        assert_eq!(
            strip_base("file:///tmp/x/tenants/acme/a.md", base),
            Some("a.md")
        );
        // gdrive:// keeps its scheme.
        assert_eq!(
            strip_base(
                "gdrive://lanes/tenants/acme/a.md",
                "gdrive://lanes/tenants/acme"
            ),
            Some("a.md")
        );
        // A hit outside the base is not a match, rather than a mangled path.
        assert_eq!(strip_base("/tmp/other/a.md", base), None);
    }

    #[test]
    fn content_version_is_stable_and_content_addressed() {
        assert_eq!(content_version(b"hello"), content_version(b"hello"));
        assert_ne!(content_version(b"hello"), content_version(b"world"));
        assert!(content_version(b"hello").starts_with("sha256:"));
    }
}
