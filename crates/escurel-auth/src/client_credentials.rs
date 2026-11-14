//! An OAuth 2.0 client-credentials token, fetched once and reused until it
//! nears expiry.
//!
//! Two services hold one each: the chat bridge (the Bot Framework connector
//! token it replies with) and the gateway (the Microsoft Graph token it reads
//! an asker's group memberships with, #830). One implementation, so the
//! renew margin, the "never log the body" rule and the error shape cannot
//! drift between them. The HTTP client is the caller's: the gateway hands in
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

/// One client-credentials grant: where to ask, as whom, for what.
pub struct ClientCredentials {
    token_url: String,
    client_id: String,
    client_secret: String,
    scope: String,
    cached: Mutex<Option<(String, Instant)>>,
}

impl std::fmt::Debug for ClientCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientCredentials")
            .field("token_url", &self.token_url)
            .field("client_id", &self.client_id)
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}

impl ClientCredentials {
    #[must_use]
    pub fn new(
        token_url: impl Into<String>,
        client_id: impl Into<String>,
        client_secret: impl Into<String>,
        scope: impl Into<String>,
    ) -> Self {
        Self {
            token_url: token_url.into(),
            client_id: client_id.into(),
            client_secret: client_secret.into(),
            scope: scope.into(),
            cached: Mutex::new(None),
        }
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
            .form(&[
                ("grant_type", "client_credentials"),
                ("client_id", self.client_id.as_str()),
                ("client_secret", self.client_secret.as_str()),
                ("scope", self.scope.as_str()),
            ])
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
