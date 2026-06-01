//! `OidcVerifier`: validate a bearer JWT against the configured
//! issuer and project it into an [`AuthContext`].

use std::time::Duration;

use jsonwebtoken::{Algorithm, Validation, decode, decode_header};
use serde::Deserialize;

use crate::jwks::{JwksCache, JwksCacheError};

/// Operator-supplied OIDC configuration. Mirrors
/// `docs/spec/platform.md §Auth`.
#[derive(Debug, Clone)]
pub struct OidcConfig {
    /// `https://auth.example.com/realms/main`. The verifier
    /// appends `/protocol/openid-connect/certs` only when no
    /// explicit `jwks_uri` is supplied via [`OidcConfig::jwks_uri`].
    pub issuer: String,
    pub audience: String,
    /// JWT claim name that carries the tenant id. Default `"tenant"`.
    pub tenant_claim: String,
    /// JWT claim name that lists the subject's roles. Default
    /// `"roles"`.
    pub admin_role_claim: String,
    /// The role value that grants admin access. Default
    /// `"escurel:admin"`.
    pub admin_role_value: String,
    /// TTL for the in-memory JWKS cache.
    pub jwks_refresh: Duration,
    /// Optional override for the JWKS URL. When `None`, the
    /// verifier constructs `${issuer}/protocol/openid-connect/certs`
    /// (the Keycloak convention).
    pub jwks_uri: Option<String>,
}

impl OidcConfig {
    /// Defaults matching the substrate binding
    /// (`docs/deploy/substrate.md §1`).
    #[must_use]
    pub fn new(issuer: impl Into<String>, audience: impl Into<String>) -> Self {
        Self {
            issuer: issuer.into(),
            audience: audience.into(),
            tenant_claim: "tenant".to_owned(),
            admin_role_claim: "roles".to_owned(),
            admin_role_value: "escurel:admin".to_owned(),
            jwks_refresh: Duration::from_secs(300),
            jwks_uri: None,
        }
    }

    #[must_use]
    pub fn with_jwks_uri(mut self, uri: impl Into<String>) -> Self {
        self.jwks_uri = Some(uri.into());
        self
    }

    #[must_use]
    pub fn with_tenant_claim(mut self, claim: impl Into<String>) -> Self {
        self.tenant_claim = claim.into();
        self
    }

    #[must_use]
    pub fn with_admin_role(mut self, claim: impl Into<String>, value: impl Into<String>) -> Self {
        self.admin_role_claim = claim.into();
        self.admin_role_value = value.into();
        self
    }

    fn effective_jwks_uri(&self) -> String {
        self.jwks_uri.clone().unwrap_or_else(|| {
            format!(
                "{}/protocol/openid-connect/certs",
                self.issuer.trim_end_matches('/')
            )
        })
    }
}

/// Resolved auth result. Downstream layers route on these three.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthContext {
    pub subject: String,
    pub tenant_id: String,
    pub role: Role,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Standard agent-level access. No admin endpoints.
    Agent,
    /// Admin role grants tenant-CRUD and operator endpoints.
    Admin,
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("token header could not be decoded: {0}")]
    BadHeader(String),
    #[error("token header missing required `kid`")]
    MissingKid,
    #[error("JWKS lookup failed: {0}")]
    Jwks(#[from] JwksCacheError),
    #[error("token validation failed: {0}")]
    Invalid(String),
    #[error("token header algorithm `{0:?}` is not in the trusted allow-list")]
    UnsupportedAlg(Algorithm),
    #[error("token missing required `{tenant_claim}` claim")]
    MissingTenant { tenant_claim: String },
}

#[derive(Debug, Deserialize)]
struct Claims {
    sub: String,
    #[serde(default, flatten)]
    rest: serde_json::Map<String, serde_json::Value>,
}

pub struct OidcVerifier {
    config: OidcConfig,
    jwks: JwksCache,
}

impl std::fmt::Debug for OidcVerifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OidcVerifier")
            .field("issuer", &self.config.issuer)
            .field("audience", &self.config.audience)
            .finish_non_exhaustive()
    }
}

impl OidcVerifier {
    /// Build a verifier from config. Lazy: the JWKS isn't fetched
    /// until the first `verify` call (or an explicit
    /// `prime_cache`).
    #[must_use]
    pub fn new(config: OidcConfig) -> Self {
        let jwks_uri = config.effective_jwks_uri();
        let jwks = JwksCache::new(jwks_uri, config.jwks_refresh);
        Self { config, jwks }
    }

    /// Force a JWKS fetch now (e.g. to surface JWKS errors at
    /// startup instead of on the first request).
    pub async fn prime_cache(&self) -> Result<(), AuthError> {
        self.jwks.refresh().await?;
        Ok(())
    }

    /// Verify a bearer token. Returns an [`AuthContext`] on
    /// success.
    pub async fn verify(&self, token: &str) -> Result<AuthContext, AuthError> {
        let header = decode_header(token).map_err(|e| AuthError::BadHeader(e.to_string()))?;
        // Pin the accepted algorithm set from a trusted allow-list
        // (asymmetric only — RSA/ECDSA, never HMAC) rather than
        // trusting the header. Reject the untrusted header `alg` if
        // it falls outside the allow-list before doing any further
        // work. Defence-in-depth against alg-confusion / downgrade.
        let alg = header.alg;
        if !allowed_algorithms().contains(&alg) {
            return Err(AuthError::UnsupportedAlg(alg));
        }
        let kid = header.kid.ok_or(AuthError::MissingKid)?;
        let key = self.jwks.key_for_kid(&kid).await?;
        // Validate against exactly the (already allow-listed) header
        // alg. We must not stuff the whole allow-list in here —
        // jsonwebtoken returns `InvalidAlgorithm` if any algorithm in
        // the set is incompatible with the key family (e.g. an EC alg
        // against an RSA key). The allow-list gate above is what bounds
        // the accepted set; `Validation::new(alg)` pins this token to
        // its single, vetted algorithm.
        let mut validation = Validation::new(alg);
        validation.set_audience(&[self.config.audience.as_str()]);
        validation.set_issuer(&[self.config.issuer.as_str()]);
        // Required claims left at default (exp, iat); we add aud + iss above.
        let token_data = decode::<Claims>(token, &key, &validation)
            .map_err(|e| AuthError::Invalid(e.to_string()))?;

        let claims = token_data.claims;
        let tenant_id = claims
            .rest
            .get(&self.config.tenant_claim)
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| AuthError::MissingTenant {
                tenant_claim: self.config.tenant_claim.clone(),
            })?
            .to_owned();

        let role = if has_admin_role(&claims.rest, &self.config) {
            Role::Admin
        } else {
            Role::Agent
        };

        Ok(AuthContext {
            subject: claims.sub,
            tenant_id,
            role,
        })
    }

    #[must_use]
    pub fn config(&self) -> &OidcConfig {
        &self.config
    }
}

fn has_admin_role(claims: &serde_json::Map<String, serde_json::Value>, cfg: &OidcConfig) -> bool {
    let Some(v) = claims.get(&cfg.admin_role_claim) else {
        return false;
    };
    match v {
        serde_json::Value::Array(arr) => arr
            .iter()
            .any(|item| item.as_str() == Some(cfg.admin_role_value.as_str())),
        serde_json::Value::String(s) => s == &cfg.admin_role_value,
        _ => false,
    }
}

/// Algorithms we trust at validate-time. RSA SHA-256/384/512 +
/// ECDSA P-256/P-384. Asymmetric only — no HMAC.
fn allowed_algorithms() -> &'static [Algorithm] {
    &[
        Algorithm::RS256,
        Algorithm::RS384,
        Algorithm::RS512,
        Algorithm::ES256,
        Algorithm::ES384,
    ]
}
