//! S3-backed [`LaneStore`] implementation.
//!
//! Production default per the v1 spec (storage.md). Speaks plain S3
//! (GET / PUT / DELETE / ListObjectsV2) against any S3-compatible
//! endpoint — AWS S3, MinIO, Hetzner Object Storage — via a custom
//! endpoint URL with **path-style addressing** (`force_path_style`).
//! Virtual-host-style addressing requires per-bucket DNS, which MinIO
//! and Hetzner do not provide out of the box, so path-style is the
//! portable choice.
//!
//! Layout (storage.md §S3): `s3://<bucket>/<prefix>/tenants/<tenant>/<path>`.
//!
//! Gated behind the `s3` Cargo feature so the dev/test default
//! (`FsStore`) need not pull the AWS SDK.

use async_trait::async_trait;
use aws_credential_types::Credentials;
use aws_sdk_s3::Client;
use aws_sdk_s3::config::{BehaviorVersion, Region};
use aws_sdk_s3::error::SdkError;
use aws_sdk_s3::operation::get_object::GetObjectError;
use aws_sdk_s3::operation::head_object::HeadObjectError;
use aws_sdk_s3::primitives::ByteStream;
use bytes::Bytes;
use url::Url;

use crate::{Key, LaneStore, Result, StoreError, Version};

/// Static configuration for an [`S3Store`].
///
/// All fields are required and supplied by the caller (12-factor:
/// the binary translates `ESCUREL_*` env / Vault secrets into this
/// struct). No ambient AWS credential chain is consulted — the
/// substrate target injects an explicit access key / secret.
#[derive(Debug, Clone)]
pub struct S3StoreConfig {
    /// Target bucket. Must already exist (or call [`S3Store::ensure_bucket`]).
    pub bucket: String,
    /// Key prefix under the bucket, e.g. `escurel/prod`. May be empty.
    pub prefix: String,
    /// S3 endpoint URL, e.g. `https://s3.eu-central-1.example.com`.
    ///
    /// Critical (storage.md): the hostname here must equal the
    /// hostname DuckDB `httpfs` is configured against, since `url()`
    /// derives from `bucket` + `prefix` only — callers must keep the
    /// DuckDB secret's `ENDPOINT` in sync with this value.
    pub endpoint_url: String,
    /// Region label. S3-compatible stores ignore it but the SDK
    /// requires one; `us-east-1` is the conventional default.
    pub region: String,
    /// Static access key id.
    pub access_key_id: String,
    /// Static secret access key.
    pub secret_access_key: String,
}

/// S3-backed lane store.
#[derive(Debug, Clone)]
pub struct S3Store {
    client: Client,
    bucket: String,
    /// Normalised prefix: no leading/trailing slash. Empty means the
    /// bucket root.
    prefix: String,
}

impl S3Store {
    /// Build an `S3Store` from static configuration.
    ///
    /// Constructs an S3 client with the custom endpoint, path-style
    /// addressing, and static credentials. Does not touch the
    /// network — call [`S3Store::ensure_bucket`] (or pre-create the
    /// bucket out of band) before first use.
    pub async fn new(config: S3StoreConfig) -> Result<Self> {
        let creds = Credentials::new(
            config.access_key_id,
            config.secret_access_key,
            None,
            None,
            "escurel-static",
        );

        let s3_config = aws_sdk_s3::config::Builder::new()
            .behavior_version(BehaviorVersion::latest())
            .region(Region::new(config.region))
            .endpoint_url(config.endpoint_url)
            .credentials_provider(creds)
            // MinIO + Hetzner Object Storage require path-style
            // addressing (bucket in the path, not the hostname).
            .force_path_style(true)
            .build();

        let client = Client::from_conf(s3_config);

        Ok(Self {
            client,
            bucket: config.bucket,
            prefix: normalise_prefix(&config.prefix),
        })
    }

    /// Idempotently create the configured bucket. Safe to call when
    /// the bucket already exists (treats `BucketAlreadyOwnedByYou`
    /// and `BucketAlreadyExists` as success). Primarily for tests
    /// and first-boot provisioning; production may pre-create the
    /// bucket via terraform.
    pub async fn ensure_bucket(&self) -> Result<()> {
        match self
            .client
            .create_bucket()
            .bucket(&self.bucket)
            .send()
            .await
        {
            Ok(_) => Ok(()),
            Err(e) => {
                let already = e
                    .as_service_error()
                    .map(|svc| {
                        svc.is_bucket_already_owned_by_you() || svc.is_bucket_already_exists()
                    })
                    .unwrap_or(false);
                if already {
                    Ok(())
                } else {
                    Err(sdk_io_error("create_bucket", e))
                }
            }
        }
    }

    /// Object key for `key`: `<prefix>/tenants/<tenant>/<path>`.
    ///
    /// The tenant and path are already validated by [`Key::new`]
    /// (no `..`, no absolute paths, no backslashes), so this cannot
    /// be tricked into escaping the tenant subtree. The `prefix` is
    /// operator-supplied config, normalised to drop stray slashes so
    /// it can never inject an empty segment or a leading `/`.
    fn object_key(&self, key: &Key) -> String {
        let tenant_path = format!("tenants/{}/{}", key.tenant(), key.path());
        if self.prefix.is_empty() {
            tenant_path
        } else {
            format!("{}/{}", self.prefix, tenant_path)
        }
    }

    /// The `<prefix>/tenants/<tenant>/` portion for a list prefix,
    /// plus the per-tenant base used to strip results back to keys.
    fn list_prefix(&self, prefix: &Key) -> (String, String) {
        // Base under which a tenant's objects live, *with* trailing
        // slash so we can strip it off listed object keys.
        let tenant_base = if self.prefix.is_empty() {
            format!("tenants/{}/", prefix.tenant())
        } else {
            format!("{}/tenants/{}/", self.prefix, prefix.tenant())
        };
        let full = format!("{tenant_base}{}", prefix.path());
        (full, tenant_base)
    }
}

#[async_trait]
impl LaneStore for S3Store {
    async fn read(&self, key: &Key) -> Result<Bytes> {
        let object_key = self.object_key(key);
        match self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(&object_key)
            .send()
            .await
        {
            Ok(out) => {
                let data = out
                    .body
                    .collect()
                    .await
                    .map_err(|e| StoreError::Io(std::io::Error::other(e)))?;
                Ok(data.into_bytes())
            }
            Err(e) => {
                if is_get_not_found(&e) {
                    Err(StoreError::NotFound(key.clone()))
                } else {
                    Err(sdk_io_error("get_object", e))
                }
            }
        }
    }

    async fn write(&self, key: &Key, body: Bytes) -> Result<Version> {
        let object_key = self.object_key(key);

        // S3 PUT is atomic at the object level: a reader sees either
        // the prior object or the new one in full, never a partial
        // body. No temp-then-rename dance is needed (unlike the FS
        // backend, whose `rename(2)` provides that guarantee on a
        // mutable filesystem).
        let out = self
            .client
            .put_object()
            .bucket(&self.bucket)
            .key(&object_key)
            .body(ByteStream::from(body))
            .send()
            .await
            .map_err(|e| sdk_io_error("put_object", e))?;

        // Prefer the version-id (versioned buckets); fall back to the
        // etag, which every S3-compatible store returns. Either way
        // distinct writes of distinct content yield distinct versions.
        let version = out
            .version_id()
            .map(str::to_owned)
            .or_else(|| out.e_tag().map(|t| t.trim_matches('"').to_owned()))
            .unwrap_or_default();
        Ok(version)
    }

    async fn list(&self, prefix: &Key) -> Result<Vec<Key>> {
        let (full_prefix, tenant_base) = self.list_prefix(prefix);
        let tenant = prefix.tenant().to_owned();

        let mut keys = Vec::new();
        let mut continuation: Option<String> = None;

        loop {
            let mut req = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(&full_prefix);
            if let Some(token) = &continuation {
                req = req.continuation_token(token);
            }

            let out = req
                .send()
                .await
                .map_err(|e| sdk_io_error("list_objects_v2", e))?;

            for object in out.contents() {
                let Some(object_key) = object.key() else {
                    continue;
                };
                // Strip the `<prefix>/tenants/<tenant>/` base back off
                // to recover the tenant-relative path.
                let Some(rel) = object_key.strip_prefix(&tenant_base) else {
                    continue;
                };
                // Defensive: skip any object whose recovered path
                // fails `Key` validation rather than poisoning the
                // whole listing (mirrors FsStore::list_under).
                if let Ok(k) = Key::new(tenant.clone(), rel.to_owned()) {
                    keys.push(k);
                }
            }

            if out.is_truncated().unwrap_or(false) {
                continuation = out.next_continuation_token().map(str::to_owned);
                if continuation.is_none() {
                    break;
                }
            } else {
                break;
            }
        }

        Ok(keys)
    }

    async fn delete(&self, key: &Key) -> Result<()> {
        let object_key = self.object_key(key);

        // S3 DeleteObject is idempotent — deleting a missing key
        // returns 204, not 404. To honour the trait's NotFound
        // contract (parity with FsStore) we HEAD first.
        match self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(&object_key)
            .send()
            .await
        {
            Ok(_) => {}
            Err(e) => {
                return if is_head_not_found(&e) {
                    Err(StoreError::NotFound(key.clone()))
                } else {
                    Err(sdk_io_error("head_object", e))
                };
            }
        }

        self.client
            .delete_object()
            .bucket(&self.bucket)
            .key(&object_key)
            .send()
            .await
            .map_err(|e| sdk_io_error("delete_object", e))?;
        Ok(())
    }

    fn url(&self, key: &Key) -> Result<Url> {
        // s3://<bucket>/<prefix>/tenants/<tenant>/<path>
        let object_key = self.object_key(key);
        let raw = format!("s3://{}/{}", self.bucket, object_key);
        Url::parse(&raw).map_err(|_| StoreError::InvalidFileUrl(key.clone()))
    }
}

/// Drop leading/trailing slashes from an operator-supplied prefix so
/// it can never inject an empty path segment or an absolute key.
fn normalise_prefix(prefix: &str) -> String {
    prefix.trim_matches('/').to_owned()
}

/// True if a `get_object` error is a missing-key (404 / NoSuchKey).
fn is_get_not_found(e: &SdkError<GetObjectError>) -> bool {
    matches!(e.as_service_error(), Some(svc) if svc.is_no_such_key())
}

/// True if a `head_object` error is a missing-key. HEAD returns no
/// typed `NoSuchKey` variant (the body is empty), so match on the
/// 404 status of the raw HTTP response.
fn is_head_not_found(e: &SdkError<HeadObjectError>) -> bool {
    e.raw_response()
        .map(|resp| resp.status().as_u16() == 404)
        .unwrap_or(false)
}

/// Wrap an AWS SDK error as a `StoreError::Io` with operation context.
fn sdk_io_error<E>(op: &str, e: E) -> StoreError
where
    E: std::error::Error + Send + Sync + 'static,
{
    StoreError::Io(std::io::Error::other(format!("s3 {op}: {e}")))
}
