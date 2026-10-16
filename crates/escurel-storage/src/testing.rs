//! A real S3-compatible server in a container, for the live tests.
//!
//! One place that knows which server the tests use, so a registry change is a
//! one-line edit and not a grep through six test files. Used by this crate's
//! `s3_roundtrip` and by escurel-index's `live-ducklake` tests (via the
//! `test-s3-server` feature); both need Docker.
//!
//! The server is **RustFS**. MinIO — the original choice — stopped publishing
//! its images (Docker Hub, then quay.io: not even `latest` resolves), which
//! left the nightly `live` workflow red and the real-S3 coverage dark. The
//! comparison of the candidates lives in
//! `docs/notes/discovered/2026-10-16-minio-images-gone.md`.

use std::time::Duration;

use testcontainers_modules::testcontainers::{
    ContainerAsync, GenericImage, ImageExt,
    core::{ContainerPort, WaitFor, wait::HttpWaitStrategy},
    runners::AsyncRunner,
};

use crate::{S3Store, S3StoreConfig};

/// Pinned: the tests assert behaviour, so the server must not move under them.
pub const IMAGE: &str = "rustfs/rustfs";
pub const TAG: &str = "1.0.1";
/// The S3 API port inside the container.
const S3_PORT: u16 = 9000;

pub const ACCESS_KEY: &str = "escurel-test";
pub const SECRET_KEY: &str = "escurel-test-secret";
pub const REGION: &str = "us-east-1";

/// A running S3 server. Dropping it tears the container down.
pub struct S3Server {
    port: u16,
    _container: ContainerAsync<GenericImage>,
}

impl S3Server {
    /// Start the server and wait until it answers HTTP.
    pub async fn start() -> Self {
        let container = GenericImage::new(IMAGE, TAG)
            .with_exposed_port(ContainerPort::Tcp(S3_PORT))
            .with_wait_for(WaitFor::http(
                HttpWaitStrategy::new("/health").with_expected_status_code(200u16),
            ))
            .with_env_var("RUSTFS_ACCESS_KEY", ACCESS_KEY)
            .with_env_var("RUSTFS_SECRET_KEY", SECRET_KEY)
            .with_cmd(["/data"])
            .start()
            .await
            .unwrap_or_else(|err| panic!("start the S3 test server {IMAGE}:{TAG}: {err}"));
        let port = container
            .get_host_port_ipv4(S3_PORT)
            .await
            .expect("S3 test server port");
        Self {
            port,
            _container: container,
        }
    }

    /// `http://127.0.0.1:<port>`, for the AWS SDK.
    pub fn endpoint_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// `127.0.0.1:<port>` without a scheme, for DuckDB's S3 secret.
    pub fn endpoint_host_port(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }

    /// An `S3Store` on `bucket`/`prefix` with the bucket created. The server
    /// answers HTTP a moment before it accepts S3 calls, so the first bucket
    /// creation is retried.
    pub async fn store(&self, bucket: &str, prefix: &str) -> S3Store {
        let store = S3Store::new(S3StoreConfig {
            bucket: bucket.to_owned(),
            prefix: prefix.to_owned(),
            endpoint_url: self.endpoint_url(),
            region: REGION.to_owned(),
            access_key_id: ACCESS_KEY.to_owned(),
            secret_access_key: SECRET_KEY.to_owned(),
        })
        .await
        .expect("build S3Store");
        let mut last = None;
        for _ in 0..30 {
            match store.ensure_bucket().await {
                Ok(()) => return store,
                Err(err) => {
                    last = Some(err);
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
        panic!(
            "create bucket {bucket}: {}",
            last.expect("at least one try")
        );
    }
}
