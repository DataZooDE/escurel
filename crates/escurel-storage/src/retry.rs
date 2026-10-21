//! Bounded retry for transient store errors.
//!
//! A Google Drive lane answers an occasional `temporary Drive API failure` (and rate limits, 5xx and
//! timeouts). Without a retry one such answer, anywhere in a boot that touches hundreds of lane objects,
//! was fatal: heron-escurel's pod died at minute 20 of a cold boot and started the 20 minutes over.
//! [`RetryingStore`] wraps any [`LaneStore`] and repeats an operation that failed with a TRANSIENT
//! error, with exponential backoff and jitter, under a hard cap on attempts and on total time. A
//! permanent error (not found, permission, quota, bad key) returns at once.
//!
//! The wrapper sits at the store boundary, so it is exercised with a real store behind a fault-injecting
//! store (`tests/retry.rs`), not by mocking the retry loop.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bytes::Bytes;
use url::Url;

use crate::{Key, LaneStore, Result, StoreError, Version};

/// How hard to retry. [`Default`] is what a Drive lane gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Total tries including the first; `1` disables retrying.
    pub attempts: u32,
    /// Backoff before the second try; doubles each time.
    pub initial_backoff: Duration,
    /// No single wait exceeds this.
    pub max_backoff: Duration,
    /// Once this much time has passed since the first try, the next failure is returned.
    pub total: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            attempts: 6,
            initial_backoff: Duration::from_millis(500),
            max_backoff: Duration::from_secs(30),
            total: Duration::from_secs(180),
        }
    }
}

impl RetryPolicy {
    /// No retries: every error is returned at once.
    pub const fn none() -> Self {
        Self {
            attempts: 1,
            initial_backoff: Duration::ZERO,
            max_backoff: Duration::ZERO,
            total: Duration::ZERO,
        }
    }

    /// The wait before try number `next_attempt` (2 for the first retry): `initial * 2^(n-2)` capped at
    /// `max_backoff`, then jittered into its upper half so concurrent callers do not retry in step.
    fn backoff(&self, next_attempt: u32) -> Duration {
        let exp = next_attempt.saturating_sub(2).min(30);
        let base = self
            .initial_backoff
            .saturating_mul(1u32 << exp)
            .min(self.max_backoff);
        let half = base / 2;
        half + (base - half).mul_f64(jitter_unit())
    }
}

/// A pseudo-random number in `[0, 1)`: a splitmix64 step over a process-wide counter seeded from the
/// clock. Jitter only has to decorrelate callers; it needs no cryptographic quality, and this avoids a
/// dependency.
fn jitter_unit() -> f64 {
    static STATE: AtomicU64 = AtomicU64::new(0);
    let mut s = STATE.load(Ordering::Relaxed);
    if s == 0 {
        s = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0x9E37_79B9_7F4A_7C15, |d| d.as_nanos() as u64)
            | 1;
    }
    s = s.wrapping_add(0x9E37_79B9_7F4A_7C15);
    STATE.store(s, Ordering::Relaxed);
    let mut z = s;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    (z >> 11) as f64 / (1u64 << 53) as f64
}

/// Messages that mark an error as worth repeating. Matched case-insensitively against the error text
/// (the DuckDB filesystem surfaces Drive/Graph failures only as text).
const TRANSIENT_MARKERS: &[&str] = &[
    "temporary",
    "rate limit",
    "ratelimit",
    "too many requests",
    "backend error",
    "backenderror",
    "internal error",
    "internal server error",
    "service unavailable",
    "bad gateway",
    "gateway timeout",
    "timed out",
    "timeout",
    "connection reset",
    "connection refused",
    "connection closed",
    "broken pipe",
    "try again",
];

/// HTTP statuses that mean "try again". Matched as whole numbers so an object id that happens to
/// contain `503` does not count.
const TRANSIENT_CODES: &[&str] = &["429", "500", "502", "503", "504"];

fn has_code(text: &str, code: &str) -> bool {
    let b = text.as_bytes();
    let mut from = 0;
    while let Some(i) = text[from..].find(code) {
        let s = from + i;
        let e = s + code.len();
        let before = s == 0 || !b[s - 1].is_ascii_alphanumeric();
        let after = e >= b.len() || !b[e].is_ascii_alphanumeric();
        if before && after {
            return true;
        }
        from = s + 1;
    }
    false
}

/// Why `err` is transient, or `None` when it is permanent (or not an I/O error at all).
pub fn transient_reason(err: &StoreError) -> Option<&'static str> {
    let StoreError::Io(io) = err else {
        return None;
    };
    let text = io.to_string().to_ascii_lowercase();
    // Permanent is the default: not found, permission, quota and bad-key errors carry none of these
    // markers. A rate limit is often a 403 whose text also says "forbidden": the marker wins.
    for m in TRANSIENT_MARKERS {
        if text.contains(m) {
            return Some(m);
        }
    }
    TRANSIENT_CODES.iter().find(|c| has_code(&text, c)).copied()
}

/// `true` when [`transient_reason`] finds one.
pub fn is_transient(err: &StoreError) -> bool {
    transient_reason(err).is_some()
}

/// A [`LaneStore`] that repeats transient failures of the store it wraps.
pub struct RetryingStore {
    inner: Arc<dyn LaneStore>,
    policy: RetryPolicy,
}

impl RetryingStore {
    pub fn new(inner: Arc<dyn LaneStore>, policy: RetryPolicy) -> Self {
        Self { inner, policy }
    }

    /// Run `op` until it succeeds, fails permanently, or the policy is spent. `key` is only for the
    /// log line (its tenant-relative path; never the URL or the error text).
    async fn run<T, F, Fut>(&self, name: &'static str, key: &Key, op: F) -> Result<T>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        let started = Instant::now();
        let mut attempt = 1u32;
        loop {
            let err = match op().await {
                Ok(v) => return Ok(v),
                Err(e) => e,
            };
            let Some(reason) = transient_reason(&err) else {
                return Err(err);
            };
            if attempt >= self.policy.attempts {
                return Err(err);
            }
            let wait = self.policy.backoff(attempt + 1);
            if started.elapsed() + wait > self.policy.total {
                return Err(err);
            }
            tracing::warn!(
                target: "escurel_storage::retry",
                op = name,
                path = key.path(),
                attempt,
                of = self.policy.attempts,
                wait_ms = wait.as_millis() as u64,
                reason,
                "transient lane store error, retrying"
            );
            tokio::time::sleep(wait).await;
            attempt += 1;
        }
    }
}

#[async_trait]
impl LaneStore for RetryingStore {
    async fn read(&self, key: &Key) -> Result<Bytes> {
        self.run("read", key, || self.inner.read(key)).await
    }

    async fn write(&self, key: &Key, body: Bytes) -> Result<Version> {
        self.run("write", key, || self.inner.write(key, body.clone()))
            .await
    }

    async fn list(&self, prefix: &Key) -> Result<Vec<Key>> {
        self.run("list", prefix, || self.inner.list(prefix)).await
    }

    async fn delete(&self, key: &Key) -> Result<()> {
        // A delete that failed transiently may have taken effect: its retry then finds nothing. That is
        // the outcome the caller asked for.
        let tried = std::sync::atomic::AtomicBool::new(false);
        self.run("delete", key, || {
            let again = tried.swap(true, Ordering::SeqCst);
            async move {
                match self.inner.delete(key).await {
                    Err(StoreError::NotFound(_)) if again => Ok(()),
                    other => other,
                }
            }
        })
        .await
    }

    fn url(&self, key: &Key) -> Result<Url> {
        self.inner.url(key)
    }

    fn backend(&self) -> &'static str {
        self.inner.backend()
    }

    async fn size(&self, key: &Key) -> Result<u64> {
        self.run("size", key, || self.inner.size(key)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn io(msg: &str) -> StoreError {
        StoreError::Io(std::io::Error::other(msg.to_owned()))
    }

    #[test]
    fn drive_and_http_transients_are_recognised() {
        for m in [
            "duckvfs write: IO Error: temporary Drive API failure while accessing gdrive://x",
            "HTTP 429 Too Many Requests",
            "userRateLimitExceeded",
            "Backend Error",
            "503 Service Unavailable",
            "request timed out",
            "Connection reset by peer",
        ] {
            assert!(is_transient(&io(m)), "{m}");
        }
    }

    #[test]
    fn permanent_errors_are_not_retried() {
        for m in [
            "File not found: gdrive://x",
            "403 Forbidden: insufficient permission",
            "storageQuotaExceeded",
            "invalid key",
            "object id 15031503 is fine",
        ] {
            assert!(!is_transient(&io(m)), "{m}");
        }
        let nf = StoreError::NotFound(Key::new("t", "a/b".to_owned()).unwrap());
        assert!(!is_transient(&nf));
    }

    #[test]
    fn a_status_code_inside_a_longer_number_does_not_count() {
        assert!(!is_transient(&io("object 1503x failed")));
        assert!(is_transient(&io("status 503")));
    }

    #[test]
    fn backoff_doubles_is_capped_and_stays_in_the_upper_half() {
        let p = RetryPolicy {
            attempts: 9,
            initial_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_millis(1_000),
            total: Duration::from_secs(60),
        };
        for _ in 0..200 {
            let w2 = p.backoff(2);
            assert!(w2 >= Duration::from_millis(50) && w2 <= Duration::from_millis(100));
            let w4 = p.backoff(4);
            assert!(w4 >= Duration::from_millis(200) && w4 <= Duration::from_millis(400));
            let w9 = p.backoff(9);
            assert!(w9 >= Duration::from_millis(500) && w9 <= Duration::from_millis(1_000));
        }
    }
}
