//! Integration tests for `OidcVerifier`.
//!
//! Real RSA keys, real signed JWTs, real HTTP server (wiremock)
//! serving real JWKS. No mocks at the trait layer — the verifier
//! goes through its full prod path (header decode, JWKS fetch +
//! cache, signature verify, claims projection).

use std::time::Duration;

use escurel_auth::{AuthError, OidcConfig, OidcVerifier, Role, ServiceRole};
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

/// The built-in service roles (#828): `escurel:runner` / `escurel:bridge` in the
/// roles claim project to `AuthContext.service`, renamable like the admin value,
/// never admin by themselves, and IGNORED on any machine token (run, skill,
/// agent or `act`) so a token minted for a run cannot inherit them. `iat`
/// comes out as `issued_at` for the agent-token epoch (#829).
#[tokio::test]
async fn service_roles_project_on_a_service_credential_and_never_on_a_machine_token() {
    let server = MockServer::start().await;
    let keys = make_keys();
    mock_jwks(&server, &keys).await;
    let issuer = format!("{}{ISSUER_PATH}", server.uri());
    let v = verifier_pointing_at(&server);
    let now = now();
    let token = |roles: Value, extra: Value| {
        let mut c = json!({
            "iss": issuer, "aud": AUDIENCE, "sub": "runner:hq", "tenant": "acme",
            "iat": now, "exp": now + 600, "roles": roles,
        });
        for (k, val) in extra.as_object().unwrap() {
            c[k] = val.clone();
        }
        sign_token(&keys, c)
    };

    let ctx = v
        .verify(&token(json!(["escurel:runner"]), json!({})))
        .await
        .unwrap();
    assert_eq!(ctx.service, Some(ServiceRole::Runner));
    assert_eq!(ctx.role, Role::Agent, "a runner is not an admin");
    assert_eq!(ctx.issued_at, Some(now));
    let ctx = v
        .verify(&token(json!(["escurel:bridge"]), json!({})))
        .await
        .unwrap();
    assert_eq!(ctx.service, Some(ServiceRole::Bridge));
    let ctx = v
        .verify(&token(json!(["escurel:agent"]), json!({})))
        .await
        .unwrap();
    assert_eq!(ctx.service, None);

    for machine in [
        json!({ "run_id": "01HRUN" }),
        json!({ "skill": "note" }),
        json!({ "agent": "scribe" }),
        json!({ "act": { "sub": "runner:hq" } }),
    ] {
        let ctx = v
            .verify(&token(json!(["escurel:runner"]), machine.clone()))
            .await
            .unwrap();
        assert_eq!(
            ctx.service, None,
            "a machine token ({machine}) holds no service role"
        );
    }

    // Renamed: the default value is then an ordinary group, the new one the role.
    let renamed = OidcVerifier::new(
        OidcConfig::new(issuer.clone(), AUDIENCE.to_owned())
            .with_jwks_uri(format!("{issuer}/protocol/openid-connect/certs"))
            .with_service_roles("ops:runner", "ops:bridge"),
    );
    let ctx = renamed
        .verify(&token(json!(["escurel:runner"]), json!({})))
        .await
        .unwrap();
    assert_eq!(ctx.service, None);
    let ctx = renamed
        .verify(&token(json!(["ops:runner"]), json!({})))
        .await
        .unwrap();
    assert_eq!(ctx.service, Some(ServiceRole::Runner));
}

/// The run's identity on a per-run agent token (workbench backend P1):
/// `run_id` / `root_event_id` / `trace_id` come out on the context so the
/// gateway can stamp lineage and authorise `report_progress`. Absent claims
/// are absent, never an error — a token is not invalid for lacking them, and
/// an empty `run_id` is no run at all.
#[tokio::test]
async fn run_claims_are_exposed_on_auth_context_and_absent_by_default() {
    let server = MockServer::start().await;
    let keys = make_keys();
    mock_jwks(&server, &keys).await;
    let issuer = format!("{}{ISSUER_PATH}", server.uri());
    let v = verifier_pointing_at(&server);
    let now = now();
    let base = json!({
        "iss": issuer, "aud": AUDIENCE, "sub": "agent:inbox-scan", "tenant": "acme",
        "iat": now, "exp": now + 600, "roles": ["escurel:agent"],
    });

    let mut with_run = base.clone();
    with_run["run_id"] = json!("01HRUN");
    with_run["root_event_id"] = json!("01HROOT");
    with_run["trace_id"] = json!("0123456789abcdef0123456789abcdef");
    let ctx = v
        .verify(&sign_token(&keys, with_run))
        .await
        .expect("verify");
    let run = ctx.run.expect("run claims are exposed");
    assert_eq!(run.run_id, "01HRUN");
    assert_eq!(run.root_event_id.as_deref(), Some("01HROOT"));
    assert_eq!(
        run.trace_id.as_deref(),
        Some("0123456789abcdef0123456789abcdef")
    );

    let ctx = v
        .verify(&sign_token(&keys, base.clone()))
        .await
        .expect("verify");
    assert!(ctx.run.is_none(), "an ordinary token belongs to no run");

    let mut empty = base;
    empty["run_id"] = json!("");
    let ctx = v.verify(&sign_token(&keys, empty)).await.expect("verify");
    assert!(ctx.run.is_none(), "an empty run_id is no run");
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

/// A runner→agent internal-delegation token is refused at the gateway surface
/// even when it carries escurel's OWN audience (fleet #801 AD-7 fail-closed).
///
/// The delegation token is signed by the same issuer and verified against the
/// same JWKS as a gateway bearer; its only intended separation is `aud` (the
/// agent's). This test mints one with `aud=escurel` on purpose — so it PASSES
/// the audience gate — and asserts the explicit `purpose` reject refuses it
/// anyway. That is the defense-in-depth the design promises: a leaked delegation
/// bearer is not a candidate `/mcp` credential even under an audience
/// misconfiguration.
#[tokio::test]
async fn a_delegation_purpose_token_is_refused_even_with_escurels_own_audience() {
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
            "aud": AUDIENCE, // escurel's OWN audience → passes the aud gate
            "sub": "escurel-async-runner",
            "tenant": "acme",
            "iat": now,
            "exp": now + 600,
            "roles": [],
            "purpose": "internal_delegation"
        }),
    );

    let err = v
        .verify(&token)
        .await
        .expect_err("delegation purpose must be refused");
    assert!(
        matches!(err, AuthError::PurposeRefused(ref p) if p == "internal_delegation"),
        "expected PurposeRefused, got {err:?}"
    );
}

/// Positive control: the SAME token shape WITHOUT the delegation purpose still
/// verifies — so the test above is about the purpose, not a coincidental
/// rejection.
#[tokio::test]
async fn the_same_token_without_the_delegation_purpose_verifies() {
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
            "sub": "escurel-async-runner",
            "tenant": "acme",
            "iat": now,
            "exp": now + 600,
            "roles": []
        }),
    );

    let ctx = v.verify(&token).await.expect("verify without purpose");
    assert_eq!(ctx.subject, "escurel-async-runner");
    assert_eq!(ctx.tenant_id, "acme");
}

/// An expired bearer is refused the second it expires, not a minute later.
///
/// `jsonwebtoken` defaults `leeway` to 60 seconds. Inherited, that accepted a
/// dead credential for a full minute — and, worse, made a broken
/// re-authentication path look correct: the first version of the expiry test
/// in escurel#442 minted a 5-second bearer, slept, and passed against a runner
/// that provably never re-minted.
///
/// The window is what matters, so the test uses one: a token 30 seconds past
/// `exp` is inside the old default and outside the new policy. A token merely
/// one second past would pass under either if this regressed to a small
/// non-zero leeway, which is exactly the change this must catch.
#[tokio::test]
async fn a_token_past_its_exp_is_refused_without_a_minute_of_grace() {
    let server = MockServer::start().await;
    let keys = make_keys();
    mock_jwks(&server, &keys).await;
    let issuer = format!("{}{ISSUER_PATH}", server.uri());
    let v = verifier_pointing_at(&server);
    let now = now();

    let expired = sign_token(
        &keys,
        json!({
            "iss": issuer,
            "aud": AUDIENCE,
            "sub": "user-42",
            "tenant": "acme",
            "iat": now - 630,
            "exp": now - 30,
            "roles": ["regular-user"]
        }),
    );
    let err = v
        .verify(&expired)
        .await
        .expect_err("a token 30s past exp must be refused");
    assert!(
        format!("{err}").to_lowercase().contains("expired"),
        "the refusal must name the expiry, or a caller cannot tell it from a \
         bad signature and will not know to re-authenticate: {err}"
    );

    // POSITIVE CONTROL: the same claims, still live, verify — so the refusal
    // above is the clock and not the issuer, audience or key.
    let live = sign_token(
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
    assert_eq!(v.verify(&live).await.expect("control").subject, "user-42");
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
async fn groups_claim_array_projects_to_authcontext() {
    // A `roles` claim that is a JSON array of strings projects each
    // element into `AuthContext.groups`. Admin derivation is unchanged
    // (no admin value present → Agent).
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
            "sub": "mara",
            "tenant": "acme",
            "iat": now,
            "exp": now + 600,
            "roles": ["moderator"]
        }),
    );

    let ctx = v.verify(&token).await.expect("verify");
    assert_eq!(ctx.groups, vec!["moderator".to_owned()]);
    assert_eq!(ctx.role, Role::Agent);
}

#[tokio::test]
async fn groups_claim_delimited_string_splits() {
    // A `roles` claim that is a single string is split on whitespace
    // AND commas (Keycloak/Auth0 both occur in the wild), trimmed,
    // empties dropped.
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
            "sub": "mara",
            "tenant": "acme",
            "iat": now,
            "exp": now + 600,
            "roles": "moderator billing,analyst"
        }),
    );

    let ctx = v.verify(&token).await.expect("verify");
    assert_eq!(
        ctx.groups,
        vec![
            "moderator".to_owned(),
            "billing".to_owned(),
            "analyst".to_owned()
        ]
    );
}

#[tokio::test]
async fn groups_claim_absent_is_empty() {
    // No `roles` claim at all (tenant still present) → empty group set.
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
            "sub": "alice",
            "tenant": "acme",
            "iat": now,
            "exp": now + 600,
        }),
    );

    let ctx = v.verify(&token).await.expect("verify");
    assert!(ctx.groups.is_empty(), "groups: {:?}", ctx.groups);
    assert_eq!(ctx.role, Role::Agent);
}

#[tokio::test]
async fn admin_unchanged_when_groups_claim_is_roles() {
    // `groups_claim` defaults to the same `roles` claim admin derives
    // from. The admin value rides into `groups` raw here (the verifier
    // stays dumb); reserved-name + admin-value stripping happens later
    // in the index's `effective_groups`, not in the verifier.
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
            "sub": "otis",
            "tenant": "acme",
            "iat": now,
            "exp": now + 600,
            "roles": ["escurel:admin"]
        }),
    );

    let ctx = v.verify(&token).await.expect("verify");
    assert_eq!(ctx.role, Role::Admin);
    assert!(
        ctx.groups.contains(&"escurel:admin".to_owned()),
        "groups: {:?}",
        ctx.groups
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

/// #510: a per-run agent token names the agent in `sub` and keeps the runner
/// in `act.sub` (RFC 8693 delegation). The verifier must surface that actor,
/// because everything downstream that needs the chain — the provenance stamp,
/// the runner's own lineage-trust guard — can only read what the verifier
/// resolved. A token without `act` has no actor, which is the ordinary case.
#[tokio::test]
async fn an_act_claim_projects_the_delegating_actor_into_authcontext() {
    let server = MockServer::start().await;
    let keys = make_keys();
    mock_jwks(&server, &keys).await;
    let issuer = format!("{}{ISSUER_PATH}", server.uri());
    let v = verifier_pointing_at(&server);
    let now = now();

    let delegated = sign_token(
        &keys,
        json!({
            "iss": issuer,
            "aud": AUDIENCE,
            "sub": "agent:inbox-scan",
            "act": { "sub": "escurel-runner" },
            "tenant": "acme",
            "iat": now,
            "exp": now + 600,
        }),
    );
    let ctx = v.verify(&delegated).await.expect("verify");
    assert_eq!(ctx.subject, "agent:inbox-scan");
    assert_eq!(
        ctx.actor.as_deref(),
        Some("escurel-runner"),
        "the delegation chain must survive verification, or nothing \
         downstream can record who the agent was acting for"
    );

    // The ordinary token: nobody is acting for anybody.
    let plain = sign_token(
        &keys,
        json!({
            "iss": issuer,
            "aud": AUDIENCE,
            "sub": "consultant:alice",
            "tenant": "acme",
            "iat": now,
            "exp": now + 600,
        }),
    );
    let ctx = v.verify(&plain).await.expect("verify");
    assert_eq!(ctx.actor, None, "no act claim, no actor");

    // A malformed `act` is not an actor — never a stringified object, and
    // never an error either: it is a claim we do not understand, so the
    // token verifies as itself with no chain.
    for junk in [json!("escurel-runner"), json!({ "sub": "" }), json!([])] {
        let odd = sign_token(
            &keys,
            json!({
                "iss": issuer,
                "aud": AUDIENCE,
                "sub": "agent:x",
                "act": junk,
                "tenant": "acme",
                "iat": now,
                "exp": now + 600,
            }),
        );
        let ctx = v.verify(&odd).await.expect("verify");
        assert_eq!(ctx.actor, None, "malformed act must not become an actor");
    }
}

/// The operator claim is additive: `escurel:operator` in the roles claim marks the credential
/// as an operator (and, being a superset, also grants the admin role). An admin token without
/// it is not an operator; the operator value is configurable.
#[tokio::test]
async fn operator_claim_is_projected_and_admin_alone_is_not_an_operator() {
    let server = MockServer::start().await;
    let keys = make_keys();
    mock_jwks(&server, &keys).await;
    let issuer = format!("{}{ISSUER_PATH}", server.uri());
    let v = verifier_pointing_at(&server);
    let now = now();
    let mint = |roles: Value| {
        sign_token(
            &keys,
            json!({
                "iss": issuer, "aud": AUDIENCE, "sub": "s", "tenant": "acme",
                "iat": now, "exp": now + 600, "roles": roles,
            }),
        )
    };

    let admin = v
        .verify(&mint(json!(["escurel:admin"])))
        .await
        .expect("verify");
    assert_eq!(admin.role, Role::Admin);
    assert!(!admin.operator, "an admin token is not an operator");

    let op = v
        .verify(&mint(json!(["escurel:operator"])))
        .await
        .expect("verify");
    assert!(op.operator);
    assert_eq!(op.role, Role::Admin, "operator is a superset of admin");

    let agent = v
        .verify(&mint(json!(["escurel:agent"])))
        .await
        .expect("verify");
    assert!(!agent.operator);
    assert_eq!(agent.role, Role::Agent);

    let renamed = OidcVerifier::new(
        OidcConfig::new(issuer.clone(), AUDIENCE.to_owned())
            .with_jwks_uri(format!("{issuer}/protocol/openid-connect/certs"))
            .with_operator_role("ops:root"),
    );
    assert!(
        renamed
            .verify(&mint(json!(["ops:root"])))
            .await
            .expect("verify")
            .operator
    );
    assert!(
        !renamed
            .verify(&mint(json!(["escurel:operator"])))
            .await
            .expect("verify")
            .operator
    );
}

/// A token minted FOR AN AGENT (an `agent` or `skill` claim, or the gateway's workbench purpose) is
/// never admin or operator, whatever its roles say (crew review PR-1, F9). Its groups come from a
/// skill page any member may edit; in a deployment that maps an IdP GROUP to admin
/// (`ESCUREL_AUTH_ADMIN_ROLE_VALUE`), a page listing that group would otherwise mint an admin agent.
#[tokio::test]
async fn an_agent_token_listing_the_admin_or_operator_group_is_not_admin() {
    let server = MockServer::start().await;
    let keys = make_keys();
    mock_jwks(&server, &keys).await;
    let issuer = format!("{}{ISSUER_PATH}", server.uri());
    let v = OidcVerifier::new(
        OidcConfig::new(issuer.clone(), AUDIENCE.to_owned())
            .with_jwks_uri(format!("{issuer}/protocol/openid-connect/certs"))
            .with_admin_role("roles", "ops-admins")
            .with_operator_role("ops-root"),
    );
    let now = now();
    let mint = |extra: Value| {
        let mut claims = json!({
            "iss": issuer, "aud": AUDIENCE, "sub": "agent:x", "tenant": "acme",
            "iat": now, "exp": now + 600,
            "roles": ["escurel:agent", "ops-admins", "ops-root"],
        });
        for (k, val) in extra.as_object().unwrap() {
            claims[k] = val.clone();
        }
        sign_token(&keys, claims)
    };
    for (what, extra) in [
        (
            "agent profile",
            json!({ "agent": "scribe", "skill": "note" }),
        ),
        ("narrowed", json!({ "skill": "note" })),
        (
            "workbench",
            json!({ "purpose": "workbench_agent", "act": { "sub": "alice" } }),
        ),
    ] {
        let ctx = v.verify(&mint(extra)).await.expect("verify");
        assert_eq!(ctx.role, Role::Agent, "{what} must not be admin");
        assert!(!ctx.operator, "{what} must not be an operator");
    }
    // CONTROL: the same roles on a person's token are admin and operator.
    let person = v.verify(&mint(json!({}))).await.expect("verify");
    assert_eq!(person.role, Role::Admin);
    assert!(person.operator);
}

/// A chat bridge's token (#824): under a kid the gateway lists as a bridge's,
/// whatever the bridge signs is bounded by the verifier, not by the bridge.
mod bridge_kid {
    use super::*;

    fn bridge_verifier(server: &MockServer) -> OidcVerifier {
        let issuer = format!("{}{ISSUER_PATH}", server.uri());
        OidcVerifier::new(
            OidcConfig::new(issuer.clone(), AUDIENCE.to_owned())
                .with_jwks_uri(format!("{issuer}/protocol/openid-connect/certs"))
                .with_bridge_issuer(issuer.clone())
                .with_bridge_kid(KID),
        )
    }

    fn claims(server: &MockServer, ttl: u64) -> Value {
        let now = now();
        json!({
            "iss": format!("{}{ISSUER_PATH}", server.uri()), "aud": AUDIENCE,
            "sub": "user:alice", "tenant": "acme", "iat": now, "exp": now + ttl,
            "roles": ["escurel:admin", "escurel:operator", "sales"],
            "bridge_channel": "msteams",
        })
    }

    async fn setup() -> (MockServer, TestKeys, OidcVerifier) {
        let server = MockServer::start().await;
        let keys = make_keys();
        mock_jwks(&server, &keys).await;
        let v = bridge_verifier(&server);
        (server, keys, v)
    }

    #[tokio::test]
    async fn a_bridge_token_is_never_admin_has_no_groups_and_is_acting_via_its_channel() {
        let (server, keys, v) = setup().await;
        let ctx = v
            .verify(&sign_token(&keys, claims(&server, 900)))
            .await
            .expect("a 900 s bridge token verifies");
        assert_eq!(ctx.subject, "user:alice");
        assert_eq!(ctx.role, Role::Agent, "roles are not honoured");
        assert!(!ctx.operator);
        assert!(ctx.groups.is_empty(), "{:?}", ctx.groups);
        assert_eq!(ctx.actor.as_deref(), Some("bridge:msteams"));
        assert_eq!(ctx.run, None);
        // The same token on a verifier with no bridge entry is an ordinary
        // admin token: the restriction is the trust entry's, not the claims'.
        let plain = verifier_pointing_at(&server);
        let ctx = plain
            .verify(&sign_token(&keys, claims(&server, 900)))
            .await
            .unwrap();
        assert_eq!(ctx.role, Role::Admin);
    }

    #[tokio::test]
    async fn a_long_lived_or_agent_shaped_or_unnamed_bridge_token_is_refused() {
        let (server, keys, v) = setup().await;
        let refused = |c: Value| {
            let token = sign_token(&keys, c);
            let v = &v;
            async move {
                match v.verify(&token).await {
                    Err(AuthError::BridgeRefused(why)) => why,
                    other => panic!("expected a bridge refusal, got {other:?}"),
                }
            }
        };
        assert!(refused(claims(&server, 3600)).await.contains("at most 900"));
        let mut late = claims(&server, 900);
        late["iat"] = json!(now() + 3000);
        late["exp"] = json!(now() + 3600);
        assert!(refused(late).await.contains("at most 900"));
        for (claim, value) in [
            ("agent", json!("deploy-bot")),
            ("skill", json!("triage")),
            ("run_id", json!("r-1")),
            ("root_event_id", json!("e-1")),
            ("purpose", json!("workbench_agent")),
            ("act", json!({ "sub": "escurel-runner" })),
        ] {
            let mut c = claims(&server, 900);
            c[claim] = value;
            assert!(refused(c).await.contains(claim), "{claim}");
        }
        for channel in [json!(null), json!("Bridge Runner"), json!(7)] {
            let mut c = claims(&server, 900);
            c["bridge_channel"] = channel;
            assert!(refused(c).await.contains("bridge_channel"));
        }
    }
}

/// The bridge restriction belongs to the TRUST ENTRY, not to a kid (crew
/// security F1): the bridge serves its own entry's JWKS, so whoever controls
/// that response — a compromised bridge host, or the network path to it — can
/// publish a key under any kid. A token under such a kid must not escape the
/// restriction by naming a kid nobody listed.
mod bridge_entry {
    use super::*;

    const BRIDGE_PATH: &str = "/bridge";
    const BRIDGE_KID: &str = "escurel-chat-bridge";
    const ROGUE_KID: &str = "rogue";

    fn sign_with(keys: &TestKeys, kid: &str, claims: &Value) -> String {
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(kid.to_owned());
        let key = EncodingKey::from_rsa_pem(&keys.private_pem).expect("encoding key");
        encode(&header, claims, &key).expect("sign jwt")
    }

    /// A gateway trusting a primary issuer and a bridge entry whose JWKS
    /// publishes the bridge's kid AND a second, rogue one.
    struct Setup {
        primary: MockServer,
        primary_keys: TestKeys,
        bridge: MockServer,
        bridge_keys: TestKeys,
        rogue_keys: TestKeys,
        verifier: OidcVerifier,
    }

    impl Setup {
        async fn start() -> Self {
            let primary = MockServer::start().await;
            let primary_keys = make_keys();
            mock_jwks(&primary, &primary_keys).await;
            let bridge = MockServer::start().await;
            let (bridge_keys, rogue_keys) = (make_keys(), make_keys());
            let jwk = |kid: &str, k: &TestKeys| json!({ "kid": kid, "kty": "RSA", "alg": "RS256", "n": k.n_b64, "e": k.e_b64 });
            Mock::given(method("GET"))
                .and(path(format!("{BRIDGE_PATH}/jwks.json")))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "keys": [jwk(BRIDGE_KID, &bridge_keys), jwk(ROGUE_KID, &rogue_keys)]
                })))
                .mount(&bridge)
                .await;
            let primary_iss = format!("{}{ISSUER_PATH}", primary.uri());
            let bridge_iss = format!("{}{BRIDGE_PATH}", bridge.uri());
            let verifier = OidcVerifier::new(
                OidcConfig::new(primary_iss.clone(), AUDIENCE.to_owned())
                    .with_jwks_uri(format!("{primary_iss}/protocol/openid-connect/certs"))
                    .with_additional_issuer(
                        bridge_iss.clone(),
                        Some(format!("{bridge_iss}/jwks.json")),
                    )
                    .with_bridge_issuer(bridge_iss)
                    .with_bridge_kid(BRIDGE_KID),
            );
            Self {
                primary,
                primary_keys,
                bridge,
                bridge_keys,
                rogue_keys,
                verifier,
            }
        }

        fn claims(&self, server: &MockServer, issuer_path: &str) -> Value {
            let now = now();
            json!({
                "iss": format!("{}{issuer_path}", server.uri()), "aud": AUDIENCE,
                "sub": "user:alice", "tenant": "acme", "iat": now, "exp": now + 600,
                "roles": ["escurel:admin", "escurel:operator"],
                "run_id": "r-1", "root_event_id": "e-1",
                "bridge_channel": "msteams",
            })
        }
    }

    #[tokio::test]
    async fn a_rogue_kid_in_the_bridge_jwks_with_admin_operator_and_run_claims_is_refused() {
        let s = Setup::start().await;
        let rogue = sign_with(&s.rogue_keys, ROGUE_KID, &s.claims(&s.bridge, BRIDGE_PATH));
        match s.verifier.verify(&rogue).await {
            Err(AuthError::BridgeRefused(why)) => {
                assert!(why.contains(ROGUE_KID), "{why}");
            }
            other => panic!("a rogue bridge kid must be refused, got {other:?}"),
        }
        // Even without the run claims a rogue kid is never an admin.
        let mut plain = s.claims(&s.bridge, BRIDGE_PATH);
        for k in ["run_id", "root_event_id", "bridge_channel"] {
            plain.as_object_mut().unwrap().remove(k);
        }
        let ctx = s
            .verifier
            .verify(&sign_with(&s.rogue_keys, ROGUE_KID, &plain))
            .await;
        assert!(ctx.is_err(), "rogue kid without run claims: {ctx:?}");
    }

    #[tokio::test]
    async fn the_bridge_kid_is_restricted_and_the_primary_issuer_is_not() {
        let s = Setup::start().await;
        let mut c = s.claims(&s.bridge, BRIDGE_PATH);
        for k in ["run_id", "root_event_id"] {
            c.as_object_mut().unwrap().remove(k);
        }
        let ctx = s
            .verifier
            .verify(&sign_with(&s.bridge_keys, BRIDGE_KID, &c))
            .await
            .expect("the bridge's own token verifies");
        assert_eq!(ctx.role, Role::Agent);
        assert!(!ctx.operator);
        assert!(
            ctx.bridge,
            "a token from the bridge entry is a bridge context"
        );

        let mut p = s.claims(&s.primary, ISSUER_PATH);
        for k in ["run_id", "root_event_id", "bridge_channel"] {
            p.as_object_mut().unwrap().remove(k);
        }
        let ctx = s
            .verifier
            .verify(&sign_with(&s.primary_keys, KID, &p))
            .await
            .expect("a primary token verifies");
        assert_eq!(ctx.role, Role::Admin);
        assert!(!ctx.bridge);
    }

    /// A bridge kid under another entry is refused rather than treated as
    /// either restricted or ordinary: the kid list is a pin on the bridge
    /// entry, so meeting it elsewhere means the configuration is confused.
    #[tokio::test]
    async fn a_bridge_kid_under_another_entry_is_refused() {
        let s = Setup::start().await;
        let primary_iss = format!("{}{ISSUER_PATH}", s.primary.uri());
        let v = OidcVerifier::new(
            OidcConfig::new(primary_iss.clone(), AUDIENCE.to_owned())
                .with_jwks_uri(format!("{primary_iss}/protocol/openid-connect/certs"))
                .with_bridge_issuer(format!("{}{BRIDGE_PATH}", s.bridge.uri()))
                .with_bridge_kid(KID),
        );
        // A well-formed bridge token in every other respect.
        let mut p = s.claims(&s.primary, ISSUER_PATH);
        for k in ["run_id", "root_event_id"] {
            p.as_object_mut().unwrap().remove(k);
        }
        let got = v.verify(&sign_with(&s.primary_keys, KID, &p)).await;
        assert!(matches!(got, Err(AuthError::BridgeRefused(_))), "{got:?}");
    }
}
