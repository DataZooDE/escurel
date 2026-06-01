//! Integration tests for `OidcVerifier`.
//!
//! Real RSA keys, real signed JWTs, real HTTP server (wiremock)
//! serving real JWKS. No mocks at the trait layer — the verifier
//! goes through its full prod path (header decode, JWKS fetch +
//! cache, signature verify, claims projection).

use std::time::Duration;

use escurel_auth::{AuthError, OidcConfig, OidcVerifier, Role};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use rsa::pkcs1::EncodeRsaPrivateKey;
use rsa::traits::PublicKeyParts;
use rsa::{BigUint, RsaPrivateKey, RsaPublicKey};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const ISSUER_PATH: &str = "/realms/test";
const AUDIENCE: &str = "escurel";
const KID: &str = "test-kid";

/// One reusable RSA keypair + the encoding/decoding bytes the test
/// needs. Tests in this file each spin up their own.
struct TestKeys {
    private_pem: Vec<u8>,
    n_b64: String,
    e_b64: String,
}

fn make_keys() -> TestKeys {
    let mut rng = rand::thread_rng();
    let private = RsaPrivateKey::new(&mut rng, 2048).expect("rsa keygen");
    let public = RsaPublicKey::from(&private);
    let private_pem = private
        .to_pkcs1_pem(rsa::pkcs1::LineEnding::LF)
        .expect("pem")
        .as_bytes()
        .to_vec();
    let n_b64 = base64url_no_pad(&public.n().to_bytes_be());
    let e_b64 = base64url_no_pad(&public.e().to_bytes_be());
    let _ = BigUint::new(vec![]);
    TestKeys {
        private_pem,
        n_b64,
        e_b64,
    }
}

fn base64url_no_pad(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn sign_token(keys: &TestKeys, claims: Value) -> String {
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(KID.to_owned());
    let key = EncodingKey::from_rsa_pem(&keys.private_pem).expect("encoding key");
    encode(&header, &claims, &key).expect("sign jwt")
}

async fn mock_jwks(server: &MockServer, keys: &TestKeys) {
    let jwks = json!({
        "keys": [{
            "kid": KID,
            "kty": "RSA",
            "alg": "RS256",
            "use": "sig",
            "n": keys.n_b64,
            "e": keys.e_b64,
        }]
    });
    Mock::given(method("GET"))
        .and(path(format!("{ISSUER_PATH}/protocol/openid-connect/certs")))
        .respond_with(ResponseTemplate::new(200).set_body_json(jwks))
        .mount(server)
        .await;
}

fn verifier_pointing_at(server: &MockServer) -> OidcVerifier {
    let issuer = format!("{}{ISSUER_PATH}", server.uri());
    let config = OidcConfig::new(issuer.clone(), AUDIENCE.to_owned())
        .with_jwks_uri(format!("{issuer}/protocol/openid-connect/certs"));
    OidcVerifier::new(config)
}

fn now() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

#[tokio::test]
async fn verifies_a_well_formed_token_to_agent_role() {
    let server = MockServer::start().await;
    let keys = make_keys();
    mock_jwks(&server, &keys).await;
    let issuer = format!("{}{ISSUER_PATH}", server.uri());
    let v = verifier_pointing_at(&server);
    let now = now();

    let token = sign_token(
        &keys,
        json!({
            "iss": issuer,
            "aud": AUDIENCE,
            "sub": "user-42",
            "tenant": "acme",
            "iat": now,
            "exp": now + 600,
            "roles": ["regular-user"]
        }),
    );

    let ctx = v.verify(&token).await.expect("verify");
    assert_eq!(ctx.subject, "user-42");
    assert_eq!(ctx.tenant_id, "acme");
    assert_eq!(ctx.role, Role::Agent);
}

#[tokio::test]
async fn admin_role_is_detected_when_role_value_in_claim_array() {
    let server = MockServer::start().await;
    let keys = make_keys();
    mock_jwks(&server, &keys).await;
    let issuer = format!("{}{ISSUER_PATH}", server.uri());
    let v = verifier_pointing_at(&server);
    let now = now();

    let token = sign_token(
        &keys,
        json!({
            "iss": issuer,
            "aud": AUDIENCE,
            "sub": "admin-1",
            "tenant": "acme",
            "iat": now,
            "exp": now + 600,
            "roles": ["regular-user", "escurel:admin"]
        }),
    );

    let ctx = v.verify(&token).await.expect("verify");
    assert_eq!(ctx.role, Role::Admin);
}

#[tokio::test]
async fn missing_tenant_claim_errors() {
    let server = MockServer::start().await;
    let keys = make_keys();
    mock_jwks(&server, &keys).await;
    let issuer = format!("{}{ISSUER_PATH}", server.uri());
    let v = verifier_pointing_at(&server);
    let now = now();

    let token = sign_token(
        &keys,
        json!({
            "iss": issuer,
            "aud": AUDIENCE,
            "sub": "u",
            "iat": now,
            "exp": now + 600,
        }),
    );

    let err = v.verify(&token).await.expect_err("must error");
    assert!(matches!(err, AuthError::MissingTenant { .. }), "{err}");
}

#[tokio::test]
async fn wrong_audience_is_rejected() {
    let server = MockServer::start().await;
    let keys = make_keys();
    mock_jwks(&server, &keys).await;
    let issuer = format!("{}{ISSUER_PATH}", server.uri());
    let v = verifier_pointing_at(&server);
    let now = now();

    let token = sign_token(
        &keys,
        json!({
            "iss": issuer,
            "aud": "some-other-service",
            "sub": "u",
            "tenant": "acme",
            "iat": now,
            "exp": now + 600,
        }),
    );

    let err = v.verify(&token).await.expect_err("must error");
    assert!(matches!(err, AuthError::Invalid(_)), "{err}");
}

#[tokio::test]
async fn expired_token_is_rejected() {
    let server = MockServer::start().await;
    let keys = make_keys();
    mock_jwks(&server, &keys).await;
    let issuer = format!("{}{ISSUER_PATH}", server.uri());
    let v = verifier_pointing_at(&server);
    let now = now();

    let token = sign_token(
        &keys,
        json!({
            "iss": issuer,
            "aud": AUDIENCE,
            "sub": "u",
            "tenant": "acme",
            "iat": now - 1200,
            "exp": now - 600,
        }),
    );

    let err = v.verify(&token).await.expect_err("must error");
    assert!(matches!(err, AuthError::Invalid(_)));
}

#[tokio::test]
async fn token_signed_by_unknown_kid_is_rejected() {
    let server = MockServer::start().await;
    let keys = make_keys();
    mock_jwks(&server, &keys).await;
    let issuer = format!("{}{ISSUER_PATH}", server.uri());
    let v = verifier_pointing_at(&server);
    let now = now();

    let claims = json!({
        "iss": issuer,
        "aud": AUDIENCE,
        "sub": "u",
        "tenant": "acme",
        "iat": now,
        "exp": now + 600,
    });
    // Build a token with the right private key but a *different*
    // kid header — the JWKS doesn't have this kid, so the lookup
    // fails.
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some("totally-unknown-kid".to_owned());
    let key = EncodingKey::from_rsa_pem(&keys.private_pem).unwrap();
    let token = encode(&header, &claims, &key).unwrap();

    let err = v.verify(&token).await.expect_err("must error");
    assert!(matches!(err, AuthError::Jwks(_)));
}

#[tokio::test]
async fn tampered_signature_is_rejected() {
    let server = MockServer::start().await;
    let keys = make_keys();
    mock_jwks(&server, &keys).await;
    let issuer = format!("{}{ISSUER_PATH}", server.uri());
    let v = verifier_pointing_at(&server);
    let now = now();

    let token = sign_token(
        &keys,
        json!({
            "iss": issuer,
            "aud": AUDIENCE,
            "sub": "u",
            "tenant": "acme",
            "iat": now,
            "exp": now + 600,
        }),
    );
    // Flip one byte of the signature segment.
    let mut parts: Vec<&str> = token.split('.').collect();
    let mut sig_bytes = parts.pop().unwrap().to_owned();
    let last_char = sig_bytes.pop().unwrap();
    let replaced = if last_char == 'A' { 'B' } else { 'A' };
    sig_bytes.push(replaced);
    let tampered = format!("{}.{}.{}", parts[0], parts[1], sig_bytes);

    let err = v.verify(&tampered).await.expect_err("must error");
    assert!(matches!(err, AuthError::Invalid(_)));
}

#[tokio::test]
async fn token_with_disallowed_alg_is_rejected() {
    // A token whose header `alg` is outside the asymmetric allow-list
    // (here HS256, an HMAC alg) must be rejected on the algorithm
    // allow-list — even when it carries the right `kid`. This pins
    // the defence-in-depth against alg-confusion / downgrade.
    let server = MockServer::start().await;
    let keys = make_keys();
    mock_jwks(&server, &keys).await;
    let issuer = format!("{}{ISSUER_PATH}", server.uri());
    let v = verifier_pointing_at(&server);
    let now = now();

    let claims = json!({
        "iss": issuer,
        "aud": AUDIENCE,
        "sub": "u",
        "tenant": "acme",
        "iat": now,
        "exp": now + 600,
    });
    // Sign with HS256 + a symmetric secret, but advertise the known kid.
    let mut header = Header::new(Algorithm::HS256);
    header.kid = Some(KID.to_owned());
    let key = EncodingKey::from_secret(b"attacker-chosen-secret");
    let token = encode(&header, &claims, &key).expect("sign hs256");

    let err = v.verify(&token).await.expect_err("must reject HS256");
    assert!(
        matches!(err, AuthError::UnsupportedAlg(Algorithm::HS256)),
        "{err}"
    );
}

#[tokio::test]
async fn jwks_cache_serves_repeated_lookups_without_extra_fetches() {
    let server = MockServer::start().await;
    let keys = make_keys();
    let jwks_path = format!("{ISSUER_PATH}/protocol/openid-connect/certs");
    Mock::given(method("GET"))
        .and(path(jwks_path.clone()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "keys": [{
                "kid": KID, "kty": "RSA", "alg": "RS256",
                "n": keys.n_b64, "e": keys.e_b64,
            }]
        })))
        .expect(1) // exactly one fetch across all verify calls
        .mount(&server)
        .await;

    let issuer = format!("{}{ISSUER_PATH}", server.uri());
    let config = OidcConfig::new(issuer.clone(), AUDIENCE.to_owned())
        .with_jwks_uri(format!("{issuer}/protocol/openid-connect/certs"));
    let v = OidcVerifier::new(config);
    let now = now();
    let token = sign_token(
        &keys,
        json!({
            "iss": issuer,
            "aud": AUDIENCE,
            "sub": "u",
            "tenant": "acme",
            "iat": now,
            "exp": now + 600,
        }),
    );
    for _ in 0..5 {
        v.verify(&token).await.unwrap();
    }
    // wiremock's expect(1) asserts on drop.
    let _ = Duration::from_secs(1);
}
