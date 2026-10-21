//! `RetryingStore` against a real store behind a fault injector.
//!
//! The faults are injected at the `LaneStore` boundary (a real `FsStore` that fails its first N calls
//! with the text a Drive lane produces); the retry loop under test is the production one.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bytes::Bytes;
use escurel_storage::{FsStore, Key, LaneStore, Result, RetryPolicy, RetryingStore, StoreError};
use tempfile::TempDir;
use url::Url;

const TRANSIENT: &str =
    "duckvfs write: IO Error: temporary Drive API failure while accessing gdrive://x";

/// Fails the first `fail` mutating/reading calls with `message`, then forwards to the real store.
struct Flaky {
    inner: FsStore,
    fail: AtomicUsize,
    calls: AtomicUsize,
    message: String,
}

impl Flaky {
    fn new(dir: &TempDir, fail: usize, message: &str) -> Arc<Self> {
        Arc::new(Self {
            inner: FsStore::new(dir.path().to_path_buf()),
            fail: AtomicUsize::new(fail),
            calls: AtomicUsize::new(0),
            message: message.to_owned(),
        })
    }
    fn gate(&self) -> Result<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        loop {
            let n = self.fail.load(Ordering::SeqCst);
            if n == 0 {
                return Ok(());
            }
            if self
                .fail
                .compare_exchange(n, n - 1, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                return Err(StoreError::Io(std::io::Error::other(self.message.clone())));
            }
        }
    }
}

#[async_trait]
impl LaneStore for Flaky {
    async fn read(&self, key: &Key) -> Result<Bytes> {
        self.gate()?;
        self.inner.read(key).await
    }
    async fn write(&self, key: &Key, body: Bytes) -> Result<escurel_storage::Version> {
        self.gate()?;
        self.inner.write(key, body).await
    }
    async fn list(&self, prefix: &Key) -> Result<Vec<Key>> {
        self.gate()?;
        self.inner.list(prefix).await
    }
    async fn delete(&self, key: &Key) -> Result<()> {
        self.gate()?;
        self.inner.delete(key).await
    }
    fn url(&self, key: &Key) -> Result<Url> {
        self.inner.url(key)
    }
}

fn fast(attempts: u32) -> RetryPolicy {
    RetryPolicy {
        attempts,
        initial_backoff: Duration::from_millis(2),
        max_backoff: Duration::from_millis(10),
        total: Duration::from_secs(5),
    }
}

fn key(p: &str) -> Key {
    Key::new("acme", p.to_owned()).unwrap()
}

#[tokio::test]
async fn a_write_that_fails_transiently_then_succeeds_is_retried() {
    let dir = TempDir::new().unwrap();
    let flaky = Flaky::new(&dir, 3, TRANSIENT);
    let store = RetryingStore::new(flaky.clone(), fast(6));

    store
        .write(&key("markdown/a.md"), Bytes::from_static(b"hello"))
        .await
        .expect("succeeds on the 4th try");

    assert_eq!(
        flaky.calls.load(Ordering::SeqCst),
        4,
        "3 failures + 1 success"
    );
    assert_eq!(
        store.read(&key("markdown/a.md")).await.unwrap().as_ref(),
        b"hello"
    );
}

#[tokio::test]
async fn reads_lists_and_deletes_are_retried_too() {
    let dir = TempDir::new().unwrap();
    let flaky = Flaky::new(&dir, 0, TRANSIENT);
    let store = RetryingStore::new(flaky.clone(), fast(6));
    store
        .write(&key("markdown/a.md"), Bytes::from_static(b"x"))
        .await
        .unwrap();

    flaky.fail.store(2, Ordering::SeqCst);
    assert_eq!(
        store.read(&key("markdown/a.md")).await.unwrap().as_ref(),
        b"x"
    );
    flaky.fail.store(2, Ordering::SeqCst);
    let listed = store.list(&key("markdown")).await.unwrap();
    assert_eq!(listed.len(), 1);
    flaky.fail.store(2, Ordering::SeqCst);
    store.delete(&key("markdown/a.md")).await.unwrap();
}

#[tokio::test]
async fn a_permanent_error_is_returned_at_once_without_retrying() {
    let dir = TempDir::new().unwrap();
    let flaky = Flaky::new(&dir, 100, "403 Forbidden: insufficient permission");
    let store = RetryingStore::new(flaky.clone(), fast(6));

    let err = store
        .write(&key("markdown/a.md"), Bytes::from_static(b"x"))
        .await
        .expect_err("a permission error is permanent");

    assert!(matches!(err, StoreError::Io(_)));
    assert_eq!(flaky.calls.load(Ordering::SeqCst), 1, "one try, no retry");
}

#[tokio::test]
async fn not_found_is_never_retried() {
    let dir = TempDir::new().unwrap();
    let flaky = Flaky::new(&dir, 0, TRANSIENT);
    let store = RetryingStore::new(flaky.clone(), fast(6));
    let err = store.read(&key("markdown/missing.md")).await.unwrap_err();
    assert!(matches!(err, StoreError::NotFound(_)));
    assert_eq!(flaky.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn the_attempt_limit_is_honoured_and_the_last_error_is_returned() {
    let dir = TempDir::new().unwrap();
    let flaky = Flaky::new(&dir, usize::MAX, TRANSIENT);
    let store = RetryingStore::new(flaky.clone(), fast(4));

    let err = store
        .write(&key("markdown/a.md"), Bytes::from_static(b"x"))
        .await
        .expect_err("never recovers");

    assert!(err.to_string().contains("temporary Drive API failure"));
    assert_eq!(
        flaky.calls.load(Ordering::SeqCst),
        4,
        "exactly `attempts` tries"
    );
}

#[tokio::test]
async fn one_attempt_means_no_retry() {
    let dir = TempDir::new().unwrap();
    let flaky = Flaky::new(&dir, 1, TRANSIENT);
    let store = RetryingStore::new(flaky.clone(), RetryPolicy::none());
    store
        .write(&key("markdown/a.md"), Bytes::from_static(b"x"))
        .await
        .expect_err("a single try fails");
    assert_eq!(flaky.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn the_total_time_cap_stops_the_retries() {
    let dir = TempDir::new().unwrap();
    let flaky = Flaky::new(&dir, usize::MAX, TRANSIENT);
    // Plenty of attempts but only ~60 ms of total budget: the cap, not the attempt count, ends it.
    let store = RetryingStore::new(
        flaky.clone(),
        RetryPolicy {
            attempts: 1_000,
            initial_backoff: Duration::from_millis(20),
            max_backoff: Duration::from_millis(20),
            total: Duration::from_millis(60),
        },
    );

    let started = Instant::now();
    store
        .write(&key("markdown/a.md"), Bytes::from_static(b"x"))
        .await
        .expect_err("never recovers");

    assert!(
        started.elapsed() < Duration::from_millis(500),
        "bounded by `total`, took {:?}",
        started.elapsed()
    );
    let calls = flaky.calls.load(Ordering::SeqCst);
    assert!(
        (2..20).contains(&calls),
        "a few tries, not a thousand ({calls})"
    );
}

#[tokio::test]
async fn a_retried_delete_that_already_took_effect_is_a_success() {
    let dir = TempDir::new().unwrap();
    let inner = FsStore::new(dir.path().to_path_buf());
    inner
        .write(&key("markdown/a.md"), Bytes::from_static(b"x"))
        .await
        .unwrap();

    /// Deletes, then reports a transient failure (the answer was lost), so the retry finds nothing.
    struct LostAck {
        inner: FsStore,
        first: AtomicUsize,
    }
    #[async_trait]
    impl LaneStore for LostAck {
        async fn read(&self, key: &Key) -> Result<Bytes> {
            self.inner.read(key).await
        }
        async fn write(&self, key: &Key, body: Bytes) -> Result<escurel_storage::Version> {
            self.inner.write(key, body).await
        }
        async fn list(&self, prefix: &Key) -> Result<Vec<Key>> {
            self.inner.list(prefix).await
        }
        async fn delete(&self, key: &Key) -> Result<()> {
            self.inner.delete(key).await?;
            if self.first.fetch_add(1, Ordering::SeqCst) == 0 {
                return Err(StoreError::Io(std::io::Error::other(TRANSIENT)));
            }
            Ok(())
        }
        fn url(&self, key: &Key) -> Result<Url> {
            self.inner.url(key)
        }
    }

    let store = RetryingStore::new(
        Arc::new(LostAck {
            inner,
            first: AtomicUsize::new(0),
        }),
        fast(4),
    );
    store
        .delete(&key("markdown/a.md"))
        .await
        .expect("the delete had already happened");
}
