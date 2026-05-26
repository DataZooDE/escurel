//! JWKS fetch + in-memory cache.
//!
//! Caches `kid → DecodingKey` with a TTL. On a `kid` miss the cache
//! refreshes the JWKS once and retries; if still missing, the token
//! is rejected as `UnknownKid`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use jsonwebtoken::DecodingKey;
use serde::Deserialize;
use tokio::sync::RwLock;

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
}

/// In-memory JWKS cache. Cloneable; shared across requests by
/// holding `Arc<JwksCache>`.
#[derive(Debug, Clone)]
pub struct JwksCache {
    jwks_uri: String,
    ttl: Duration,
    state: Arc<RwLock<CacheState>>,
    client: reqwest::Client,
}

#[derive(Default)]
struct CacheState {
    keys: HashMap<String, DecodingKey>,
    fetched_at: Option<Instant>,
}

impl std::fmt::Debug for CacheState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CacheState")
            .field("key_count", &self.keys.len())
            .field("fetched_at", &self.fetched_at)
            .finish()
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
            state: Arc::new(RwLock::new(CacheState::default())),
            client: reqwest::Client::new(),
        }
    }

    /// Look up a decoding key by `kid`. Refreshes the JWKS on
    /// miss or when the TTL has expired.
    pub async fn key_for_kid(&self, kid: &str) -> Result<DecodingKey, JwksCacheError> {
        // Fast path: TTL-fresh cached lookup.
        {
            let state = self.state.read().await;
            if let Some(fetched_at) = state.fetched_at
                && fetched_at.elapsed() < self.ttl
                && let Some(k) = state.keys.get(kid)
            {
                return Ok(k.clone());
            }
        }
        // Slow path: refresh, then look up.
        self.refresh().await?;
        let state = self.state.read().await;
        state
            .keys
            .get(kid)
            .cloned()
            .ok_or_else(|| JwksCacheError::UnknownKid(kid.to_owned()))
    }

    /// Force a refresh of the JWKS, replacing the cached set.
    pub async fn refresh(&self) -> Result<(), JwksCacheError> {
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
                Ok(k) => {
                    new_keys.insert(kid, k);
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

        let mut state = self.state.write().await;
        state.keys = new_keys;
        state.fetched_at = Some(Instant::now());
        Ok(())
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
