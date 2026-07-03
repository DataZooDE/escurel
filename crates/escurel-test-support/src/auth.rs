//! In-process OIDC issuer used by [`AuthMode::TestIssuer`].
//!
//! This module hoists the `keys` / `jwks_mock` / `token` helpers
//! that previously lived as private copies in
//! `crates/escurel-server/tests/auth_quota.rs`,
//! `crates/escurel-server/tests/mcp_admin_tools.rs`, and
//! `crates/escurel-client/tests/client_roundtrip.rs`. The downstream
//! contract from `docs/spec/dx.md` §"Auth in tests" promises that a
//! test using `AuthMode::TestIssuer` never imports `wiremock`,
//! `jsonwebtoken`, or `rsa` directly — those become private deps of
//! this crate.

use base64::Engine as _;
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use rsa::pkcs1::EncodeRsaPrivateKey;
use rsa::traits::PublicKeyParts;
use rsa::{RsaPrivateKey, RsaPublicKey};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

pub use escurel_auth::Role;

/// Auth selector consumed by [`crate::Opts`].
///
/// Variants map to the three positions a downstream test can be in:
/// no auth at all, an in-process JWKS issuer the support crate
/// manages, or a real external OIDC issuer (the application's
/// staging realm, typically).
#[derive(Default, Debug, Clone)]
pub enum AuthMode {
    /// No verifier installed. `/mcp` is unauthenticated. Useful for
    /// tests that exercise the *dispatch* and not the auth path.
    #[default]
    Disabled,

    /// `EscurelProcess` stands up an in-process JWKS endpoint with
    /// an ephemeral 2048-bit RSA keypair. [`crate::EscurelProcess::mint_token`]
    /// signs JWTs that the running server will accept.
    TestIssuer,

    /// Point at a real OIDC. Used when the application's tests want
    /// to exercise the production auth path end-to-end.
    External {
        issuer_url: String,
        jwks_url: String,
    },

    /// Point at TWO OR MORE real issuers — the production substrate
    /// shape where one Escurel trusts both Triton (the forwarded
    /// inbound bearer) and Carl (the self-minted dashboard token).
    /// The first pair is the primary issuer; the rest are additional.
    /// Each entry is `(issuer_url, jwks_url)`. All share the audience
    /// + tenant claim.
    ExternalMulti { issuers: Vec<(String, String)> },
}

/// Audience claim used by the test issuer. Matches the value the
/// gateway verifies against in
/// [`escurel_auth::OidcConfig::new`]'s caller in production. Hard-
/// coded here so tests never need to spell it out.
pub(crate) const TEST_AUDIENCE: &str = "escurel";

/// JWT `kid` used by the in-process issuer's signing key. The
/// matching `kid` is published on the wiremock JWKS endpoint, so
/// the gateway's `OidcVerifier` resolves it through its normal
/// `/protocol/openid-connect/certs` lookup.
pub(crate) const TEST_KID: &str = "escurel-test-support";

/// Path appended to the wiremock origin to form the issuer URL.
/// Anything starting with `/realms/...` would work; this matches
/// the shape used elsewhere in the workspace's auth tests for
/// continuity.
pub(crate) const TEST_ISSUER_PATH: &str = "/realms/test";

/// Ephemeral RSA keypair used by the in-process issuer. Regenerated
/// on every [`crate::EscurelProcess::spawn`] so independent test
/// processes never share signing material.
pub(crate) struct Keys {
    private_pem: Vec<u8>,
    n_b64: String,
    e_b64: String,
}

impl Keys {
    /// Generate a fresh 2048-bit RSA keypair. 2048 is the smallest
    /// size `jsonwebtoken`'s RS256 path accepts without warning,
    /// matches the production substrate's Keycloak issuer, and
    /// keeps key generation under ~200 ms in release builds.
    pub(crate) fn generate() -> Self {
        let mut rng = rand::thread_rng();
        let private = RsaPrivateKey::new(&mut rng, 2048).expect("rsa keygen");
        let public = RsaPublicKey::from(&private);
        let private_pem = private
            .to_pkcs1_pem(rsa::pkcs1::LineEnding::LF)
            .expect("pem encode")
            .as_bytes()
            .to_vec();
        Self {
            private_pem,
            n_b64: b64url(&public.n().to_bytes_be()),
            e_b64: b64url(&public.e().to_bytes_be()),
        }
    }
}

fn b64url(b: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

/// In-process OIDC issuer: a wiremock server publishing a single-
/// key JWKS, and a signing key matching it. Wrap the `MockServer`
/// so [`crate::EscurelProcess`] can carry ownership for the
/// process's lifetime.
pub(crate) struct TestIssuer {
    pub(crate) keys: Keys,
    pub(crate) issuer_url: String,
    pub(crate) jwks_url: String,
    /// When the gateway was configured with a custom
    /// `ConfigOverrides::groups_claim`, the mint helpers emit the group
    /// array under BOTH `roles` (so the `admin_role_claim` projection and
    /// every existing caller keep working) and this claim (so the group
    /// ACL keeps seeing the groups). Without this, setting the knob would
    /// silently strip every TestIssuer principal of its groups — the
    /// second-order trap the knob's tests pin.
    pub(crate) groups_claim: Option<String>,
    /// Owned wiremock server. Kept alive so the JWKS endpoint stays
    /// reachable until the [`crate::EscurelProcess`] is dropped.
    pub(crate) _mock_server: MockServer,
}

impl TestIssuer {
    pub(crate) async fn start() -> Self {
        Self::start_with_groups_claim(None).await
    }

    pub(crate) async fn start_with_groups_claim(groups_claim: Option<String>) -> Self {
        let mock_server = MockServer::start().await;
        let keys = Keys::generate();
        mount_jwks(&mock_server, &keys).await;
        let issuer_url = format!("{}{}", mock_server.uri(), TEST_ISSUER_PATH);
        let jwks_url = format!("{issuer_url}/protocol/openid-connect/certs");
        Self {
            keys,
            issuer_url,
            jwks_url,
            groups_claim,
            _mock_server: mock_server,
        }
    }

    /// Sign a fresh bearer token for `tenant` with `role`. The
    /// 10-minute expiry is plenty for a single test run.
    pub(crate) fn mint(&self, tenant: &str, role: Role) -> String {
        self.mint_with_sub(tenant, role, "test-subject")
    }

    /// Sign a bearer for `tenant`/`role` with an explicit `sub` claim —
    /// for per-instance ACL tests where the subject is the owning
    /// principal (e.g. a member credential).
    pub(crate) fn mint_with_sub(&self, tenant: &str, role: Role, subject: &str) -> String {
        let role_claim = match role {
            // The gateway's default `admin_role_value` is
            // `"escurel:admin"` (see `OidcConfig::new` in
            // `escurel-auth`). The Agent variant deliberately uses
            // a string that does *not* match, so the verifier
            // projects it as `Role::Agent`.
            Role::Admin => "escurel:admin",
            Role::Agent => "escurel:agent",
        };
        self.sign_with_roles(tenant, subject, &[role_claim.to_owned()])
    }

    /// Sign a bearer with an explicit `sub` and an arbitrary set of
    /// group/role names in the `roles` claim — for RBAC tests that need
    /// custom token groups (e.g. `moderator`, `team-acme`). When `admin`
    /// is true the `escurel:admin` marker is appended so the verifier
    /// also projects [`Role::Admin`]. The `groups_claim` and
    /// `admin_role_claim` both default to `roles`, so a single array
    /// drives both projections.
    pub(crate) fn mint_with_groups(
        &self,
        tenant: &str,
        subject: &str,
        groups: &[&str],
        admin: bool,
    ) -> String {
        let mut roles: Vec<String> = groups.iter().map(|g| (*g).to_owned()).collect();
        if admin {
            roles.push("escurel:admin".to_owned());
        }
        self.sign_with_roles(tenant, subject, &roles)
    }

    fn sign_with_roles(&self, tenant: &str, subject: &str, roles: &[String]) -> String {
        let now = now_secs();
        let mut claims = json!({
            "iss": self.issuer_url,
            "aud": TEST_AUDIENCE,
            "sub": subject,
            "tenant": tenant,
            "roles": roles,
            "iat": now,
            "exp": now + 600,
        });
        // Claim-aware minting: mirror the group array under the configured
        // groups claim (see the `groups_claim` field docs).
        if let Some(claim) = &self.groups_claim
            && claim != "roles"
        {
            claims[claim.as_str()] = json!(roles);
        }
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(TEST_KID.to_owned());
        let key = EncodingKey::from_rsa_pem(&self.keys.private_pem).expect("rsa pem parses");
        encode(&header, &claims, &key).expect("jwt sign")
    }
}

/// A standalone in-process OIDC issuer for **multi-issuer** tests — the
/// deployed shape where one Escurel trusts its primary issuer AND a second
/// party's signer (Triton's forwarded bearers, Carl's dashboard tokens).
/// Downstream tests wire it in via `ConfigOverrides::extra_issuers` and
/// mint tokens with arbitrary audience arrays + a custom groups claim, all
/// without importing `wiremock`/`jsonwebtoken` themselves (the dx.md
/// "Auth in tests" promise).
pub struct ExtraIssuer {
    inner: TestIssuer,
}

impl ExtraIssuer {
    pub async fn start() -> Self {
        Self {
            inner: TestIssuer::start().await,
        }
    }

    /// The issuer URL (the token's `iss`).
    pub fn issuer_url(&self) -> &str {
        &self.inner.issuer_url
    }

    /// The JWKS URL serving this issuer's signing key.
    pub fn jwks_url(&self) -> &str {
        &self.inner.jwks_url
    }

    /// Sign a bearer with an **audience array** (the Triton-minted shape:
    /// `aud: [agents, escurel]`) and the group list under `groups_claim`.
    pub fn mint(
        &self,
        tenant: &str,
        subject: &str,
        audiences: &[&str],
        groups_claim: &str,
        groups: &[&str],
    ) -> String {
        let now = now_secs();
        let claims = json!({
            "iss": self.inner.issuer_url,
            "aud": audiences,
            "sub": subject,
            "tenant": tenant,
            groups_claim: groups,
            "iat": now,
            "exp": now + 600,
        });
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(TEST_KID.to_owned());
        let key = EncodingKey::from_rsa_pem(&self.inner.keys.private_pem).expect("rsa pem parses");
        encode(&header, &claims, &key).expect("jwt sign")
    }
}

async fn mount_jwks(server: &MockServer, k: &Keys) {
    let jwks = json!({
        "keys": [{
            "kid": TEST_KID,
            "kty": "RSA",
            "alg": "RS256",
            "use": "sig",
            "n": k.n_b64,
            "e": k.e_b64,
        }]
    });
    Mock::given(method("GET"))
        .and(path(format!(
            "{TEST_ISSUER_PATH}/protocol/openid-connect/certs"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(jwks))
        .mount(server)
        .await;
}

fn now_secs() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_secs()
}
