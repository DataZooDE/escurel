//! Integration tests for the multi-issuer `OidcVerifier`.
//!
//! The substrate runs one Escurel that must trust TWO token issuers:
//! Triton (the agent forwards Triton's inbound bearer) AND Carl (the
//! operator dashboard mints its own token — operators carry no inbound
//! bearer). Both name the same audience; only `iss` + the signing key
//! (JWKS) differ. The verifier routes by the token's `iss` to the
//! matching trust entry, then verifies against that entry's JWKS.
//!
//! Real RSA keys, real signed JWTs, real wiremock JWKS — no trait mocks.

use escurel_auth::{AuthError, OidcConfig, OidcVerifier, Role};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use rsa::pkcs1::EncodeRsaPrivateKey;
use rsa::traits::PublicKeyParts;
use rsa::{RsaPrivateKey, RsaPublicKey};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const AUDIENCE: &str = "escurel";
const JWKS_PATH: &str = "/protocol/openid-connect/certs";

/// An issuer's RSA keypair + its public-key material, plus the `kid`
/// it publishes. Each issuer in a multi-issuer test gets a distinct
/// keypair and `kid`.
struct IssuerKeys {
    private_pem: Vec<u8>,
    n_b64: String,
    e_b64: String,
    kid: String,
}

fn make_keys(kid: &str) -> IssuerKeys {
    let mut rng = rand::thread_rng();
    let private = RsaPrivateKey::new(&mut rng, 2048).expect("rsa keygen");
    let public = RsaPublicKey::from(&private);
    let private_pem = private
        .to_pkcs1_pem(rsa::pkcs1::LineEnding::LF)
        .expect("pem")
        .as_bytes()
        .to_vec();
    IssuerKeys {
        private_pem,
        n_b64: base64url_no_pad(&public.n().to_bytes_be()),
        e_b64: base64url_no_pad(&public.e().to_bytes_be()),
        kid: kid.to_owned(),
    }
}

fn base64url_no_pad(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Sign a JWT with `keys`, advertising `keys.kid` in the header.
fn sign(keys: &IssuerKeys, claims: Value) -> String {
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(keys.kid.clone());
    let key = EncodingKey::from_rsa_pem(&keys.private_pem).expect("encoding key");
    encode(&header, &claims, &key).expect("sign jwt")
}

/// Mount a single-key JWKS for `keys` on `server` at the Keycloak path.
async fn mount_jwks(server: &MockServer, keys: &IssuerKeys) {
    let jwks = json!({
        "keys": [{
            "kid": keys.kid,
            "kty": "RSA",
            "alg": "RS256",
            "use": "sig",
            "n": keys.n_b64,
            "e": keys.e_b64,
        }]
    });
    Mock::given(method("GET"))
        .and(path(JWKS_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(jwks))
        .mount(server)
        .await;
}

fn issuer_url(server: &MockServer) -> String {
    server.uri()
}

fn jwks_uri(server: &MockServer) -> String {
    format!("{}{JWKS_PATH}", server.uri())
}

fn now() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn claims(iss: &str, aud: &str, sub: &str, tenant: &str) -> Value {
    let now = now();
    json!({
        "iss": iss, "aud": aud, "sub": sub, "tenant": tenant,
        "iat": now, "exp": now + 600,
    })
}

/// A verifier that trusts `primary` (issuer #1) plus `additional`
/// (issuer #2) — the production dz-escurel shape (Triton + Carl).
fn dual_verifier(primary: &MockServer, additional: &MockServer) -> OidcVerifier {
    let config = OidcConfig::new(issuer_url(primary), AUDIENCE.to_owned())
        .with_jwks_uri(jwks_uri(primary))
        .with_additional_issuer(issuer_url(additional), Some(jwks_uri(additional)));
    OidcVerifier::new(config)
}

#[tokio::test]
async fn second_issuer_token_is_accepted() {
    let triton = MockServer::start().await;
    let carl = MockServer::start().await;
    let triton_keys = make_keys("triton-kid");
    let carl_keys = make_keys("carl-kid");
    mount_jwks(&triton, &triton_keys).await;
    mount_jwks(&carl, &carl_keys).await;

    let v = dual_verifier(&triton, &carl);

    // A token from the SECOND issuer (Carl), signed by Carl's key.
    let token = sign(
        &carl_keys,
        claims(&issuer_url(&carl), AUDIENCE, "operator-1", "default"),
    );
    let ctx = v.verify(&token).await.expect("second issuer must verify");
    assert_eq!(ctx.subject, "operator-1");
    assert_eq!(ctx.tenant_id, "default");
    assert_eq!(ctx.role, Role::Agent);
}

#[tokio::test]
async fn primary_issuer_still_accepted_with_additional_configured() {
    let triton = MockServer::start().await;
    let carl = MockServer::start().await;
    let triton_keys = make_keys("triton-kid");
    let carl_keys = make_keys("carl-kid");
    mount_jwks(&triton, &triton_keys).await;
    mount_jwks(&carl, &carl_keys).await;

    let v = dual_verifier(&triton, &carl);

    let token = sign(
        &triton_keys,
        claims(&issuer_url(&triton), AUDIENCE, "member-7", "default"),
    );
    let ctx = v.verify(&token).await.expect("primary issuer must verify");
    assert_eq!(ctx.subject, "member-7");
    assert_eq!(ctx.tenant_id, "default");
}

#[tokio::test]
async fn untrusted_issuer_is_rejected() {
    let triton = MockServer::start().await;
    let carl = MockServer::start().await;
    let triton_keys = make_keys("triton-kid");
    let carl_keys = make_keys("carl-kid");
    mount_jwks(&triton, &triton_keys).await;
    mount_jwks(&carl, &carl_keys).await;

    let v = dual_verifier(&triton, &carl);

    // A well-signed token whose `iss` matches NEITHER trust entry.
    let token = sign(
        &carl_keys,
        claims("https://evil.example.com", AUDIENCE, "x", "default"),
    );
    let err = v
        .verify(&token)
        .await
        .expect_err("untrusted iss must reject");
    assert!(matches!(err, AuthError::Invalid(_)), "{err}");
}

#[tokio::test]
async fn issuer_routing_does_not_let_one_issuers_key_sign_for_another() {
    // Key-confusion guard: a token CLAIMING iss=Carl but signed with
    // Triton's key (and advertising Carl's kid). Routing by `iss` sends
    // it to Carl's JWKS, whose key cannot verify Triton's signature →
    // rejected. Proves the `iss` route only selects the trust entry; the
    // signature is still checked against THAT entry's published key.
    let triton = MockServer::start().await;
    let carl = MockServer::start().await;
    let triton_keys = make_keys("triton-kid");
    let carl_keys = make_keys("carl-kid");
    mount_jwks(&triton, &triton_keys).await;
    mount_jwks(&carl, &carl_keys).await;

    let v = dual_verifier(&triton, &carl);

    // Sign with Triton's PRIVATE key but advertise Carl's kid + iss=Carl.
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(carl_keys.kid.clone());
    let key = EncodingKey::from_rsa_pem(&triton_keys.private_pem).unwrap();
    let forged = encode(
        &header,
        &claims(&issuer_url(&carl), AUDIENCE, "attacker", "default"),
        &key,
    )
    .unwrap();

    let err = v.verify(&forged).await.expect_err("cross-key must reject");
    assert!(matches!(err, AuthError::Invalid(_)), "{err}");
}
