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

use crate::{Key, LaneStore, Result, SharepointCredentials, StoreError, Version};

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
    /// App-registration credentials for a `sharepoint://` root. Required
    /// for that scheme and refused for any other, so a stray credential can
    /// never silently ride along with a different provider.
    pub sharepoint: Option<SharepointCredentials>,
}

/// Which extension serves the root's scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Provider {
    /// `gdrive://`, and the home of the scheme-generic `write_blob` family
    /// for every other root (`file://`, `s3://`, …) until they ship in core.
    Gdrive,
    /// `sharepoint://`, a sibling extension exposing the same functions.
    Sharepoint,
}

impl Provider {
    fn for_root(root: &str) -> Self {
        if root.starts_with("sharepoint://") {
            Self::Sharepoint
        } else {
            Self::Gdrive
        }
    }

    /// The extension name for `INSTALL … FROM <repo>` / `LOAD`.
    fn extension(self) -> &'static str {
        match self {
            Self::Gdrive => "gdrive",
            Self::Sharepoint => "sharepoint",
        }
    }
}

/// A [`LaneStore`] over any filesystem DuckDB can reach.
pub struct DuckVfsStore {
    conn: Arc<Mutex<Connection>>,
    root: String,
    provider: Provider,
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
        let provider = check_provider_config(cfg)?;
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
            let name = provider.extension();
            let install = if repo == COMMUNITY_REPO {
                format!("INSTALL {name} FROM community;")
            } else {
                format!("INSTALL {name} FROM '{}';", escape_sql(repo))
            };
            conn.execute_batch(&format!("{install} LOAD {name};"))
                .map_err(|e| duck_err("install extension", e))?;
        }

        if provider == Provider::Sharepoint {
            let creds = cfg
                .sharepoint
                .as_ref()
                .expect("checked by check_provider_config");
            // The statement carries the client secret, and DuckDB echoes the
            // statement in some errors: scrub before the message leaves.
            conn.execute_batch(&sharepoint_secret_sql(creds))
                .map_err(|e| {
                    duck_err(
                        "create sharepoint secret",
                        creds.client_secret.scrub(&e.to_string()),
                    )
                })?;
        } else if cfg.root.starts_with("gdrive://") {
            conn.execute_batch(&gdrive_secret_sql(cfg))
                .map_err(|e| duck_err("create gdrive secret", e))?;
        }

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            root: cfg.root.trim_end_matches('/').to_owned(),
            provider,
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
        if self.provider == Provider::Sharepoint {
            check_sharepoint_name(key)?;
        }
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
        let root = self.root.clone();

        self.with_conn(move |conn| {
            let mut stmt = conn
                .prepare("SELECT file FROM glob(?)")
                .map_err(|e| duck_err("prepare list", e))?;
            let rows: Vec<String> = match stmt.query_map([&pattern], |row| row.get::<_, String>(0))
            {
                Ok(it) => it
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(|e| duck_err("list", e))?,
                // A glob on a prefix that does not exist raises on some
                // filesystems and returns nothing on others, so an error here
                // may mean "this tenant is new" — or it may mean the store is
                // unreachable, which for a `gdrive://` root is the everyday
                // failure (an expired token, a 429, the extension not loaded).
                //
                // Those cannot share an answer. `Ok(empty)` is taken as
                // authoritative by destructive callers: `Indexer::rebuild`
                // truncates the index against it. The previous `Err(_) =>
                // Vec::new()` reported both as empty, which is the same
                // reasoning this file rejects for `read` a hundred lines
                // below — "treating every read failure as `NotFound` would
                // hide real errors (a bad credential, a network fault)".
                //
                // So probe the STORE ROOT, which is created at provisioning.
                // If the root lists, the tenant prefix genuinely has nothing.
                // If it does not, the store is unreachable and that is an
                // error. The probe runs only on this path, so an ordinary
                // listing still costs one round trip.
                Err(tenant_err) => {
                    let root_pattern = format!("{root}/**");
                    let root_ok = conn
                        .prepare("SELECT file FROM glob(?)")
                        .and_then(|mut st| {
                            st.query_map([&root_pattern], |row| row.get::<_, String>(0))
                                .and_then(|it| it.collect::<std::result::Result<Vec<String>, _>>())
                        })
                        .is_ok();
                    if root_ok {
                        Vec::new()
                    } else {
                        return Err(duck_err(
                            "list (store root unreachable, so an empty listing \
                             would be a lie)",
                            tenant_err,
                        ));
                    }
                }
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

/// Validate the root/credential pairing and pick the provider.
///
/// A `sharepoint://` root needs complete credentials; credentials on any
/// other root are a misconfiguration worth a loud refusal rather than a
/// secret that is never used.
fn check_provider_config(cfg: &DuckVfsStoreConfig) -> Result<Provider> {
    let provider = Provider::for_root(&cfg.root);
    match (provider, &cfg.sharepoint) {
        (Provider::Sharepoint, Some(creds)) => {
            creds
                .validate()
                .map_err(|e| StoreError::Io(std::io::Error::other(e.to_string())))?;
        }
        (Provider::Sharepoint, None) => {
            return Err(duck_err(
                "config",
                "a sharepoint:// root needs sharepoint credentials (tenant, client id, client secret and a drive or site id)",
            ));
        }
        (Provider::Gdrive, Some(_)) => {
            return Err(duck_err(
                "config",
                "sharepoint credentials were given for a root that is not sharepoint://",
            ));
        }
        (Provider::Gdrive, None) => {}
    }
    Ok(provider)
}

/// `CREATE SECRET` for the `sharepoint` extension's `client_credentials`
/// provider. `GRAPH_SCOPE`, never `SCOPE`: DuckDB reserves `SCOPE` for the
/// secret's path-matching prefix, so an OAuth scope under that name would
/// silently rescope the secret to match nothing.
fn sharepoint_secret_sql(creds: &SharepointCredentials) -> String {
    let mut sql = format!(
        "CREATE OR REPLACE SECRET escurel_lanes (TYPE sharepoint, PROVIDER client_credentials, \
         TENANT_ID '{}', CLIENT_ID '{}', CLIENT_SECRET '{}'",
        escape_sql(&creds.tenant_id),
        escape_sql(&creds.client_id),
        escape_sql(creds.client_secret.expose()),
    );
    if let Some(drive_id) = &creds.drive_id {
        sql.push_str(&format!(", DRIVE_ID '{}'", escape_sql(drive_id)));
    }
    if let Some(site_id) = &creds.site_id {
        sql.push_str(&format!(", SITE_ID '{}'", escape_sql(site_id)));
    }
    if let Some(scope) = &creds.graph_scope {
        sql.push_str(&format!(", GRAPH_SCOPE '{}'", escape_sql(scope)));
    }
    sql.push_str(");");
    sql
}

/// SharePoint refuses these characters in a file or folder name. The
/// extension rejects them up front too, but a lane write that fails deep in
/// DuckDB reads as an IO fault; naming the key and the character makes it a
/// content problem the caller can act on.
const SHAREPOINT_FORBIDDEN: [char; 9] = ['"', '*', ':', '<', '>', '?', '\\', '|', '\0'];

fn check_sharepoint_name(key: &Key) -> Result<()> {
    let path = key.path();
    match path.chars().find(|c| SHAREPOINT_FORBIDDEN.contains(c)) {
        Some(c) => Err(StoreError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "sharepoint cannot store {path:?}: {c:?} is not allowed in a SharePoint file name"
            ),
        ))),
        None => Ok(()),
    }
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
            sharepoint: None,
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
            provider: Provider::Gdrive,
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
            sharepoint: None,
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
            sharepoint: None,
        };
        assert!(gdrive_secret_sql(&cfg).contains("'it''s-bad'"));
    }

    fn sp_creds() -> SharepointCredentials {
        SharepointCredentials {
            tenant_id: "00000000-0000-0000-0000-000000000000".to_owned(),
            client_id: "11111111-1111-1111-1111-111111111111".to_owned(),
            client_secret: crate::SecretString::new("s3cr3t-value"),
            drive_id: Some("b!AbCdEf".to_owned()),
            site_id: None,
            graph_scope: None,
        }
    }

    fn sp_cfg(creds: Option<SharepointCredentials>) -> DuckVfsStoreConfig {
        DuckVfsStoreConfig {
            root: "sharepoint://escurel/lanes".to_owned(),
            sharepoint: creds,
            ..DuckVfsStoreConfig::default()
        }
    }

    #[test]
    fn provider_follows_the_root_scheme() {
        assert_eq!(
            Provider::for_root("sharepoint://escurel/lanes"),
            Provider::Sharepoint
        );
        assert_eq!(Provider::for_root("gdrive://escurel"), Provider::Gdrive);
        // Every other scheme keeps riding the gdrive extension's write_blob
        // family, exactly as before.
        assert_eq!(Provider::for_root("file:///tmp/x"), Provider::Gdrive);
        assert_eq!(Provider::for_root("s3://bucket/p"), Provider::Gdrive);
        assert_eq!(Provider::Sharepoint.extension(), "sharepoint");
        assert_eq!(Provider::Gdrive.extension(), "gdrive");
    }

    #[test]
    fn sharepoint_secret_uses_graph_scope_and_the_right_keys() {
        let sql = sharepoint_secret_sql(&SharepointCredentials {
            graph_scope: Some("https://graph.microsoft.com/.default".to_owned()),
            ..sp_creds()
        });
        for needle in [
            "TYPE sharepoint",
            "PROVIDER client_credentials",
            "TENANT_ID '00000000-0000-0000-0000-000000000000'",
            "CLIENT_ID '11111111-1111-1111-1111-111111111111'",
            "CLIENT_SECRET 's3cr3t-value'",
            "DRIVE_ID 'b!AbCdEf'",
            "GRAPH_SCOPE 'https://graph.microsoft.com/.default'",
        ] {
            assert!(sql.contains(needle), "{needle} missing from the statement");
        }
        assert!(!sql.contains(", SCOPE '"), "SCOPE would rescope the secret");
        assert!(!sql.contains("SITE_ID"), "only one target is sent");
        assert!(sql.starts_with("CREATE OR REPLACE SECRET escurel_lanes"));
    }

    #[test]
    fn a_site_id_is_sent_as_site_id_not_drive_id() {
        let sql = sharepoint_secret_sql(&SharepointCredentials {
            drive_id: None,
            site_id: Some("contoso.sharepoint.com,s,w".to_owned()),
            ..sp_creds()
        });
        assert!(
            sql.contains("SITE_ID 'contoso.sharepoint.com,s,w'"),
            "{sql}"
        );
        assert!(!sql.contains("DRIVE_ID"), "{sql}");
        assert!(
            !sql.contains("GRAPH_SCOPE"),
            "default scope is the extension's: {sql}"
        );
    }

    #[test]
    fn sharepoint_secret_literals_are_escaped() {
        let sql = sharepoint_secret_sql(&SharepointCredentials {
            client_secret: crate::SecretString::new("it's"),
            ..sp_creds()
        });
        assert!(sql.contains("CLIENT_SECRET 'it''s'"), "{sql}");
    }

    #[test]
    fn a_sharepoint_root_without_credentials_is_refused() {
        let err = DuckVfsStore::new(&sp_cfg(None)).err().expect("refused");
        assert!(err.to_string().contains("sharepoint credentials"), "{err}");
    }

    #[test]
    fn incomplete_sharepoint_credentials_are_refused_before_duckdb() {
        let err = DuckVfsStore::new(&sp_cfg(Some(SharepointCredentials {
            drive_id: None,
            ..sp_creds()
        })))
        .err()
        .expect("refused");
        assert!(err.to_string().contains("drive_id and site_id"), "{err}");
        assert!(!err.to_string().contains("s3cr3t-value"), "{err}");
    }

    #[test]
    fn sharepoint_credentials_on_another_root_are_refused() {
        let cfg = DuckVfsStoreConfig {
            root: "gdrive://escurel".to_owned(),
            sharepoint: Some(sp_creds()),
            ..DuckVfsStoreConfig::default()
        };
        let err = DuckVfsStore::new(&cfg).err().expect("refused");
        assert!(err.to_string().contains("not sharepoint://"), "{err}");
    }

    #[test]
    fn the_secret_never_appears_in_an_open_error() {
        // No sharepoint extension is loaded here, so CREATE SECRET fails on
        // an unknown type. Whatever DuckDB echoes, the secret must not
        // survive into the error.
        let err = DuckVfsStore::new(&sp_cfg(Some(sp_creds())))
            .err()
            .expect("secret type unknown without the extension");
        assert!(!err.to_string().contains("s3cr3t-value"), "{err}");
        assert!(!format!("{err:?}").contains("s3cr3t-value"), "{err:?}");
    }

    #[test]
    fn sharepoint_forbidden_names_are_refused_with_the_character_named() {
        let bad = Key::new("acme", "notes/why?.md").unwrap();
        let err = check_sharepoint_name(&bad).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("'?'") && msg.contains("why?.md"), "{msg}");
        for ch in ['"', '*', ':', '<', '>', '|', '\\'] {
            let k = Key::new("acme", format!("a{ch}b.md"));
            if let Ok(k) = k {
                assert!(check_sharepoint_name(&k).is_err(), "{ch:?}");
            }
        }
        let ok = Key::new("acme", "skills/crm-2026.md").unwrap();
        check_sharepoint_name(&ok).expect("plain names pass");
    }

    /// The staged, extension-equipped checks. `#[ignore]`d for the same reason
    /// the gdrive round trip is: they need a built `sharepoint.duckdb_extension`
    /// (the erpl.io mirror serves one per DuckDB release), which CI does not
    /// carry. An ordinary run REPORTS them ignored; run on purpose with
    /// `ESCUREL_TEST_SHAREPOINT_EXTENSION=/path/sharepoint.duckdb_extension
    /// cargo test -p escurel-storage --features duckvfs --lib -- --ignored sharepoint`.
    /// None of them touches the network: a secret is only registered, never spent.
    fn sharepoint_extension() -> String {
        std::env::var("ESCUREL_TEST_SHAREPOINT_EXTENSION")
            .expect("set ESCUREL_TEST_SHAREPOINT_EXTENSION to a built sharepoint.duckdb_extension")
    }

    #[test]
    #[ignore = "needs ESCUREL_TEST_SHAREPOINT_EXTENSION (a built sharepoint.duckdb_extension)"]
    fn a_sharepoint_store_registers_the_secret_with_the_real_extension() {
        let store = DuckVfsStore::new(&DuckVfsStoreConfig {
            extension_path: Some(sharepoint_extension()),
            ..sp_cfg(Some(sp_creds()))
        })
        .expect("the extension accepts the statement escurel builds");
        let conn = store.conn.lock().unwrap();
        let (ty, provider, redacted): (String, String, String) = conn
            .query_row(
                "SELECT type, provider, secret_string FROM duckdb_secrets() WHERE name = 'escurel_lanes'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .expect("secret escurel_lanes is registered");
        assert_eq!(ty, "sharepoint");
        assert_eq!(provider, "client_credentials");
        // DuckDB itself redacts the value in its listing; the keys we sent
        // are the ones it recorded.
        for key in ["tenant_id", "client_id", "drive_id"] {
            assert!(redacted.contains(key), "{key} not recorded: {redacted}");
        }
        assert!(!redacted.contains("s3cr3t-value"), "{redacted}");
    }

    #[test]
    #[ignore = "needs ESCUREL_TEST_SHAREPOINT_EXTENSION (a built sharepoint.duckdb_extension)"]
    fn a_quote_in_a_sharepoint_value_cannot_inject_a_secret_option() {
        // The extension rejects an unknown option ("Unknown parameter"), so
        // an injected `BOGUS_OPTION` would fail the open. It opens, which
        // shows the quote stayed inside its string literal.
        let store = DuckVfsStore::new(&DuckVfsStoreConfig {
            extension_path: Some(sharepoint_extension()),
            ..sp_cfg(Some(SharepointCredentials {
                graph_scope: Some("x', BOGUS_OPTION 'y".to_owned()),
                ..sp_creds()
            }))
        })
        .expect("the quote is escaped, not interpreted");
        let conn = store.conn.lock().unwrap();
        let recorded: String = conn
            .query_row(
                "SELECT secret_string FROM duckdb_secrets() WHERE name = 'escurel_lanes'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            !recorded.to_lowercase().contains("bogus_option="),
            "{recorded}"
        );
    }

    #[test]
    #[ignore = "needs ESCUREL_TEST_SHAREPOINT_EXTENSION (a built sharepoint.duckdb_extension)"]
    fn a_sharepoint_write_with_a_forbidden_name_fails_before_any_io() {
        let store = DuckVfsStore::new(&DuckVfsStoreConfig {
            extension_path: Some(sharepoint_extension()),
            ..sp_cfg(Some(sp_creds()))
        })
        .expect("open");
        let key = Key::new("acme", "notes/a:b.md").unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let err = rt
            .block_on(store.write(&key, Bytes::from_static(b"x")))
            .unwrap_err();
        assert!(
            err.to_string().contains("not allowed in a SharePoint"),
            "{err}"
        );
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
