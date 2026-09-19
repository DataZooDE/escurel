//! Filesystem-backed [`LaneStore`] implementation.
//!
//! Dev-only per the spec; production uses the S3 backend.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use bytes::Bytes;
use tokio::fs as tfs;
use url::Url;
use walkdir::WalkDir;

use crate::{Key, LaneStore, Result, StoreError, Version};

/// Filesystem-backed lane store rooted at an absolute directory.
///
/// Layout: `{root}/tenants/{key.tenant}/{key.path}`.
#[derive(Debug, Clone)]
pub struct FsStore {
    root: PathBuf,
}

impl FsStore {
    /// Build an `FsStore` rooted at `root`. `root` should be an
    /// existing absolute directory; the store creates per-key
    /// parent directories lazily on `write`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn resolve(&self, key: &Key) -> PathBuf {
        self.root
            .join("tenants")
            .join(key.tenant())
            .join(key.path())
    }

    fn tenant_root(&self, tenant: &str) -> PathBuf {
        self.root.join("tenants").join(tenant)
    }
}

#[async_trait]
impl LaneStore for FsStore {
    async fn read(&self, key: &Key) -> Result<Bytes> {
        let path = self.resolve(key);
        match tfs::read(&path).await {
            Ok(bytes) => Ok(Bytes::from(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(StoreError::NotFound(key.clone()))
            }
            Err(e) => Err(StoreError::Io(e)),
        }
    }

    async fn write(&self, key: &Key, body: Bytes) -> Result<Version> {
        let path = self.resolve(key);
        if let Some(parent) = path.parent() {
            tfs::create_dir_all(parent).await?;
        }

        // Atomic publish: write to `<path>.tmp` then rename. Same-
        // filesystem rename is atomic on POSIX, matching the
        // storage-spec invariant. We use `.tmp` as a SUFFIX
        // (`customer.md.tmp`), not an extension replacement —
        // `Path::with_extension` would turn `customer.md` into
        // `customer.tmp`, which is wrong.
        let tmp = append_tmp_suffix(&path);
        tfs::write(&tmp, &body).await?;
        match tfs::rename(&tmp, &path).await {
            Ok(()) => {}
            Err(e) => {
                // Best-effort cleanup of the tmp file on rename
                // failure; ignore secondary errors.
                let _ = tfs::remove_file(&tmp).await;
                return Err(StoreError::Io(e));
            }
        }

        let meta = tfs::metadata(&path).await?;
        let version = format_version(meta.modified()?);
        Ok(version)
    }

    async fn list(&self, prefix: &Key) -> Result<Vec<Key>> {
        let tenant = prefix.tenant().to_owned();
        let store_root = self.root.clone();
        let tenant_root = self.tenant_root(&tenant);
        let prefix_path = prefix.path().to_owned();

        // walkdir is sync; offload to a blocking task so we don't
        // stall the runtime on large trees.
        let keys = tokio::task::spawn_blocking(move || {
            list_under(&store_root, &tenant_root, &tenant, &prefix_path)
        })
        .await
        .map_err(|e| StoreError::Io(std::io::Error::other(e)))??;

        Ok(keys)
    }

    async fn delete(&self, key: &Key) -> Result<()> {
        let path = self.resolve(key);
        match tfs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(StoreError::NotFound(key.clone()))
            }
            Err(e) => Err(StoreError::Io(e)),
        }
    }

    fn url(&self, key: &Key) -> Result<Url> {
        let path = self.resolve(key);
        Url::from_file_path(&path).map_err(|()| StoreError::InvalidFileUrl(key.clone()))
    }

    fn backend(&self) -> &'static str {
        "fs"
    }

    async fn size(&self, key: &Key) -> Result<u64> {
        let path = self.resolve(key);
        match tfs::metadata(&path).await {
            Ok(m) => Ok(m.len()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(StoreError::NotFound(key.clone()))
            }
            Err(e) => Err(StoreError::Io(e)),
        }
    }
}

/// Append a literal `.tmp` to `path`, preserving its existing
/// extension. Used for the atomic write-then-rename publish.
fn append_tmp_suffix(path: &Path) -> PathBuf {
    let mut bytes: OsString = path.as_os_str().to_owned();
    bytes.push(".tmp");
    PathBuf::from(bytes)
}

fn format_version(mtime: SystemTime) -> Version {
    mtime
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|err| Duration::from_nanos(err.duration().as_nanos() as u64))
        .as_nanos()
        .to_string()
}

/// Walk `tenant_root` recursively and return keys whose relative path under
/// the tenant root starts with `prefix_path`.
///
/// `Ok(empty)` means one thing only: **the store was reachable and this
/// tenant has nothing**. Every condition under which the listing might be
/// incomplete is an error, because destructive callers treat an empty
/// listing as authoritative — `Indexer::rebuild` truncates the index against
/// it, and the orphan-blob reclaim then deletes canonical bytes the empty
/// index no longer references. "There is nothing here" and "I could not
/// tell" cannot be the same answer.
///
/// `store_root` is what separates them, and it needs no marker object: the
/// root is created at provisioning, so its absence means the volume is not
/// mounted or the path is wrong. A missing *tenant* directory under a root
/// that does exist is a genuinely new tenant, and is empty.
fn list_under(
    store_root: &Path,
    tenant_root: &Path,
    tenant: &str,
    prefix_path: &str,
) -> Result<Vec<Key>> {
    if !store_root.exists() {
        return Err(StoreError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "lane store root {} does not exist — the store is unreachable, \
                 not empty (an unmounted volume or a misconfigured path)",
                store_root.display()
            ),
        )));
    }
    if !tenant_root.exists() {
        // Provisioned store, no writes for this tenant yet.
        return Ok(Vec::new());
    }

    let mut out = Vec::new();
    for entry in WalkDir::new(tenant_root).follow_links(false) {
        // Propagate rather than `filter_map(Result::ok)`: a subtree this
        // process cannot read (EACCES) or an I/O failure mid-walk silently
        // shortened the listing, which is the same lie as an empty one.
        let entry = entry.map_err(|e| {
            StoreError::Io(std::io::Error::other(format!(
                "listing {} was incomplete: {e}",
                tenant_root.display()
            )))
        })?;
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(tenant_root)
            .expect("walkdir yields paths under its root");
        let rel_str = rel
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/");
        if rel_str.starts_with(prefix_path) {
            // Defensive: a file on disk with a pathological name
            // (somebody else's process, a symlink target, …) can
            // fail `Key::new`'s validation. Skip rather than poison
            // the whole list — the audit path will surface drift
            // on the markdown side.
            if let Ok(key) = Key::new(tenant.to_owned(), rel_str) {
                out.push(key);
            }
        }
    }
    Ok(out)
}
