//! An OAuth 2.0 service token — a client-credentials grant (RFC 6749 §4.4)
//! or a JWT-bearer grant (RFC 7523, a Google service account) — fetched once
//! and reused until it nears expiry.
//!
//! The chat bridge holds one (the Bot Framework connector token it replies
//! with) and the gateway one per group directory it asks (#830: Microsoft
//! Graph by client secret, Google Cloud Identity by service-account key). One
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
}

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
            Grant::Secret { client_id, .. } => client_id,
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
        let key = jsonwebtoken::EncodingKey::from_rsa_pem(private_key_pem.as_bytes())
            .map_err(|_| ClientCredentialsError::Malformed("not an RSA private key".into()))?;
        Ok(Self {
            token_url: token_url.into(),
            grant: Grant::JwtBearer {
                issuer: issuer.into(),
                subject,
                key_id,
                key,
            },
            scope: scope.into(),
            cached: Mutex::new(None),
        })
    }

    /// The form a token request posts for this grant.
    fn form(&self) -> Result<Vec<(&'static str, String)>, ClientCredentialsError> {
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
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs());
                let mut claims = serde_json::json!({
                    "iss": issuer, "scope": self.scope, "aud": self.token_url,
                    "iat": now, "exp": now + 3600,
                });
                if let Some(sub) = subject {
                    claims["sub"] = serde_json::json!(sub);
                }
                let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
                header.kid.clone_from(key_id);
                let assertion = jsonwebtoken::encode(&header, &claims, key)
                    .map_err(|e| ClientCredentialsError::Malformed(e.to_string()))?;
                vec![
                    (
                        "grant_type",
                        "urn:ietf:params:oauth:grant-type:jwt-bearer".to_owned(),
                    ),
                    ("assertion", assertion),
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
            return Err(ClientCredentialsError::Status(resp.status().as_u16()));
        }
        let body: Value = resp
            .json()
            .await
            .map_err(|e| ClientCredentialsError::Malformed(e.without_url().to_string()))?;
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
}
