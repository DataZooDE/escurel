//! `OidcVerifier`: validate a bearer JWT against the configured
//! issuer and project it into an [`AuthContext`].

use std::time::Duration;

use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
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
    /// JWT claim name that lists the subject's group/role memberships
    /// for the data-level ACL. Default `"roles"` (the same claim admin
    /// derives from). Parsed leniently — see [`parse_groups_claim`].
    pub groups_claim: String,
    /// TTL for the in-memory JWKS cache.
    pub jwks_refresh: Duration,
    /// Optional override for the JWKS URL. When `None`, the
    /// verifier constructs `${issuer}/protocol/openid-connect/certs`
    /// (the Keycloak convention).
    pub jwks_uri: Option<String>,
    /// Additional trusted issuers beyond the primary [`Self::issuer`],
    /// each an `(issuer, jwks_uri_override)` pair. Empty by default →
    /// single-issuer behaviour. The substrate uses one entry here so a
    /// single Escurel trusts both Triton (the forwarded inbound bearer)
    /// and Carl (the self-minted dashboard token); both name the same
    /// [`Self::audience`], differing only in `iss` + signing key.
    pub additional_issuers: Vec<(String, Option<String>)>,
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
            groups_claim: "roles".to_owned(),
            jwks_refresh: Duration::from_secs(300),
            jwks_uri: None,
            additional_issuers: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_jwks_uri(mut self, uri: impl Into<String>) -> Self {
        self.jwks_uri = Some(uri.into());
        self
    }

    /// Trust one more issuer beyond the primary, with an optional
    /// explicit JWKS URL (derived from the issuer when `None`). The
    /// added issuer shares the primary's audience, tenant claim, and
    /// role config.
    #[must_use]
    pub fn with_additional_issuer(
        mut self,
        issuer: impl Into<String>,
        jwks_uri: Option<String>,
    ) -> Self {
        self.additional_issuers.push((issuer.into(), jwks_uri));
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

    #[must_use]
    pub fn with_groups_claim(mut self, claim: impl Into<String>) -> Self {
        self.groups_claim = claim.into();
        self
    }

    fn effective_jwks_uri(&self) -> String {
        derive_jwks_uri(&self.issuer, self.jwks_uri.as_deref())
    }
}

/// JWKS URL for an issuer: the explicit override, else the Keycloak
/// convention `${issuer}/protocol/openid-connect/certs`.
fn derive_jwks_uri(issuer: &str, override_uri: Option<&str>) -> String {
    override_uri.map(ToOwned::to_owned).unwrap_or_else(|| {
        format!(
            "{}/protocol/openid-connect/certs",
            issuer.trim_end_matches('/')
        )
    })
}

/// Resolved auth result. Downstream layers route on these three.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthContext {
    pub subject: String,
    pub tenant_id: String,
    pub role: Role,
    /// Group/role names parsed from the configured `groups_claim`.
    /// Raw — reserved-name and admin-value stripping is the ACL
    /// layer's job (`escurel-index`), not the verifier's.
    pub groups: Vec<String>,
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

/// One trusted issuer and the JWKS cache that backs it. The verifier
/// holds one per configured issuer and routes a token to the entry
/// whose `issuer` matches the token's `iss`.
struct TrustEntry {
    issuer: String,
    jwks: JwksCache,
}

pub struct OidcVerifier {
    config: OidcConfig,
    /// Primary issuer first, then any additional issuers. Non-empty.
    entries: Vec<TrustEntry>,
}

impl std::fmt::Debug for OidcVerifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let issuers: Vec<&str> = self.entries.iter().map(|e| e.issuer.as_str()).collect();
        f.debug_struct("OidcVerifier")
            .field("issuers", &issuers)
            .field("audience", &self.config.audience)
            .finish_non_exhaustive()
    }
}

/// Just the `iss` claim — read WITHOUT signature verification, only to
/// route the token to its trust entry. The verified decode below is
/// what authenticates; this projection is never trusted on its own.
#[derive(Debug, Deserialize)]
struct IssProbe {
    iss: Option<String>,
}

impl OidcVerifier {
    /// Build a verifier from config. Lazy: the JWKS isn't fetched
    /// until the first `verify` call (or an explicit
    /// `prime_cache`).
    #[must_use]
    pub fn new(config: OidcConfig) -> Self {
        let mut entries = Vec::with_capacity(1 + config.additional_issuers.len());
        // Primary issuer first.
        entries.push(TrustEntry {
            issuer: config.issuer.clone(),
            jwks: JwksCache::new(config.effective_jwks_uri(), config.jwks_refresh),
        });
        for (issuer, jwks_uri) in &config.additional_issuers {
            entries.push(TrustEntry {
                issuer: issuer.clone(),
                jwks: JwksCache::new(
                    derive_jwks_uri(issuer, jwks_uri.as_deref()),
                    config.jwks_refresh,
                ),
            });
        }
        Self { config, entries }
    }

    /// Force a JWKS fetch now for every trusted issuer (e.g. to
    /// surface JWKS errors at startup instead of on the first request).
    pub async fn prime_cache(&self) -> Result<(), AuthError> {
        for entry in &self.entries {
            entry.jwks.refresh().await?;
        }
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
        // Route by the token's `iss` to its trust entry. The issuer is
        // read WITHOUT verifying the signature — purely to pick which
        // configured JWKS + issuer to verify against. The verified
        // `decode` below (against that entry's published key) is what
        // authenticates; a forged `iss` only selects the wrong entry,
        // whose key then fails to verify the signature.
        let entry = self.select_entry(token)?;
        let key = entry.jwks.key_for_kid(&kid).await?;
        // Validate against exactly the (already allow-listed) header
        // alg. We must not stuff the whole allow-list in here —
        // jsonwebtoken returns `InvalidAlgorithm` if any algorithm in
        // the set is incompatible with the key family (e.g. an EC alg
        // against an RSA key). The allow-list gate above is what bounds
        // the accepted set; `Validation::new(alg)` pins this token to
        // its single, vetted algorithm.
        let mut validation = Validation::new(alg);
        validation.set_audience(&[self.config.audience.as_str()]);
        validation.set_issuer(&[entry.issuer.as_str()]);
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

        let groups = claims
            .rest
            .get(&self.config.groups_claim)
            .map(parse_groups_claim)
            .unwrap_or_default();

        Ok(AuthContext {
            subject: claims.sub,
            tenant_id,
            role,
            groups,
        })
    }

    /// Pick the trust entry whose issuer matches the token's `iss`.
    /// The `iss` is read without verifying the signature (routing only).
    /// Single-issuer configs skip the probe — there is one entry and the
    /// verified `set_issuer` check below still pins `iss`.
    fn select_entry(&self, token: &str) -> Result<&TrustEntry, AuthError> {
        if let [only] = self.entries.as_slice() {
            return Ok(only);
        }
        let mut probe = Validation::new(Algorithm::RS256);
        probe.insecure_disable_signature_validation();
        probe.validate_exp = false;
        probe.validate_aud = false;
        probe.required_spec_claims = std::collections::HashSet::new();
        let iss = decode::<IssProbe>(token, &DecodingKey::from_secret(&[]), &probe)
            .map_err(|e| AuthError::Invalid(e.to_string()))?
            .claims
            .iss
            .ok_or_else(|| AuthError::Invalid("token missing `iss` claim".to_owned()))?;
        self.entries
            .iter()
            .find(|e| e.issuer == iss)
            .ok_or_else(|| AuthError::Invalid(format!("untrusted issuer `{iss}`")))
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

/// Project the `groups_claim` value into a flat list of group names.
///
/// - a JSON **array of strings** → each element (non-string elements
///   dropped);
/// - a single **string** → split on whitespace **and** commas
///   (Keycloak/Auth0 both occur in the wild), trimmed, empties dropped;
/// - any other shape → empty.
///
/// Reserved-name (`public`/`owner`/`admin`) and admin-value stripping is
/// deliberately NOT done here — the verifier stays dumb; the ACL layer
/// owns that policy.
fn parse_groups_claim(value: &serde_json::Value) -> Vec<String> {
    match value {
        serde_json::Value::Array(arr) => arr
            .iter()
            .filter_map(|v| v.as_str())
            .map(str::to_owned)
            .collect(),
        serde_json::Value::String(s) => s
            .split([' ', '\t', '\n', '\r', ','])
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
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
