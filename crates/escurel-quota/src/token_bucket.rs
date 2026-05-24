//! Classic token bucket: continuous refill, integer tokens,
//! `try_consume(n)` returns `QuotaExhausted` (with a retry-after
//! hint) when the bucket lacks the demanded count.

use std::sync::Mutex;
use std::time::Instant;

use thiserror::Error;

/// One token bucket: a `capacity` ceiling refilled at
/// `refill_per_sec` tokens/sec. `try_consume(n)` is O(1) and
/// thread-safe via an internal mutex.
#[derive(Debug)]
pub struct TokenBucket {
    capacity: f64,
    refill_per_sec: f64,
    state: Mutex<State>,
}

#[derive(Debug)]
struct State {
    tokens: f64,
    last_refill: Instant,
}

impl TokenBucket {
    /// Build a bucket with the given per-minute rate. Capacity
    /// equals the rate (so a burst can drain the bucket without
    /// throttling, but the steady-state cap is the per-minute
    /// number).
    #[must_use]
    pub fn per_minute(rate_per_minute: u32) -> Self {
        let capacity = f64::from(rate_per_minute);
        let refill_per_sec = capacity / 60.0;
        Self {
            capacity,
            refill_per_sec,
            state: Mutex::new(State {
                tokens: capacity,
                last_refill: Instant::now(),
            }),
        }
    }

    /// Build a bucket with explicit capacity + refill rate.
    #[must_use]
    pub fn new(capacity: u32, refill_per_sec: f64) -> Self {
        Self {
            capacity: f64::from(capacity),
            refill_per_sec,
            state: Mutex::new(State {
                tokens: f64::from(capacity),
                last_refill: Instant::now(),
            }),
        }
    }

    /// Try to take `n` tokens. Returns `Ok(())` on success,
    /// `Err(QuotaExhausted { retry_after_ms })` when fewer than
    /// `n` tokens are available; `retry_after_ms` is how long the
    /// caller should wait before the bucket holds `n` again.
    pub fn try_consume(&self, n: u32) -> Result<(), QuotaExhausted> {
        let n = f64::from(n);
        let mut state = self.state.lock().expect("token bucket mutex");
        let now = Instant::now();
        let elapsed = now.duration_since(state.last_refill).as_secs_f64();
        state.tokens = (state.tokens + elapsed * self.refill_per_sec).min(self.capacity);
        state.last_refill = now;

        if state.tokens >= n {
            state.tokens -= n;
            Ok(())
        } else {
            let deficit = n - state.tokens;
            // retry_after assumes the caller wants enough tokens
            // for exactly this single request.
            let wait_secs = if self.refill_per_sec > 0.0 {
                deficit / self.refill_per_sec
            } else {
                f64::INFINITY
            };
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let retry_after_ms = (wait_secs * 1000.0).ceil() as u64;
            Err(QuotaExhausted { retry_after_ms })
        }
    }

    /// Current token count (best-effort, for tests + metrics).
    #[must_use]
    pub fn available(&self) -> f64 {
        let mut state = self.state.lock().expect("token bucket mutex");
        let now = Instant::now();
        let elapsed = now.duration_since(state.last_refill).as_secs_f64();
        state.tokens = (state.tokens + elapsed * self.refill_per_sec).min(self.capacity);
        state.last_refill = now;
        state.tokens
    }
}

/// Returned by [`TokenBucket::try_consume`] when the bucket is dry.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[error("quota exhausted; retry after {retry_after_ms} ms")]
pub struct QuotaExhausted {
    pub retry_after_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;
    use std::time::Duration;

    #[test]
    fn fresh_bucket_serves_up_to_capacity() {
        let b = TokenBucket::per_minute(60);
        for _ in 0..60 {
            b.try_consume(1).expect("under capacity");
        }
        assert!(b.try_consume(1).is_err(), "61st must exhaust");
    }

    #[test]
    fn exhausted_bucket_returns_finite_retry_after() {
        let b = TokenBucket::per_minute(60);
        for _ in 0..60 {
            b.try_consume(1).unwrap();
        }
        let err = b.try_consume(1).unwrap_err();
        assert!(err.retry_after_ms > 0);
        assert!(err.retry_after_ms < 2_000, "wait should be ~1s for 60/min");
    }

    #[test]
    fn bucket_refills_over_time() {
        let b = TokenBucket::per_minute(6_000); // 100/s
        b.try_consume(6_000).unwrap();
        assert!(b.try_consume(1).is_err());
        sleep(Duration::from_millis(50));
        // After 50ms at 100 tok/sec the bucket should have ~5 tokens.
        b.try_consume(3).expect("refilled");
    }
}
