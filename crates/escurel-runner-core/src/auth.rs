//! The runner's own credential for the gateway.
//!
//! `ESCUREL_RUNNER_TOKEN` is a bearer somebody minted by hand and pasted into
//! a secret. It works, and it has one property that makes it wrong for a
//! deployment: it expires silently. A runner whose token has lapsed still
//! answers `/healthz`, still polls, still marks nothing — it just stops being
//! able to read the inbox, which looks exactly like an empty inbox. The same
//! reasoning already removed static bearers from every other workload on this
//! substrate.
//!
//! So the runner can mint instead. The claim shape and the `kid` derivation
//! here are deliberately IDENTICAL to the ones heron and `agent-core` use,
//! because this borrows the existing signing identity rather than becoming a
//! second issuer: the JWKS the platform already publishes is what verifies
//! these tokens, and no gateway configuration changes.
//!
//! `sub` is a SERVICE principal, not a person — the runner acts on events
//! whose author has gone home. That is also why it carries the admin role:
//! the gateway's write ACL is group-matched, a minted token carries no
//! engagement groups, and an agent that cannot write is a runner that
//! silently does nothing. The privilege is real and worth naming: every run
//! for every consultant currently shares it, and the per-run scoped token is
//! the seam that fixes that (`packager.rs`'s `token` field).

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use rsa::pkcs1::{DecodeRsaPrivateKey, EncodeRsaPrivateKey};
use rsa::pkcs8::DecodePrivateKey;
use rsa::traits::PublicKeyParts;
use rsa::{RsaPrivateKey, RsaPublicKey};
use serde_json::json;

/// The claim the gateway's verifier reads for the tenant.
const TENANT_CLAIM: &str = "tenant";

/// How long a minted bearer lives.
///
/// Long enough that a run and its retries never straddle an expiry; short
/// enough that a leaked one is worth little. Re-minted well before it lapses
/// — see [`REFRESH_MARGIN_SECS`].
pub const TTL_SECS: u64 = 30 * 60;

/// Re-mint this long before expiry, so a token is never handed out with less
/// life left than a slow run might need.
const REFRESH_MARGIN_SECS: u64 = 5 * 60;

/// Errors from building or using the signing identity.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// The key did not parse as PKCS#8 or PKCS#1 RSA.
    #[error(
        "ESCUREL_RUNNER_AUTH_SIGNING_KEY is not a valid RSA private key (tried PKCS#8, PKCS#1)"
    )]
    KeyParse,
    /// Re-encoding the parsed key failed.
    #[error("re-encoding the RSA key failed: {0}")]
    KeyEncode(rsa::pkcs1::Error),
    /// Signing failed.
    #[error("minting failed: {0}")]
    Mint(#[from] jsonwebtoken::errors::Error),
}

/// Where the runner's gateway bearer comes from.
///
/// Two variants and no fallback between them: a deployment that configured a
/// signing key and gets a stale static token instead would be authenticated
/// as something nobody chose.
pub enum TokenSource {
    /// A bearer supplied whole (`ESCUREL_RUNNER_TOKEN`). Returned unchanged,
    /// expiry and all.
    Static(String),
    /// Minted here, and re-minted before it lapses.
    Minted {
        signer: Signer,
        subject: String,
        /// How long each minted bearer lives. Configurable so a test can
        /// outlive one in seconds rather than half an hour — the only way to
        /// prove a long-running loop actually re-mints.
        ttl_secs: u64,
        /// `(token, expires_at_unix)`.
        cached: Mutex<Option<(String, u64)>>,
    },
}

impl std::fmt::Debug for TokenSource {
    /// Hand-written: neither the key nor a live bearer may reach a log.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Static(_) => f.write_str("TokenSource::Static(<redacted>)"),
            Self::Minted { subject, .. } => f
                .debug_struct("TokenSource::Minted")
                .field("subject", subject)
                .finish_non_exhaustive(),
        }
    }
}

impl TokenSource {
    /// A usable bearer: the static one, or a minted one with life left.
    ///
    /// # Errors
    /// When minting fails.
    pub fn current(&self) -> Result<String, AuthError> {
        match self {
            Self::Static(t) => Ok(t.clone()),
            Self::Minted {
                signer,
                subject,
                ttl_secs,
                cached,
            } => {
                let ttl = *ttl_secs;
                let now = now_secs();
                let mut slot = cached.lock().unwrap_or_else(|e| e.into_inner());
                // A TTL at or under the margin re-mints every call, which is
                // exactly what a short-lived test TTL should do.
                if let Some((token, exp)) = slot.as_ref()
                    && *exp > now + REFRESH_MARGIN_SECS.min(ttl / 2)
                {
                    return Ok(token.clone());
                }
                let token = signer.mint(subject, ttl)?;
                *slot = Some((token.clone(), now + ttl));
                Ok(token)
            }
        }
    }
}

/// The RSA signing identity, built once at boot.
pub struct Signer {
    issuer: String,
    audience: String,
    tenant: String,
    kid: String,
    /// PKCS#1 PEM — the flavour `jsonwebtoken`'s RS256 path accepts.
    private_pem: Vec<u8>,
}

impl std::fmt::Debug for Signer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Signer")
            .field("issuer", &self.issuer)
            .field("audience", &self.audience)
            .field("tenant", &self.tenant)
            .field("kid", &self.kid)
            .finish_non_exhaustive()
    }
}

impl Signer {
    /// Build from an issuer, audience, tenant, optional `kid` and the key.
    ///
    /// Both PEM flavours are accepted deliberately: Secret Manager holds the
    /// shared key as PKCS#8, while most key-generation paths emit PKCS#1.
    /// Rejecting one is a boot failure whose message points nowhere near the
    /// cause.
    ///
    /// # Errors
    /// When the key does not parse or cannot be re-encoded.
    pub fn build(
        issuer: String,
        audience: String,
        tenant: String,
        kid: Option<String>,
        signing_key_pem: &str,
    ) -> Result<Self, AuthError> {
        let raw = signing_key_pem.trim();
        let private = RsaPrivateKey::from_pkcs8_pem(raw)
            .or_else(|_| RsaPrivateKey::from_pkcs1_pem(raw))
            .map_err(|_| AuthError::KeyParse)?;
        let private_pem = private
            .to_pkcs1_pem(rsa::pkcs1::LineEnding::LF)
            .map_err(AuthError::KeyEncode)?
            .as_bytes()
            .to_vec();

        let public = RsaPublicKey::from(&private);
        let n_b64 = b64url(&public.n().to_bytes_be());
        let e_b64 = b64url(&public.e().to_bytes_be());
        let kid = kid.unwrap_or_else(|| derive_kid(&n_b64, &e_b64));

        Ok(Self {
            issuer,
            audience,
            tenant,
            kid,
            private_pem,
        })
    }

    /// Mint a service bearer for `subject`, valid `ttl_secs`.
    ///
    /// # Errors
    /// When signing fails.
    pub fn mint(&self, subject: &str, ttl_secs: u64) -> Result<String, AuthError> {
        let now = now_secs();
        let claims = json!({
            "iss": self.issuer,
            "aud": self.audience,
            "sub": subject,
            TENANT_CLAIM: self.tenant,
            // See the module note: a minted token carries no engagement
            // groups, and the gateway's write ACL matches on groups.
            "roles": ["escurel:admin"],
            "iat": now,
            "exp": now + ttl_secs,
        });
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(self.kid.clone());
        Ok(encode(
            &header,
            &claims,
            &EncodingKey::from_rsa_pem(&self.private_pem)?,
        )?)
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The `kid`, in the platform's scheme.
///
/// The `agent-escurel-` prefix is NOT decoration. This borrows the platform's
/// Escurel identity by sharing its key, so the gateway verifies against the
/// JWKS that identity publishes — and that document names the key
/// `agent-escurel-<fingerprint>`. A bare fingerprint is a `kid` the gateway
/// has never seen, and every token is then rejected on a `kid` miss rather
/// than on anything to do with the signature.
///
/// Derived rather than pinned, so rotating the key rotates the identifier.
fn derive_kid(n_b64: &str, e_b64: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(n_b64.as_bytes());
    h.update(b".");
    h.update(e_b64.as_bytes());
    format!("agent-escurel-{}", &b64url(&h.finalize())[..12])
}

fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real 2048-bit RSA key, generated for this test.
    fn test_key() -> String {
        use rsa::pkcs8::EncodePrivateKey;
        let mut rng = rand::thread_rng();
        let key = RsaPrivateKey::new(&mut rng, 2048).expect("generate");
        key.to_pkcs8_pem(rsa::pkcs8::LineEnding::LF)
            .expect("encode")
            .to_string()
    }

    #[test]
    fn a_minted_token_carries_the_claims_the_gateway_verifies() {
        let signer = Signer::build(
            "https://agent-lab.data-zoo.de".into(),
            "escurel".into(),
            "datazoo-loops".into(),
            None,
            &test_key(),
        )
        .expect("signer");
        let token = signer.mint("escurel-runner", 600).expect("mint");

        let parts: Vec<&str> = token.split('.').collect();
        assert_eq!(parts.len(), 3, "a JWT has three parts: {token}");
        let decode = |s: &str| {
            let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(s)
                .expect("b64");
            serde_json::from_slice::<serde_json::Value>(&bytes).expect("json")
        };
        let header = decode(parts[0]);
        let claims = decode(parts[1]);

        assert_eq!(header["alg"], "RS256");
        assert!(
            header["kid"]
                .as_str()
                .is_some_and(|k| k.starts_with("agent-escurel-")),
            "the kid must be in the platform's scheme, or every token is \
             rejected on a kid miss: {header}"
        );
        assert_eq!(claims["tenant"], "datazoo-loops", "{claims}");
        assert_eq!(claims["aud"], "escurel", "{claims}");
        assert_eq!(claims["sub"], "escurel-runner", "{claims}");
        assert_eq!(claims["roles"][0], "escurel:admin", "{claims}");
        assert!(
            claims["exp"].as_u64().unwrap_or(0) > claims["iat"].as_u64().unwrap_or(0),
            "{claims}"
        );
    }

    /// The reason to mint at all: a token that is about to lapse is replaced
    /// before it is handed out.
    #[test]
    fn a_stale_cached_token_is_re_minted() {
        let signer = Signer::build(
            "https://issuer".into(),
            "escurel".into(),
            "t".into(),
            None,
            &test_key(),
        )
        .expect("signer");
        let stale = signer.mint("escurel-runner", 60).expect("mint");
        let source = TokenSource::Minted {
            signer,
            ttl_secs: TTL_SECS,
            subject: "escurel-runner".into(),
            // Expires inside the refresh margin: still valid, not valid
            // enough to hand to a run that may take minutes.
            cached: Mutex::new(Some((stale.clone(), now_secs() + 60))),
        };

        let fresh = source.current().expect("current");
        assert_ne!(
            fresh, stale,
            "a token inside the refresh margin must be re-minted"
        );
        // Positive control: the freshly minted one IS reused, so the test
        // above is about staleness and not about caching being broken.
        assert_eq!(source.current().expect("current"), fresh);
    }

    #[test]
    fn a_static_token_is_returned_unchanged() {
        let source = TokenSource::Static("pasted-bearer".into());
        assert_eq!(source.current().expect("current"), "pasted-bearer");
    }

    /// Neither the key nor a live bearer may reach a log.
    #[test]
    fn debug_redacts_the_credential() {
        let dbg = format!("{:?}", TokenSource::Static("super-secret".into()));
        assert!(!dbg.contains("super-secret"), "{dbg}");
    }
}
