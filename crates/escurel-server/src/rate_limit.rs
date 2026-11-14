//! Token buckets: the one rate limiter the gateway has.
//!
//! [`TokenBucket`] is a burst of `capacity` tokens refilled continuously at
//! `per_sec`. The egress policy keeps one per remote endpoint
//! ([`crate::egress::Egress::admit`]); `mint_user_token` keys them by bridge
//! credential and by asker ([`KeyedBuckets`], #835), so a misbehaving or
//! stolen bridge credential cannot flood minting or the group directories.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// A burst of `capacity` tokens, refilled at `per_sec`.
#[derive(Debug)]
pub struct TokenBucket {
    capacity: f64,
    per_sec: f64,
    /// Tokens left and when they were counted.
    state: Mutex<(f64, Instant)>,
}

impl TokenBucket {
    /// A full bucket. `capacity` and `per_sec` are floored at a tiny positive
    /// value, so a misconfigured zero never divides by zero.
    #[must_use]
    pub fn new(capacity: f64, per_sec: f64) -> Self {
        let capacity = capacity.max(1.0);
        Self {
            capacity,
            per_sec: per_sec.max(f64::MIN_POSITIVE),
            state: Mutex::new((capacity, Instant::now())),
        }
    }

    /// Take one token.
    ///
    /// # Errors
    /// The bucket is empty: how long until a token is back.
    pub fn take(&self) -> Result<(), Duration> {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        s.0 = (s.0 + now.duration_since(s.1).as_secs_f64() * self.per_sec).min(self.capacity);
        s.1 = now;
        if s.0 < 1.0 {
            return Err(Duration::from_secs_f64((1.0 - s.0) / self.per_sec));
        }
        s.0 -= 1.0;
        Ok(())
    }

    /// Whether the bucket has refilled completely: forgetting it now loses
    /// nothing, a fresh one would be the same.
    fn is_full(&self) -> bool {
        let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.0 + s.1.elapsed().as_secs_f64() * self.per_sec >= self.capacity
    }
}

/// One [`TokenBucket`] per key, `per_minute` each, over at most `max_keys`
/// keys. A full bucket is forgotten first (losslessly); past the bound the
/// longest-idle one goes.
pub struct KeyedBuckets {
    per_minute: u32,
    max_keys: usize,
    buckets: Mutex<HashMap<String, (TokenBucket, Instant)>>,
}

impl KeyedBuckets {
    #[must_use]
    pub fn new(per_minute: u32, max_keys: usize) -> Self {
        Self {
            per_minute,
            max_keys: max_keys.max(1),
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Take one token from `key`'s bucket.
    ///
    /// # Errors
    /// The bucket is empty: how long until a token is back.
    pub fn take(&self, key: &str) -> Result<(), Duration> {
        let mut map = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        if !map.contains_key(key) && map.len() >= self.max_keys {
            map.retain(|_, (b, _)| !b.is_full());
            if map.len() >= self.max_keys
                && let Some(idle) = map
                    .iter()
                    .min_by_key(|(_, (_, used))| *used)
                    .map(|(k, _)| k.clone())
            {
                map.remove(&idle);
            }
        }
        let n = f64::from(self.per_minute);
        let (bucket, used) = map
            .entry(key.to_owned())
            .or_insert_with(|| (TokenBucket::new(n, n / 60.0), Instant::now()));
        *used = Instant::now();
        bucket.take()
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.buckets.lock().unwrap().len()
    }
}

/// `mint_user_token`'s caps (#835): `ESCUREL_AUTH_BRIDGE_MINT_PER_MINUTE` per
/// bridge credential and `ESCUREL_AUTH_BRIDGE_MINT_PER_ASKER_PER_MINUTE` per
/// asker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MintRateLimits {
    pub per_credential_per_minute: u32,
    pub per_asker_per_minute: u32,
}

impl Default for MintRateLimits {
    fn default() -> Self {
        Self {
            per_credential_per_minute: 600,
            per_asker_per_minute: 30,
        }
    }
}

/// The most bridge credentials / askers a mint limiter tracks at once.
const MAX_MINT_KEYS: usize = 10_000;

/// One bridge credential minting for more distinct askers than this in a
/// minute is logged (crew F5): a captured activity token plus a stolen
/// credential reaches every roster member, so a sweep across people is the
/// signal worth an alert.
pub const MANY_ASKERS_PER_MINUTE: usize = 50;

/// The built caps: one bucket set per credential, one per asker.
pub struct MintLimiter {
    credentials: KeyedBuckets,
    askers: KeyedBuckets,
    spread: AskerSpread,
}

/// Distinct askers per credential in the current minute, bounded: at most
/// [`MAX_MINT_KEYS`] credentials, and no set grows past the threshold.
#[derive(Default)]
struct AskerSpread {
    windows: Mutex<HashMap<String, (Instant, std::collections::HashSet<String>)>>,
}

impl AskerSpread {
    /// Count `asker` for `credential`; `true` exactly when this mint takes
    /// the minute past [`MANY_ASKERS_PER_MINUTE`].
    fn note(&self, credential: &str, asker: &str) -> bool {
        let mut map = self.windows.lock().unwrap_or_else(|e| e.into_inner());
        if !map.contains_key(credential) && map.len() >= MAX_MINT_KEYS {
            map.retain(|_, (start, _)| start.elapsed() < Duration::from_secs(60));
            if map.len() >= MAX_MINT_KEYS {
                return false;
            }
        }
        let (start, seen) = map
            .entry(credential.to_owned())
            .or_insert_with(|| (Instant::now(), std::collections::HashSet::new()));
        if start.elapsed() >= Duration::from_secs(60) {
            *start = Instant::now();
            seen.clear();
        }
        if seen.len() > MANY_ASKERS_PER_MINUTE || seen.contains(asker) {
            return false;
        }
        seen.insert(asker.to_owned());
        seen.len() == MANY_ASKERS_PER_MINUTE + 1
    }
}

impl MintLimiter {
    #[must_use]
    pub fn new(limits: MintRateLimits) -> Self {
        Self {
            credentials: KeyedBuckets::new(limits.per_credential_per_minute, MAX_MINT_KEYS),
            askers: KeyedBuckets::new(limits.per_asker_per_minute, MAX_MINT_KEYS),
            spread: AskerSpread::default(),
        }
    }

    /// # Errors
    /// The credential's bucket is empty: when to try again.
    pub fn admit_credential(&self, subject: &str) -> Result<(), Duration> {
        self.credentials.take(subject)
    }

    /// # Errors
    /// The asker's bucket is empty: when to try again.
    pub fn admit_asker(&self, principal: &str) -> Result<(), Duration> {
        self.askers.take(principal)
    }

    /// Record that `credential` minted for `asker`; WARN once a minute in
    /// which it mints for more than [`MANY_ASKERS_PER_MINUTE`] people.
    pub fn note_asker(&self, credential: &str, asker: &str) {
        if self.spread.note(credential, asker) {
            tracing::warn!(bridge = %credential, askers = MANY_ASKERS_PER_MINUTE + 1,
                "mint_user_token: one bridge credential minted for many distinct askers within a \
                 minute; if that is not a busy bridge, rotate its credential");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bucket_gives_its_burst_then_says_when_to_come_back() {
        let b = TokenBucket::new(2.0, 1.0 / 60.0);
        assert!(b.take().is_ok());
        assert!(b.take().is_ok());
        let wait = b.take().unwrap_err();
        assert!(
            wait > Duration::from_secs(55) && wait <= Duration::from_secs(60),
            "{wait:?}"
        );
    }

    #[test]
    fn many_distinct_askers_on_one_credential_trip_once_per_minute() {
        let s = AskerSpread::default();
        for i in 0..MANY_ASKERS_PER_MINUTE {
            assert!(!s.note("bridge", &format!("user:{i}")));
            assert!(
                !s.note("bridge", &format!("user:{i}")),
                "a repeat is not new"
            );
        }
        assert!(s.note("bridge", "user:one-too-many"));
        assert!(!s.note("bridge", "user:and-another"), "once per minute");
        assert!(!s.note("other-bridge", "user:0"));
    }

    #[test]
    fn keys_have_their_own_buckets_and_the_key_set_stays_bounded() {
        let k = KeyedBuckets::new(1, 2);
        assert!(k.take("a").is_ok());
        assert!(k.take("a").is_err());
        assert!(k.take("b").is_ok());
        // A third key evicts the longest-idle one (no bucket is full yet).
        assert!(k.take("c").is_ok());
        assert_eq!(k.len(), 2);
    }
}
