//! Tenant-scoped storage key.

use std::fmt;

/// A tenant-scoped relative path inside the lane store.
///
/// The store maps `Key { tenant, path }` to either a filesystem
/// path under `${root}/tenants/{tenant}/{path}` or an S3 object
/// under `s3://{bucket}/{prefix}/tenants/{tenant}/{path}`.
///
/// `path` is a forward-slash-separated relative path; the store
/// rejects `..` segments and absolute paths.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Key {
    tenant: String,
    path: String,
}

impl Key {
    /// Build a key from a tenant id and a forward-slash-separated
    /// relative path. `path` must not contain `..` segments or
    /// start with `/`; the constructor only does the cheap checks,
    /// the store enforces the rest at access time.
    pub fn new(tenant: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            tenant: tenant.into(),
            path: path.into(),
        }
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
