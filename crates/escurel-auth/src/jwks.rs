//! JWKS fetch + in-memory cache.
//!
//! Caches `kid → DecodingKey` with a TTL. On a `kid` miss the cache
//! refreshes the JWKS once and retries; if still missing, the token
//! is rejected as `UnknownKid`. Refreshes are single-flight (concurrent
//! misses share one fetch) and a miss within [`MIN_REFRESH_INTERVAL`] of the
//! last attempt is answered from that attempt, without fetching.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use jsonwebtoken::DecodingKey;
use serde::Deserialize;
use tokio::sync::{Mutex, RwLock};

/// Result of one JWKS fetch (raw JSON shape, before we convert to
/// `DecodingKey`s). Exposed for tests that want to assert on the
/// wire shape.
#[derive(Debug, Clone, Deserialize)]
pub struct Jwks {
    pub keys: Vec<JwkRaw>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct JwkRaw {
    pub kid: String,
    pub kty: String,
    #[serde(default)]
    pub alg: Option<String>,
    /// RSA modulus (base64url).
    #[serde(default)]
    pub n: Option<String>,
    /// RSA exponent (base64url).
    #[serde(default)]
    pub e: Option<String>,
    /// EC curve (when kty = "EC").
    #[serde(default)]
    pub crv: Option<String>,
    /// EC x-coordinate (base64url).
    #[serde(default)]
    pub x: Option<String>,
    /// EC y-coordinate (base64url).
    #[serde(default)]
    pub y: Option<String>,
    /// The channels this key may sign for — a Bot Framework extension: its
    /// JWKS lists, per key, the channel ids (`msteams`, …) the key endorses.
    #[serde(default)]
    pub endorsements: Option<Vec<String>>,
}

/// `reqwest` has no default request timeout — see the substrate finding
/// (issue #569) that a bare `Client::new()` on a similar external call
/// (Gemini's embed API) hung a whole pod's request handling until kubelet
/// force-restarted it. This client sits on the AUTHENTICATED-REQUEST path
/// (every request behind an expired TTL can trigger a refresh), so an
/// unbounded hang here is the same failure mode with a wider blast radius.
/// 10s matches `remote_backend.rs`'s existing precedent for a comparably
/// small, fast external call.
const JWKS_FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// The least time between two fetches an unknown kid triggers (crew security
/// F3). A kid nobody publishes must not cost a fetch per request: the chat
/// bridge looks kids up on a public endpoint before any signature check.
/// Capped by the TTL, so a cache configured to refresh more often still does.
pub const MIN_REFRESH_INTERVAL: Duration = Duration::from_secs(30);

/// In-memory JWKS cache. Cloneable; shared across requests by
/// holding `Arc<JwksCache>`.
#[derive(Debug, Clone)]
pub struct JwksCache {
    jwks_uri: String,
    ttl: Duration,
    min_refresh_interval: Duration,
    state: Arc<RwLock<CacheState>>,
    /// Held for the duration of a fetch: concurrent misses queue here and
    /// then read the outcome instead of fetching again.
    in_flight: Arc<Mutex<()>>,
    client: reqwest::Client,
}

struct CachedKey {
    key: DecodingKey,
    endorsements: Option<Vec<String>>,
}

#[derive(Default)]
struct CacheState {
    keys: HashMap<String, CachedKey>,
    fetched_at: Option<Instant>,
    /// Fetch attempts so far, successful or not: a waiter that sees it move
    /// knows someone fetched while it queued.
    attempts: u64,
    attempted_at: Option<Instant>,
    /// Why the last attempt failed; `None` after a success.
    last_error: Option<String>,
}

impl CacheState {
    /// The answer for a kid the last attempt did not produce.
    fn miss(&self, kid: &str) -> JwksCacheError {
        match &self.last_error {
            Some(e) => JwksCacheError::Fetch(e.clone()),
            None => JwksCacheError::UnknownKid(kid.to_owned()),
        }
    }
}

impl std::fmt::Debug for CacheState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CacheState")
            .field("key_count", &self.keys.len())
            .field("fetched_at", &self.fetched_at)
            .field("attempts", &self.attempts)
            .finish_non_exhaustive()
    }
}

impl JwksCache {
    /// Build a cache backed by `jwks_uri`. Caller supplies the
    /// refresh TTL; once a refresh happens, subsequent calls
    /// within the TTL window hit memory without touching the
    /// network.
    #[must_use]
    pub fn new(jwks_uri: impl Into<String>, ttl: Duration) -> Self {
        Self {
            jwks_uri: jwks_uri.into(),
            ttl,
            min_refresh_interval: MIN_REFRESH_INTERVAL,
            state: Arc::new(RwLock::new(CacheState::default())),
            in_flight: Arc::new(Mutex::new(())),
            client: reqwest::Client::builder()
                .timeout(JWKS_FETCH_TIMEOUT)
                .build()
                .unwrap_or_default(),
        }
    }

    /// Override the fetch timeout ([`JWKS_FETCH_TIMEOUT`] by default).
    /// Exposed so a test can prove a hung JWKS endpoint actually errors
    /// out instead of blocking forever, without a real 10s-long test.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .unwrap_or_default();
        self
    }

    /// Override the least time between two fetches triggered by an unknown
    /// kid ([`MIN_REFRESH_INTERVAL`] by default).
    #[must_use]
    pub fn with_min_refresh_interval(mut self, interval: Duration) -> Self {
        self.min_refresh_interval = interval;
        self
    }

    /// Look up a decoding key by `kid`. Refreshes the JWKS on
    /// miss or when the TTL has expired.
    pub async fn key_for_kid(&self, kid: &str) -> Result<DecodingKey, JwksCacheError> {
        Ok(self.endorsed_key_for_kid(kid).await?.0)
    }

    /// [`Self::key_for_kid`] plus the key's `endorsements` as the JWKS
    /// published them (`None` when it published none).
    pub async fn endorsed_key_for_kid(
        &self,
        kid: &str,
    ) -> Result<(DecodingKey, Option<Vec<String>>), JwksCacheError> {
        let found = |state: &CacheState| {
            state
                .keys
                .get(kid)
                .map(|k| (k.key.clone(), k.endorsements.clone()))
        };
        // Fast path: TTL-fresh cached lookup.
        {
            let state = self.state.read().await;
            if let Some(fetched_at) = state.fetched_at
                && fetched_at.elapsed() < self.ttl
                && let Some(k) = found(&state)
            {
                return Ok(k);
            }
        }
        // Slow path, single-flight: queue behind any fetch in progress.
        let seen = self.state.read().await.attempts;
        let _flight = self.in_flight.lock().await;
        {
            let state = self.state.read().await;
            // Someone fetched while we queued: their outcome is ours.
            if state.attempts != seen {
                return found(&state).ok_or_else(|| state.miss(kid));
            }
            // An unknown kid right after an attempt is still unknown: a kid
            // nobody publishes must not cost a fetch per request. A known kid
            // whose TTL lapsed is refreshed as before.
            let cooldown = self.min_refresh_interval.min(self.ttl);
            if found(&state).is_none() && state.attempted_at.is_some_and(|t| t.elapsed() < cooldown)
            {
                return Err(state.miss(kid));
            }
        }
        self.fetch().await?;
        let state = self.state.read().await;
        found(&state).ok_or_else(|| JwksCacheError::UnknownKid(kid.to_owned()))
    }

    /// Force a refresh of the JWKS, replacing the cached set.
    pub async fn refresh(&self) -> Result<(), JwksCacheError> {
        let _flight = self.in_flight.lock().await;
        self.fetch().await
    }

    /// One fetch, recorded as an attempt. The caller holds `in_flight`.
    async fn fetch(&self) -> Result<(), JwksCacheError> {
        let outcome = self.fetch_keys().await;
        let mut state = self.state.write().await;
        state.attempts += 1;
        state.attempted_at = Some(Instant::now());
        match outcome {
            Ok(keys) => {
                state.keys = keys;
                state.fetched_at = state.attempted_at;
                state.last_error = None;
                Ok(())
            }
            Err(e) => {
                state.last_error = Some(match &e {
                    JwksCacheError::Fetch(why) => why.clone(),
                    other => other.to_string(),
                });
                Err(e)
            }
        }
    }

    async fn fetch_keys(&self) -> Result<HashMap<String, CachedKey>, JwksCacheError> {
        let resp = self
            .client
            .get(&self.jwks_uri)
            .send()
            .await
            .map_err(|e| JwksCacheError::Fetch(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(JwksCacheError::Fetch(format!(
                "HTTP {} from {}",
                resp.status(),
                self.jwks_uri,
            )));
        }
        let jwks: Jwks = resp
            .json()
            .await
            .map_err(|e| JwksCacheError::Fetch(format!("parse JWKS: {e}")))?;

        let mut new_keys = HashMap::new();
        for jwk in jwks.keys {
            let kid = jwk.kid.clone();
            match decoding_key_from_jwk(&jwk) {
                Ok(key) => {
                    new_keys.insert(
                        kid,
                        CachedKey {
                            key,
                            endorsements: jwk.endorsements.clone(),
                        },
                    );
                }
                Err(_) => {
                    // Skip key shapes we don't support today; a
                    // legitimate issuer can host keys we can't
                    // decode (e.g. EC curves we don't enable).
                    // The verifier will report UnknownKid if a
                    // token references one of these.
                }
            }
        }

        Ok(new_keys)
    }
}

fn decoding_key_from_jwk(jwk: &JwkRaw) -> Result<DecodingKey, JwksCacheError> {
    match jwk.kty.as_str() {
        "RSA" => {
            let n = jwk
                .n
                .as_deref()
                .ok_or_else(|| JwksCacheError::UnsupportedKey("RSA missing n".to_owned()))?;
            let e = jwk
                .e
                .as_deref()
                .ok_or_else(|| JwksCacheError::UnsupportedKey("RSA missing e".to_owned()))?;
            DecodingKey::from_rsa_components(n, e)
                .map_err(|e| JwksCacheError::UnsupportedKey(format!("RSA: {e}")))
        }
        "EC" => {
            let crv = jwk
                .crv
                .as_deref()
                .ok_or_else(|| JwksCacheError::UnsupportedKey("EC missing crv".to_owned()))?;
            let x = jwk
                .x
                .as_deref()
                .ok_or_else(|| JwksCacheError::UnsupportedKey("EC missing x".to_owned()))?;
            let y = jwk
                .y
                .as_deref()
                .ok_or_else(|| JwksCacheError::UnsupportedKey("EC missing y".to_owned()))?;
            let _ = crv;
            DecodingKey::from_ec_components(x, y)
                .map_err(|e| JwksCacheError::UnsupportedKey(format!("EC: {e}")))
        }
        other => Err(JwksCacheError::UnsupportedKey(format!("kty={other}"))),
    }
}

/// Errors from [`JwksCache`].
#[derive(Debug, thiserror::Error)]
pub enum JwksCacheError {
    #[error("failed to fetch JWKS: {0}")]
    Fetch(String),
    #[error("unknown kid: {0}")]
    UnknownKid(String),
    #[error("unsupported key in JWKS: {0}")]
    UnsupportedKey(String),
}
