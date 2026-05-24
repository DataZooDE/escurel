//! Tenant-scoped storage key with path-traversal-safe construction.

use std::fmt;

use thiserror::Error;

/// A tenant-scoped relative path inside the lane store.
///
/// The store maps `Key { tenant, path }` to either a filesystem
/// path under `${root}/tenants/{tenant}/{path}` or an S3 object
/// under `s3://{bucket}/{prefix}/tenants/{tenant}/{path}`.
///
/// `Key::new` validates both fields at the boundary so every
/// downstream consumer (`FsStore`, future `S3Store`, indexer audit,
/// …) can trust that the key won't escape its tenant. See
/// [`KeyError`] for the rejected shapes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Key {
    tenant: String,
    path: String,
}

/// Why [`Key::new`] rejected its input.
#[derive(Debug, Error)]
pub enum KeyError {
    #[error("tenant must be non-empty and free of path separators, `.`, and `..`: {0:?}")]
    InvalidTenant(String),

    #[error(
        "path must be relative (no leading `/`) and contain no `.` / `..` segments \
         or backslashes: {0:?}"
    )]
    InvalidPath(String),
}

impl Key {
    /// Build a key from a tenant id and a forward-slash-separated
    /// relative path.
    ///
    /// # Errors
    ///
    /// Returns [`KeyError::InvalidTenant`] when `tenant` is empty,
    /// contains a path separator, or equals `.` / `..`. Returns
    /// [`KeyError::InvalidPath`] when `path` is absolute (leading
    /// `/`), contains a backslash, or contains a `.` / `..` segment.
    /// Empty `path` is allowed — it represents the full-tenant
    /// prefix used by `list`.
    pub fn new(tenant: impl Into<String>, path: impl Into<String>) -> Result<Self, KeyError> {
        let tenant = tenant.into();
        let path = path.into();
        validate_tenant(&tenant)?;
        validate_path(&path)?;
        Ok(Self { tenant, path })
    }

    #[must_use]
    pub fn tenant(&self) -> &str {
        &self.tenant
    }

    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// True if this key's `path` starts with `other.path`. Used by
    /// `list(prefix)`.
    #[must_use]
    pub fn has_prefix(&self, other: &Self) -> bool {
        self.tenant == other.tenant && self.path.starts_with(&other.path)
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.tenant, self.path)
    }
}

fn validate_tenant(tenant: &str) -> Result<(), KeyError> {
    if tenant.is_empty()
        || tenant == "."
        || tenant == ".."
        || tenant.contains('/')
        || tenant.contains('\\')
    {
        return Err(KeyError::InvalidTenant(tenant.to_owned()));
    }
    Ok(())
}

fn validate_path(path: &str) -> Result<(), KeyError> {
    if path.starts_with('/') || path.contains('\\') {
        return Err(KeyError::InvalidPath(path.to_owned()));
    }
    // Empty path is the legitimate "list everything for this tenant"
    // prefix; `split('/')` on "" yields a single empty segment, so
    // short-circuit before the segment scan.
    if path.is_empty() {
        return Ok(());
    }
    for segment in path.split('/') {
        if segment == "." || segment == ".." {
            return Err(KeyError::InvalidPath(path.to_owned()));
        }
    }
    Ok(())
}
