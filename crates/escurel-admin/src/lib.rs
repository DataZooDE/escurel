//! Admin surface for Escurel — tenant CRUD.
//!
//! The gRPC `EscurelAdmin` service in `escurel-server` is a thin
//! shell that delegates `tenant_create` / `tenant_list` /
//! `tenant_get` / `tenant_update` / `tenant_delete` to a
//! [`TenantStore`]. Today the only implementation is
//! [`FsTenantStore`] — a filesystem layout under a chosen root,
//! matching the per-tenant directory layout in
//! `docs/spec/storage.md §Per-tenant directory layout`:
//!
//! ```text
//! <root>/
//!   <tenant_id>/
//!     tenant.json         # { "tenant_id": "...", "display_name": "..." }
//!     markdown/           # canonical source — empty at create time
//!     db/escurel.duckdb   # initialised with `Migrator::up`
//! ```
//!
//! Tenant ids are validated at the boundary (lowercase ASCII,
//! digits, hyphen, underscore; 1-64 chars) so a hostile caller
//! cannot escape the data root via path traversal — the same
//! defence-in-depth posture as `escurel_storage::Key`.
//!
//! The trait is async and dyn-compatible (`Send + Sync + 'static`)
//! so the server can hold it as `Arc<dyn TenantStore>` in
//! `ServerConfig`.

use std::path::PathBuf;

use async_trait::async_trait;
use duckdb::Connection;
use escurel_index::Migrator;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Persistent identity for a tenant.
///
/// Stored on disk as `<root>/<tenant_id>/tenant.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TenantSpec {
    pub tenant_id: String,
    pub display_name: String,
}

/// Why a [`TenantStore`] call failed.
#[derive(Debug, Error)]
pub enum AdminError {
    #[error("invalid tenant id `{0}`: must match [a-z0-9_-]{{1,64}}")]
    InvalidTenantId(String),

    #[error("tenant `{0}` already exists")]
    AlreadyExists(String),

    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("duckdb error initialising tenant `{tenant}`: {source}")]
    Duckdb {
        tenant: String,
        #[source]
        source: duckdb::Error,
    },

    #[error("schema migration failed for tenant `{tenant}`: {source}")]
    Migration {
        tenant: String,
        #[source]
        source: escurel_index::schema::MigrationError,
    },

    #[error("malformed tenant.json at {path}: {source}")]
    Malformed {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

/// Storage abstraction for the admin tenant-CRUD endpoints.
///
/// Implementations must be thread-safe and cheap to clone behind
/// an `Arc` — the gRPC server keeps a single handle and calls
/// concurrently from per-request tasks.
#[async_trait]
pub trait TenantStore: Send + Sync + 'static {
    /// Enumerate every tenant the store knows about. The order is
    /// implementation-defined; callers must not rely on it.
    async fn list(&self) -> Result<Vec<TenantSpec>, AdminError>;

    /// Provision a new tenant. Returns
    /// [`AdminError::AlreadyExists`] when `spec.tenant_id` is
    /// already present; the operation is **not** idempotent (the
    /// caller decides whether to swallow it).
    async fn create(&self, spec: &TenantSpec) -> Result<(), AdminError>;

    /// Look up the spec for `tenant_id`. Returns `Ok(None)` when
    /// the tenant does not exist — only true I/O / corruption
    /// failures bubble as `Err`.
    async fn get(&self, tenant_id: &str) -> Result<Option<TenantSpec>, AdminError>;

    /// Persist a modified spec. The tenant must already exist —
    /// missing tenants return [`AdminError::Io`] with kind
    /// `NotFound` so the gRPC layer can map it to
    /// `Status::not_found`.
    async fn update(&self, spec: &TenantSpec) -> Result<(), AdminError>;

    /// Remove a tenant's directory tree. Returns `Ok(false)` when
    /// the tenant did not exist (idempotent), `Ok(true)` when a
    /// real directory was removed.
    async fn delete(&self, tenant_id: &str) -> Result<bool, AdminError>;
}

/// Filesystem-backed [`TenantStore`]. All tenant dirs live as
/// immediate children of `root`.
pub struct FsTenantStore {
    root: PathBuf,
}

impl FsTenantStore {
    /// Build a store rooted at `root`. The directory is created on
    /// first write if missing; reads of a missing root yield an
    /// empty tenant list rather than an error.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn tenant_dir(&self, tenant_id: &str) -> PathBuf {
        self.root.join(tenant_id)
    }

    fn spec_path(&self, tenant_id: &str) -> PathBuf {
        self.tenant_dir(tenant_id).join("tenant.json")
    }
}

/// Validate a tenant id against the admin-surface charset.
///
/// The accepted shape is `[a-z0-9_-]{1,64}` — lowercase ASCII,
/// digits, hyphen, underscore; never empty; never longer than 64
/// chars. This mirrors `escurel_storage::Key::validate_tenant` but
/// is strictly tighter (Key only rejects path separators and
/// `.`/`..`; admin needs the stricter charset because tenant ids
/// flow into JWT claims, log fields, and directory names).
///
/// # Errors
///
/// Returns [`AdminError::InvalidTenantId`] for anything outside
/// the charset.
pub fn validate_tenant_id(id: &str) -> Result<(), AdminError> {
    let len = id.len();
    if !(1..=64).contains(&len) {
        return Err(AdminError::InvalidTenantId(id.to_owned()));
    }
    for ch in id.chars() {
        let ok = ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '_';
        if !ok {
            return Err(AdminError::InvalidTenantId(id.to_owned()));
        }
    }
    Ok(())
}

#[async_trait]
impl TenantStore for FsTenantStore {
    async fn list(&self) -> Result<Vec<TenantSpec>, AdminError> {
        let mut out = Vec::new();
        let mut dir = match tokio::fs::read_dir(&self.root).await {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(source) => {
                return Err(AdminError::Io {
                    path: self.root.clone(),
                    source,
                });
            }
        };
        loop {
            let entry = dir.next_entry().await.map_err(|source| AdminError::Io {
                path: self.root.clone(),
                source,
            })?;
            let Some(entry) = entry else { break };
            // Only directories whose name passes validation count
            // — stray files under the root (a stray README,
            // backup tarballs, etc.) are silently skipped.
            let file_type = entry.file_type().await.map_err(|source| AdminError::Io {
                path: entry.path(),
                source,
            })?;
            if !file_type.is_dir() {
                continue;
            }
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if validate_tenant_id(name).is_err() {
                continue;
            }
            // Pull the spec; if `tenant.json` is missing fall back
            // to a synthesised spec with `display_name == tenant_id`.
            // That way a tenant created out-of-band (e.g. a
            // restored backup with no manifest yet) still shows up.
            let spec = read_spec_or_default(&self.spec_path(name), name).await?;
            out.push(spec);
        }
        Ok(out)
    }

    async fn create(&self, spec: &TenantSpec) -> Result<(), AdminError> {
        validate_tenant_id(&spec.tenant_id)?;
        let dir = self.tenant_dir(&spec.tenant_id);
        if tokio::fs::try_exists(&dir).await.unwrap_or(false) {
            return Err(AdminError::AlreadyExists(spec.tenant_id.clone()));
        }
        tokio::fs::create_dir_all(dir.join("markdown"))
            .await
            .map_err(|source| AdminError::Io {
                path: dir.join("markdown"),
                source,
            })?;
        let db_dir = dir.join("db");
        tokio::fs::create_dir_all(&db_dir)
            .await
            .map_err(|source| AdminError::Io {
                path: db_dir.clone(),
                source,
            })?;
        // tenant.json — write before DuckDB so a half-failed
        // create still leaves the spec visible to `list`/`get`.
        write_spec(&self.spec_path(&spec.tenant_id), spec).await?;
        // Initialise the per-tenant DuckDB file. We run the
        // (blocking) duckdb code on the runtime's blocking pool so
        // we don't stall the async scheduler — `Migrator::up`
        // touches `vss` + `fts` autoload which can chat with the
        // network on first run.
        let db_path = db_dir.join("escurel.duckdb");
        let tenant_id = spec.tenant_id.clone();
        tokio::task::spawn_blocking(move || -> Result<(), AdminError> {
            let conn = Connection::open(&db_path).map_err(|source| AdminError::Duckdb {
                tenant: tenant_id.clone(),
                source,
            })?;
            Migrator::up(&conn).map_err(|source| AdminError::Migration {
                tenant: tenant_id,
                source,
            })
        })
        .await
        .expect("blocking duckdb init panicked")?;
        Ok(())
    }

    async fn get(&self, tenant_id: &str) -> Result<Option<TenantSpec>, AdminError> {
        validate_tenant_id(tenant_id)?;
        let dir = self.tenant_dir(tenant_id);
        if !tokio::fs::try_exists(&dir).await.unwrap_or(false) {
            return Ok(None);
        }
        let spec = read_spec_or_default(&self.spec_path(tenant_id), tenant_id).await?;
        Ok(Some(spec))
    }

    async fn update(&self, spec: &TenantSpec) -> Result<(), AdminError> {
        validate_tenant_id(&spec.tenant_id)?;
        let dir = self.tenant_dir(&spec.tenant_id);
        if !tokio::fs::try_exists(&dir).await.unwrap_or(false) {
            return Err(AdminError::Io {
                path: dir,
                source: std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("tenant `{}` does not exist", spec.tenant_id),
                ),
            });
        }
        write_spec(&self.spec_path(&spec.tenant_id), spec).await
    }

    async fn delete(&self, tenant_id: &str) -> Result<bool, AdminError> {
        validate_tenant_id(tenant_id)?;
        let dir = self.tenant_dir(tenant_id);
        match tokio::fs::remove_dir_all(&dir).await {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(AdminError::Io { path: dir, source }),
        }
    }
}

async fn write_spec(path: &std::path::Path, spec: &TenantSpec) -> Result<(), AdminError> {
    let body = serde_json::to_vec_pretty(spec).expect("TenantSpec serialises");
    tokio::fs::write(path, body)
        .await
        .map_err(|source| AdminError::Io {
            path: path.to_path_buf(),
            source,
        })
}

async fn read_spec_or_default(
    path: &std::path::Path,
    tenant_id: &str,
) -> Result<TenantSpec, AdminError> {
    match tokio::fs::read(path).await {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|source| AdminError::Malformed {
            path: path.to_path_buf(),
            source,
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(TenantSpec {
            tenant_id: tenant_id.to_owned(),
            display_name: tenant_id.to_owned(),
        }),
        Err(source) => Err(AdminError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn validate_tenant_id_accepts_canonical_shapes() {
        for ok in ["a", "acme", "acme-corp", "acme_corp", "t-0", "a1b2c3"] {
            assert!(validate_tenant_id(ok).is_ok(), "expected `{ok}` to pass");
        }
    }

    #[test]
    fn validate_tenant_id_rejects_bad_shapes() {
        for bad in [
            "",
            "Acme",
            "acme/x",
            "..",
            ".",
            "a b",
            "a.b",
            "a:b",
            &"x".repeat(65),
        ] {
            assert!(validate_tenant_id(bad).is_err(), "expected `{bad}` to fail");
        }
    }

    #[tokio::test]
    async fn list_on_missing_root_returns_empty() {
        let store = FsTenantStore::new("/nonexistent-escurel-admin-root-xyz");
        let v = store.list().await.unwrap();
        assert!(v.is_empty());
    }

    #[tokio::test]
    async fn create_then_get_round_trips_display_name() {
        let dir = TempDir::new().unwrap();
        let store = FsTenantStore::new(dir.path());
        let spec = TenantSpec {
            tenant_id: "acme".to_owned(),
            display_name: "Acme Corp".to_owned(),
        };
        store.create(&spec).await.unwrap();
        let got = store.get("acme").await.unwrap().unwrap();
        assert_eq!(got, spec);
    }
}
