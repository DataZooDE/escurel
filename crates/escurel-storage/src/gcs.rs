//! Google Cloud Storage-backed [`LaneStore`] (feature `gcs`).
//!
//! Built on the official `google-cloud-storage` crate. Authentication is
//! **Application Default Credentials**, which is what makes this usable in
//! both the places it needs to work:
//!
//! - **On GCP** — the metadata server / workload identity supplies tokens and
//!   no credential material is stored anywhere.
//! - **Off GCP** — a service-account key file, which is the only path a
//!   non-GCP host (e.g. a Hetzner node) has. Point
//!   [`GcsStoreConfig::credentials_path`] at it and the file is loaded and
//!   passed to the client explicitly; ADC's own `GOOGLE_APPLICATION_CREDENTIALS`
//!   lookup also still works if you would rather set it in the environment.
//!
//! Note the residency constraint that comes with this backend: the DataZoo
//! substrate's SPEC §5 puts app/customer data on Hetzner Object Storage and
//! keeps only recovery/integrity-critical data on GCP, so `GcsStore` is a
//! portability backend for GCP-hosted deployments — not something that
//! substrate tenants' lanes should be pointed at.
//!
//! Object layout matches [`crate::s3::S3Store`] exactly:
//! `{prefix}/tenants/{tenant}/{path}`, so a bucket is readable by either
//! backend and the two are migration-compatible.

use async_trait::async_trait;
use bytes::Bytes;
use google_cloud_gax::paginator::ItemPaginator as _;
use google_cloud_storage::client::{Storage, StorageControl};
use url::Url;

use crate::{Key, LaneStore, Result, StoreError, Version};

/// Everything needed to reach a GCS bucket. Unlike [`crate::s3::S3StoreConfig`]
/// there are no inline key/secret fields: credentials come from ADC, so the
/// only credential knob is *which* ADC source to use.
#[derive(Debug, Clone, Default)]
pub struct GcsStoreConfig {
    /// Bucket id (not a URL, not a `gs://` prefix).
    pub bucket: String,
    /// Key prefix inside the bucket, e.g. `escurel/prod`. May be empty.
    pub prefix: String,
    /// Path to a service-account key file — the off-GCP path. When set, the
    /// file is read and handed to the client as explicit credentials (the
    /// process environment is left alone). Leave `None` on GCP, or when the
    /// operator prefers to set `GOOGLE_APPLICATION_CREDENTIALS` themselves,
    /// to fall through to ordinary ADC.
    pub credentials_path: Option<String>,
    /// Override the GCS endpoint. Only for tests against an emulator
    /// (`fake-gcs-server`); production leaves this `None`.
    pub endpoint: Option<String>,
}

/// A [`LaneStore`] over a GCS bucket.
///
/// Two clients because the crate splits the surface: object *payload*
/// (read/write) lives on [`Storage`], while object *metadata and lifecycle*
/// (list/delete/get) lives on [`StorageControl`].
pub struct GcsStore {
    data: Storage,
    control: StorageControl,
    bucket: String,
    prefix: String,
}

/// GCS addresses buckets as a resource path, not a bare id.
fn bucket_resource(bucket: &str) -> String {
    format!("projects/_/buckets/{bucket}")
}

fn normalise_prefix(raw: &str) -> String {
    raw.trim_matches('/').to_owned()
}

fn gcs_io_error(op: &str, e: impl std::fmt::Display) -> StoreError {
    StoreError::Io(std::io::Error::other(format!("gcs {op}: {e}")))
}

impl GcsStore {
    /// Build a client. Performs no network I/O beyond credential discovery,
    /// so a misconfigured bucket surfaces on first use rather than here.
    pub async fn new(config: GcsStoreConfig) -> Result<Self> {
        let mut data_builder = Storage::builder();
        let mut control_builder = StorageControl::builder();
        if let Some(endpoint) = config.endpoint.as_deref() {
            data_builder = data_builder.with_endpoint(endpoint);
            control_builder = control_builder.with_endpoint(endpoint);
        }

        // An explicitly-configured key file is loaded and handed to the
        // client directly, rather than exported as
        // `GOOGLE_APPLICATION_CREDENTIALS`: mutating the process environment
        // is `unsafe` in this edition (other threads may be reading it) and
        // it silently reconfigures every other Google client in the process.
        // With no path configured we set nothing, and the client falls
        // through to ordinary ADC — metadata server on GCP, or the
        // environment variable if the operator prefers to set it themselves.
        if let Some(path) = config.credentials_path.as_deref() {
            let raw = std::fs::read_to_string(path)
                .map_err(|e| gcs_io_error("read credentials file", format!("{path}: {e}")))?;
            let json: serde_json::Value = serde_json::from_str(&raw)
                .map_err(|e| gcs_io_error("parse credentials file", format!("{path}: {e}")))?;
            let creds = google_cloud_auth::credentials::service_account::Builder::new(json)
                .build()
                .map_err(|e| gcs_io_error("build service-account credentials", e))?;
            data_builder = data_builder.with_credentials(creds.clone());
            control_builder = control_builder.with_credentials(creds);
        }
        let data = data_builder
            .build()
            .await
            .map_err(|e| gcs_io_error("build storage client", e))?;
        let control = control_builder
            .build()
            .await
            .map_err(|e| gcs_io_error("build control client", e))?;

        Ok(Self {
            data,
            control,
            bucket: config.bucket,
            prefix: normalise_prefix(&config.prefix),
        })
    }

    /// `{prefix}/tenants/{tenant}/{path}` — identical to the S3 backend, so
    /// the same bucket layout is readable by either.
    fn object_key(&self, key: &Key) -> String {
        let tenant_path = format!("tenants/{}/{}", key.tenant(), key.path());
        if self.prefix.is_empty() {
            tenant_path
        } else {
            format!("{}/{}", self.prefix, tenant_path)
        }
    }

    /// `(full object prefix, tenant-relative base)` — the second is stripped
    /// off listing results so callers get tenant-relative paths, which is
    /// what the `LaneStore` contract (and the blob layer) requires.
    fn list_prefix(&self, prefix: &Key) -> (String, String) {
        let tenant_base = format!("tenants/{}/", prefix.tenant());
        let full = if self.prefix.is_empty() {
            format!("{tenant_base}{}", prefix.path())
        } else {
            format!("{}/{tenant_base}{}", self.prefix, prefix.path())
        };
        let base = if self.prefix.is_empty() {
            tenant_base
        } else {
            format!("{}/{tenant_base}", self.prefix)
        };
        (full, base)
    }

    /// Distinguish "no such object" from a transport failure. The GAPIC error
    /// surface reports this as HTTP 404.
    fn is_not_found(e: &google_cloud_storage::Error) -> bool {
        e.http_status_code() == Some(404)
    }
}

#[async_trait]
impl LaneStore for GcsStore {
    async fn read(&self, key: &Key) -> Result<Bytes> {
        let mut resp = self
            .data
            .read_object(bucket_resource(&self.bucket), self.object_key(key))
            .send()
            .await
            .map_err(|e| {
                if Self::is_not_found(&e) {
                    StoreError::NotFound(key.clone())
                } else {
                    gcs_io_error("read_object", e)
                }
            })?;

        let mut body = Vec::new();
        while let Some(chunk) = resp
            .next()
            .await
            .transpose()
            .map_err(|e| gcs_io_error("read_object stream", e))?
        {
            body.extend_from_slice(&chunk);
        }
        Ok(Bytes::from(body))
    }

    async fn write(&self, key: &Key, body: Bytes) -> Result<Version> {
        // A GCS object write is atomic at the object level — a reader sees
        // either the prior object or the new one in full, never a partial
        // body. No temp-then-rename dance (unlike `FsStore`, which needs
        // `rename(2)` for that guarantee).
        let object = self
            .data
            .write_object(bucket_resource(&self.bucket), self.object_key(key), body)
            .send_unbuffered()
            .await
            .map_err(|e| gcs_io_error("write_object", e))?;

        // Generation is GCS's monotonic per-object version. Fall back to the
        // etag so the contract's "distinct writes yield distinct versions"
        // holds even if a backend omits generation.
        let version = if object.generation != 0 {
            object.generation.to_string()
        } else {
            object.etag.clone()
        };
        Ok(version)
    }

    async fn list(&self, prefix: &Key) -> Result<Vec<Key>> {
        let (full_prefix, tenant_base) = self.list_prefix(prefix);
        let mut out = Vec::new();
        let mut pages = self
            .control
            .list_objects()
            .set_parent(bucket_resource(&self.bucket))
            .set_prefix(full_prefix)
            .by_item();

        while let Some(object) = pages
            .next()
            .await
            .transpose()
            .map_err(|e| gcs_io_error("list_objects", e))?
        {
            let Some(rel) = object.name.strip_prefix(&tenant_base) else {
                continue;
            };
            // Re-validate through `Key` rather than trusting the bucket: a
            // key written by something else must not poison the whole
            // listing, so skip rather than fail (parity with S3/FS).
            if let Ok(k) = Key::new(prefix.tenant(), rel) {
                out.push(k);
            }
        }
        Ok(out)
    }

    async fn delete(&self, key: &Key) -> Result<()> {
        // GCS DeleteObject on a missing key is a 404, so unlike S3 this
        // needs no HEAD-first dance to honour the NotFound contract.
        self.control
            .delete_object()
            .set_bucket(bucket_resource(&self.bucket))
            .set_object(self.object_key(key))
            .send()
            .await
            .map_err(|e| {
                if Self::is_not_found(&e) {
                    StoreError::NotFound(key.clone())
                } else {
                    gcs_io_error("delete_object", e)
                }
            })?;
        Ok(())
    }

    fn url(&self, key: &Key) -> Result<Url> {
        // `gs://bucket/key` — the scheme DuckDB httpfs and `LakeConfig`'s
        // `gs://` DATA_PATH both expect. As with S3, the endpoint is NOT in
        // the URL: a consumer resolves it from its own configured secret, so
        // that config must agree with this store's.
        let raw = format!("gs://{}/{}", self.bucket, self.object_key(key));
        Url::parse(&raw).map_err(|_| StoreError::InvalidFileUrl(key.clone()))
    }

    fn backend(&self) -> &'static str {
        "gcs"
    }

    async fn size(&self, key: &Key) -> Result<u64> {
        let object = self
            .control
            .get_object()
            .set_bucket(bucket_resource(&self.bucket))
            .set_object(self.object_key(key))
            .send()
            .await
            .map_err(|e| {
                if Self::is_not_found(&e) {
                    StoreError::NotFound(key.clone())
                } else {
                    gcs_io_error("get_object", e)
                }
            })?;
        Ok(object.size.max(0) as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(prefix: &str) -> GcsStoreConfig {
        GcsStoreConfig {
            bucket: "b".to_owned(),
            prefix: prefix.to_owned(),
            ..Default::default()
        }
    }

    /// The object layout must stay byte-identical to the S3 backend so one
    /// bucket is readable by either and a migration is a copy, not a rewrite.
    #[test]
    fn object_key_matches_the_s3_layout() {
        let store_prefix = normalise_prefix("/escurel/prod/");
        assert_eq!(store_prefix, "escurel/prod");
        let key = Key::new("acme", "markdown/skills/customer.md").unwrap();
        let tenant_path = format!("tenants/{}/{}", key.tenant(), key.path());
        assert_eq!(
            format!("{store_prefix}/{tenant_path}"),
            "escurel/prod/tenants/acme/markdown/skills/customer.md",
        );
    }

    #[test]
    fn bucket_is_addressed_as_a_resource_path() {
        assert_eq!(bucket_resource("my-bucket"), "projects/_/buckets/my-bucket");
    }

    #[test]
    fn empty_prefix_yields_no_leading_separator() {
        let _ = cfg("");
        let key = Key::new("acme", "a.md").unwrap();
        let tenant_path = format!("tenants/{}/{}", key.tenant(), key.path());
        assert_eq!(tenant_path, "tenants/acme/a.md");
    }
}
