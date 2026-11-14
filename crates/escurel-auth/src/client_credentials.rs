//! An OAuth 2.0 service token — a client-credentials grant (RFC 6749 §4.4)
//! with a client secret or a signed client assertion (RFC 7523 §2.2,
//! `private_key_jwt`: an Okta service app), or a JWT-bearer grant (RFC 7523
//! §2.1, a Google service account) — fetched once and reused until it nears
//! expiry.
//!
//! The chat bridge holds one (the Bot Framework connector token it replies
//! with) and the gateway one per group directory it asks (#830: Microsoft
//! Graph by client secret, Google Cloud Identity by service-account key, Okta
//! by a service app's private key). One
//! implementation, so the renew margin, the "never log the body" rule and the
//! error shape cannot drift between them. The HTTP client is the caller's: the gateway hands in
//! one its egress policy pinned (https, no redirects, checked addresses).

use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::Mutex;

/// Renew the token this long before it expires.
const RENEW_MARGIN: Duration = Duration::from_secs(300);
/// The lifetime assumed when the endpoint names none.
const DEFAULT_TTL: Duration = Duration::from_secs(3600);
/// The most of a token response that is read (crew Q-F5): a token answer is
/// a few KiB; a misbehaving endpoint must not grow the process without limit.
pub const MAX_TOKEN_RESPONSE_BYTES: usize = 64 * 1024;

/// Why no token could be had. Never carries the secret or the response body
/// (an identity provider may echo the request back).
#[derive(Debug, thiserror::Error)]
pub enum ClientCredentialsError {
    #[error("token endpoint unreachable: {0}")]
    Transport(String),
    #[error("token endpoint answered {0}")]
    Status(u16),
    #[error("token endpoint response: {0}")]
    Malformed(String),
    /// The endpoint wants a DPoP proof (RFC 9449), which this client never
    /// sends: Okta answers so for a service app with "Require DPoP" on.
    #[error(
        "token endpoint requires DPoP proof-of-possession, which escurel does not send: turn off \
         \"Require Demonstrating Proof of Possession (DPoP) header in token requests\" on the app"
    )]
    DpopRequired,
}

/// How long a client assertion is good for: Okta refuses one valid for more
/// than an hour; a few minutes covers clock skew and nothing more.
const ASSERTION_TTL_SECS: u64 = 300;

/// How the service proves who it is to the token endpoint.
enum Grant {
    /// `client_id` + `client_secret` (Entra, the Bot Framework).
    Secret {
        client_id: String,
        client_secret: String,
    },
    /// A self-signed RS256 assertion (`iss` = the service account, `aud` =
    /// the token endpoint, optional `sub` for domain-wide delegation).
    JwtBearer {
        issuer: String,
        subject: Option<String>,
        key_id: Option<String>,
        key: jsonwebtoken::EncodingKey,
    },
    /// A client-credentials grant whose client authenticates with an RS256
    /// assertion (`iss` = `sub` = the client id, `aud` = the token endpoint,
    /// a unique `jti`) instead of a secret.
    ClientAssertion {
        client_id: String,
        key_id: Option<String>,
        key: jsonwebtoken::EncodingKey,
    },
}

fn rsa_key(private_key_pem: &str) -> Result<jsonwebtoken::EncodingKey, ClientCredentialsError> {
    jsonwebtoken::EncodingKey::from_rsa_pem(private_key_pem.as_bytes())
        .map_err(|_| ClientCredentialsError::Malformed("not an RSA private key".into()))
}

fn sign(
    key_id: Option<&String>,
    key: &jsonwebtoken::EncodingKey,
    claims: &Value,
) -> Result<String, ClientCredentialsError> {
    let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
    header.kid = key_id.cloned();
    jsonwebtoken::encode(&header, claims, key)
        .map_err(|e| ClientCredentialsError::Malformed(e.to_string()))
}

/// One service-token grant: where to ask, as whom, for what.
pub struct ClientCredentials {
    token_url: String,
    grant: Grant,
    scope: String,
    cached: Mutex<Option<(String, Instant)>>,
}

impl std::fmt::Debug for ClientCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let who = match &self.grant {
            Grant::Secret { client_id, .. } | Grant::ClientAssertion { client_id, .. } => client_id,
            Grant::JwtBearer { issuer, .. } => issuer,
        };
        f.debug_struct("ClientCredentials")
            .field("token_url", &self.token_url)
            .field("as", who)
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}

impl ClientCredentials {
    /// A client-credentials grant with a client secret.
    #[must_use]
    pub fn new(
        token_url: impl Into<String>,
        client_id: impl Into<String>,
        client_secret: impl Into<String>,
        scope: impl Into<String>,
    ) -> Self {
        Self {
            token_url: token_url.into(),
            grant: Grant::Secret {
                client_id: client_id.into(),
                client_secret: client_secret.into(),
            },
            scope: scope.into(),
            cached: Mutex::new(None),
        }
    }

    /// A JWT-bearer grant signed with a service account's RSA key (PEM,
    /// PKCS#8 or PKCS#1), acting as `subject` when given (Google domain-wide
    /// delegation).
    ///
    /// # Errors
    /// The key is not an RSA private key.
    pub fn jwt_bearer(
        token_url: impl Into<String>,
        issuer: impl Into<String>,
        subject: Option<String>,
        key_id: Option<String>,
        private_key_pem: &str,
        scope: impl Into<String>,
    ) -> Result<Self, ClientCredentialsError> {
        Ok(Self {
            token_url: token_url.into(),
            grant: Grant::JwtBearer {
                issuer: issuer.into(),
                subject,
                key_id,
                key: rsa_key(private_key_pem)?,
            },
            scope: scope.into(),
            cached: Mutex::new(None),
        })
    }

    /// A client-credentials grant whose client authenticates with an RSA key
    /// (`private_key_jwt`, PEM, PKCS#8 or PKCS#1) — an Okta service app.
    /// `key_id` is the assertion's `kid`, for an app holding several keys.
    ///
    /// # Errors
    /// The key is not an RSA private key.
    pub fn private_key_jwt(
        token_url: impl Into<String>,
        client_id: impl Into<String>,
        key_id: Option<String>,
        private_key_pem: &str,
        scope: impl Into<String>,
    ) -> Result<Self, ClientCredentialsError> {
        Ok(Self {
            token_url: token_url.into(),
            grant: Grant::ClientAssertion {
                client_id: client_id.into(),
                key_id,
                key: rsa_key(private_key_pem)?,
            },
            scope: scope.into(),
            cached: Mutex::new(None),
        })
    }

    /// The form a token request posts for this grant.
    fn form(&self) -> Result<Vec<(&'static str, String)>, ClientCredentialsError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        Ok(match &self.grant {
            Grant::Secret {
                client_id,
                client_secret,
            } => vec![
                ("grant_type", "client_credentials".to_owned()),
                ("client_id", client_id.clone()),
                ("client_secret", client_secret.clone()),
                ("scope", self.scope.clone()),
            ],
            Grant::JwtBearer {
                issuer,
                subject,
                key_id,
                key,
            } => {
                let mut claims = serde_json::json!({
                    "iss": issuer, "scope": self.scope, "aud": self.token_url,
                    "iat": now, "exp": now + 3600,
                });
                if let Some(sub) = subject {
                    claims["sub"] = serde_json::json!(sub);
                }
                vec![
                    (
                        "grant_type",
                        "urn:ietf:params:oauth:grant-type:jwt-bearer".to_owned(),
                    ),
                    ("assertion", sign(key_id.as_ref(), key, &claims)?),
                ]
            }
            Grant::ClientAssertion {
                client_id,
                key_id,
                key,
            } => {
                // A fresh `jti` per request: the endpoint refuses a replayed assertion.
                let claims = serde_json::json!({
                    "iss": client_id, "sub": client_id, "aud": self.token_url,
                    "iat": now, "exp": now + ASSERTION_TTL_SECS,
                    "jti": ulid::Ulid::new().to_string(),
                });
                vec![
                    ("grant_type", "client_credentials".to_owned()),
                    ("scope", self.scope.clone()),
                    (
                        "client_assertion_type",
                        "urn:ietf:params:oauth:client-assertion-type:jwt-bearer".to_owned(),
                    ),
                    ("client_assertion", sign(key_id.as_ref(), key, &claims)?),
                ]
            }
        })
    }

    /// The token endpoint, for a caller that must vet the URL before
    /// building the client it hands to [`Self::token`].
    #[must_use]
    pub fn token_url(&self) -> &str {
        &self.token_url
    }

    /// The cached token while it is still good, without any request — so a
    /// caller can skip vetting the token endpoint when nothing will be sent
    /// to it (crew Q-F13).
    pub async fn cached(&self) -> Option<String> {
        let slot = self.cached.lock().await;
        slot.as_ref()
            .filter(|(_, renew_at)| Instant::now() < *renew_at)
            .map(|(token, _)| token.clone())
    }

    /// A valid access token: the cached one, or a fresh one fetched with
    /// `http`. Concurrent callers share one fetch.
    ///
    /// # Errors
    /// The endpoint cannot be reached, refuses, or answers without a token.
    pub async fn token(&self, http: &reqwest::Client) -> Result<String, ClientCredentialsError> {
        let mut slot = self.cached.lock().await;
        if let Some((token, renew_at)) = slot.as_ref()
            && Instant::now() < *renew_at
        {
            return Ok(token.clone());
        }
        let resp = http
            .post(&self.token_url)
            .form(&self.form()?)
            .send()
            .await
            .map_err(|e| ClientCredentialsError::Transport(e.without_url().to_string()))?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            // Only the OAuth error code is read, and only to name DPoP; the body is never echoed.
            if status == 400
                && let Ok(body) = read_capped(resp).await
                && let Ok(err) = serde_json::from_slice::<Value>(&body)
                && matches!(
                    err["error"].as_str(),
                    Some("invalid_dpop_proof" | "use_dpop_nonce")
                )
            {
                return Err(ClientCredentialsError::DpopRequired);
            }
            return Err(ClientCredentialsError::Status(status));
        }
        let body: Value = serde_json::from_slice(&read_capped(resp).await?)
            .map_err(|e| ClientCredentialsError::Malformed(e.to_string()))?;
        let token = body["access_token"]
            .as_str()
            .filter(|t| !t.is_empty())
            .ok_or_else(|| ClientCredentialsError::Malformed("no access_token".into()))?
            .to_owned();
        let ttl = body["expires_in"]
            .as_u64()
            .map_or(DEFAULT_TTL, Duration::from_secs);
        *slot = Some((
            token.clone(),
            Instant::now() + ttl.saturating_sub(RENEW_MARGIN),
        ));
        Ok(token)
    }

    /// Forget the cached token (the resource server refused it).
    pub async fn invalidate(&self) {
        *self.cached.lock().await = None;
    }
}

/// The response body, refused once it passes [`MAX_TOKEN_RESPONSE_BYTES`].
async fn read_capped(mut resp: reqwest::Response) -> Result<Vec<u8>, ClientCredentialsError> {
    let too_big = || {
        ClientCredentialsError::Malformed(format!(
            "the answer is larger than {MAX_TOKEN_RESPONSE_BYTES} bytes"
        ))
    };
    if resp
        .content_length()
        .is_some_and(|n| n > MAX_TOKEN_RESPONSE_BYTES as u64)
    {
        return Err(too_big());
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| ClientCredentialsError::Transport(e.without_url().to_string()))?
    {
        if body.len() + chunk.len() > MAX_TOKEN_RESPONSE_BYTES {
            return Err(too_big());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn a_token_is_fetched_once_and_reused_until_invalidated() {
        let idp = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("grant_type=client_credentials"))
            .and(body_string_contains("client_id=app"))
            .and(body_string_contains("scope=s"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    serde_json::json!({ "access_token": "t-1", "expires_in": 3600 }),
                ),
            )
            .expect(2)
            .mount(&idp)
            .await;
        let cc = ClientCredentials::new(format!("{}/token", idp.uri()), "app", "pw", "s");
        let http = reqwest::Client::new();
        assert_eq!(cc.token(&http).await.unwrap(), "t-1");
        assert_eq!(cc.token(&http).await.unwrap(), "t-1");
        cc.invalidate().await;
        assert_eq!(cc.token(&http).await.unwrap(), "t-1");
    }

    #[tokio::test]
    async fn a_jwt_bearer_grant_posts_a_signed_assertion_for_the_service_account() {
        use rsa::pkcs8::EncodePrivateKey;
        let key = rsa::RsaPrivateKey::new(&mut rand::thread_rng(), 2048).unwrap();
        let pem = key.to_pkcs8_pem(rsa::pkcs8::LineEnding::LF).unwrap();
        let idp = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_string_contains(
                "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer",
            ))
            .and(body_string_contains("assertion=ey"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    serde_json::json!({ "access_token": "g-1", "expires_in": 3600 }),
                ),
            )
            .expect(1)
            .mount(&idp)
            .await;
        let cc = ClientCredentials::jwt_bearer(
            format!("{}/token", idp.uri()),
            "sa@project.iam.gserviceaccount.com",
            Some("admin@acme.example".into()),
            Some("k1".into()),
            &pem,
            "s",
        )
        .unwrap();
        assert_eq!(cc.token(&reqwest::Client::new()).await.unwrap(), "g-1");
        assert!(ClientCredentials::jwt_bearer("u", "i", None, None, "not a key", "s").is_err());
    }

    /// RFC 7523 §2.2 client authentication (Okta service apps): a
    /// client-credentials grant whose client proves itself with a signed,
    /// short-lived, single-use assertion instead of a secret.
    #[tokio::test]
    async fn a_private_key_jwt_grant_authenticates_the_client_with_a_fresh_assertion() {
        use rsa::pkcs8::{EncodePrivateKey, EncodePublicKey};
        let key = rsa::RsaPrivateKey::new(&mut rand::thread_rng(), 2048).unwrap();
        let pem = key.to_pkcs8_pem(rsa::pkcs8::LineEnding::LF).unwrap();
        let public = key
            .to_public_key()
            .to_public_key_pem(rsa::pkcs8::LineEnding::LF)
            .unwrap();
        let idp = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth2/v1/token"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    serde_json::json!({ "access_token": "o-1", "expires_in": 3600 }),
                ),
            )
            .mount(&idp)
            .await;
        let token_url = format!("{}/oauth2/v1/token", idp.uri());
        let cc = ClientCredentials::private_key_jwt(
            token_url.clone(),
            "0oa-app",
            Some("kid-1".into()),
            &pem,
            "okta.groups.read okta.users.read",
        )
        .unwrap();
        let http = reqwest::Client::new();
        assert_eq!(cc.token(&http).await.unwrap(), "o-1");
        cc.invalidate().await;
        cc.token(&http).await.unwrap();

        let decoding = jsonwebtoken::DecodingKey::from_rsa_pem(public.as_bytes()).unwrap();
        let mut jtis = Vec::new();
        for req in idp.received_requests().await.unwrap() {
            let url = reqwest::Url::parse(&format!(
                "http://form/?{}",
                String::from_utf8_lossy(&req.body)
            ))
            .unwrap();
            let form: std::collections::HashMap<String, String> =
                url.query_pairs().into_owned().collect();
            assert_eq!(form["grant_type"], "client_credentials");
            assert_eq!(form["scope"], "okta.groups.read okta.users.read");
            assert_eq!(
                form["client_assertion_type"],
                "urn:ietf:params:oauth:client-assertion-type:jwt-bearer"
            );
            assert!(!form.contains_key("client_secret"));
            let header = jsonwebtoken::decode_header(&form["client_assertion"]).unwrap();
            assert_eq!(header.kid.as_deref(), Some("kid-1"));
            let mut v = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::RS256);
            v.set_audience(&[&token_url]);
            v.set_issuer(&["0oa-app"]);
            let claims = jsonwebtoken::decode::<Value>(&form["client_assertion"], &decoding, &v)
                .unwrap()
                .claims;
            assert_eq!(claims["sub"], "0oa-app");
            let (iat, exp) = (
                claims["iat"].as_u64().unwrap(),
                claims["exp"].as_u64().unwrap(),
            );
            assert!(exp > iat && exp - iat <= 300, "short-lived: {claims}");
            jtis.push(claims["jti"].as_str().unwrap().to_owned());
        }
        assert_eq!(jtis.len(), 2);
        assert_ne!(jtis[0], jtis[1], "every assertion is single-use");
        assert!(ClientCredentials::private_key_jwt("u", "c", None, "not a key", "s").is_err());
    }

    /// Okta can require DPoP of a service app; escurel does not send DPoP
    /// proofs, so that answer is named for the admin to fix.
    #[tokio::test]
    async fn a_token_endpoint_that_requires_dpop_is_named() {
        let idp = MockServer::start().await;
        for (route, code) in [("/a", "invalid_dpop_proof"), ("/b", "use_dpop_nonce")] {
            Mock::given(method("POST"))
                .and(path(route))
                .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                    "error": code, "error_description": "The DPoP proof JWT header is missing."
                })))
                .mount(&idp)
                .await;
            let cc = ClientCredentials::new(format!("{}{route}", idp.uri()), "app", "pw", "s");
            let err = cc.token(&reqwest::Client::new()).await.unwrap_err();
            assert!(matches!(err, ClientCredentialsError::DpopRequired), "{err}");
            assert!(err.to_string().contains("DPoP"), "{err}");
        }
    }

    #[tokio::test]
    async fn a_refusal_names_the_status_and_never_the_body() {
        let idp = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(401).set_body_string("client_secret=pw"))
            .mount(&idp)
            .await;
        let cc = ClientCredentials::new(format!("{}/token", idp.uri()), "app", "pw", "s");
        let err = cc.token(&reqwest::Client::new()).await.unwrap_err();
        assert_eq!(err.to_string(), "token endpoint answered 401");
    }

    #[tokio::test]
    async fn an_oversized_token_answer_is_refused_and_a_good_token_is_cached() {
        let idp = MockServer::start().await;
        let huge = "x".repeat(MAX_TOKEN_RESPONSE_BYTES + 1);
        Mock::given(method("POST"))
            .and(path("/huge"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "access_token": "t", "padding": huge })),
            )
            .mount(&idp)
            .await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "access_token": "t-1" })),
            )
            .mount(&idp)
            .await;
        let http = reqwest::Client::new();
        let big = ClientCredentials::new(format!("{}/huge", idp.uri()), "app", "pw", "s");
        let err = big.token(&http).await.unwrap_err();
        assert!(err.to_string().contains("larger than"), "{err}");
        let cc = ClientCredentials::new(format!("{}/token", idp.uri()), "app", "pw", "s");
        assert_eq!(cc.cached().await, None);
        cc.token(&http).await.unwrap();
        assert_eq!(cc.cached().await.as_deref(), Some("t-1"));
        cc.invalidate().await;
        assert_eq!(cc.cached().await, None);
    }
}
