//! The Microsoft Bot Framework pieces both the chat bridge and the gateway
//! need (#835): the inbound activity-token check, the connector's
//! conversation-members answer, and the defaults of a public-cloud bot.
//!
//! An activity token is a JWT the Bot Framework signs (RS256) with a key its
//! OpenID metadata names. It is accepted only for the configured issuer, with
//! `aud` = the bot's app id, inside `exp` / `nbf` (five minutes' skew), signed
//! by a key whose JWKS `endorsements` (when listed) include `msteams`, and
//! with a `serviceurl` claim equal to the `serviceUrl` the caller is about to
//! trust — so a token for one connector cannot point a reply, or a roster
//! lookup, somewhere else. The token names no user: who asked is the
//! connector's answer ([`ConversationMember`]), fetched with the bot's own
//! client-credentials token ([`crate::ClientCredentials`]).

use std::time::Duration;

use jsonwebtoken::{Algorithm, Validation, decode, decode_header};
use serde_json::Value;
use tokio::sync::OnceCell;

use crate::JwksCache;

/// The public cloud's OpenID metadata for channel tokens.
pub const DEFAULT_OPENID_METADATA: &str =
    "https://login.botframework.com/v1/.well-known/openidconfiguration";
/// The `iss` of a channel token.
pub const DEFAULT_ISSUER: &str = "https://api.botframework.com";
/// Where a bot gets its connector token.
pub const DEFAULT_TOKEN_URL: &str =
    "https://login.microsoftonline.com/botframework.com/oauth2/v2.0/token";
/// The connector token's scope.
pub const DEFAULT_TOKEN_SCOPE: &str = "https://api.botframework.com/.default";
/// The one Bot Framework channel escurel answers.
pub const TEAMS_CHANNEL: &str = "msteams";

/// Clock skew the Bot Framework SDKs allow on `exp` / `nbf`.
const LEEWAY_SECS: u64 = 300;
/// Re-fetch the signing keys at most this often (and on an unknown `kid`).
const JWKS_TTL: Duration = Duration::from_secs(3600);

/// Why an activity token is not accepted.
#[derive(Debug, thiserror::Error)]
pub enum ActivityTokenError {
    #[error("activity token rejected: {0}")]
    BadToken(String),
    #[error("Bot Framework signing keys unavailable: {0}")]
    Keys(String),
}

/// Checks activity tokens for one bot.
pub struct ActivityVerifier {
    app_id: String,
    issuer: String,
    openid_metadata_url: String,
    http: reqwest::Client,
    keys: OnceCell<JwksCache>,
}

impl std::fmt::Debug for ActivityVerifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActivityVerifier")
            .field("app_id", &self.app_id)
            .field("issuer", &self.issuer)
            .finish_non_exhaustive()
    }
}

impl ActivityVerifier {
    #[must_use]
    pub fn new(
        app_id: impl Into<String>,
        issuer: impl Into<String>,
        openid_metadata_url: impl Into<String>,
    ) -> Self {
        Self {
            app_id: app_id.into(),
            issuer: issuer.into(),
            openid_metadata_url: openid_metadata_url.into(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap_or_default(),
            keys: OnceCell::new(),
        }
    }

    async fn keys(&self) -> Result<&JwksCache, ActivityTokenError> {
        self.keys
            .get_or_try_init(|| async {
                let keys =
                    |e: reqwest::Error| ActivityTokenError::Keys(e.without_url().to_string());
                let meta: Value = self
                    .http
                    .get(&self.openid_metadata_url)
                    .send()
                    .await
                    .and_then(reqwest::Response::error_for_status)
                    .map_err(keys)?
                    .json()
                    .await
                    .map_err(keys)?;
                let jwks_uri = meta["jwks_uri"]
                    .as_str()
                    .ok_or_else(|| ActivityTokenError::Keys("metadata has no jwks_uri".into()))?;
                Ok(JwksCache::new(jwks_uri, JWKS_TTL))
            })
            .await
    }

    /// Accept `token` for an activity that names `service_url`; its claims.
    ///
    /// # Errors
    /// [`ActivityTokenError::BadToken`] for any token this bot must not trust;
    /// [`ActivityTokenError::Keys`] when the signing keys cannot be fetched.
    pub async fn verify(
        &self,
        token: &str,
        service_url: &str,
    ) -> Result<Value, ActivityTokenError> {
        let bad = |why: String| ActivityTokenError::BadToken(why);
        let header = decode_header(token).map_err(|e| bad(e.to_string()))?;
        if header.alg != Algorithm::RS256 {
            return Err(bad(format!("alg {:?}", header.alg)));
        }
        let kid = header.kid.ok_or_else(|| bad("no kid".into()))?;
        let (key, endorsements) = self
            .keys()
            .await?
            .endorsed_key_for_kid(&kid)
            .await
            .map_err(|e| bad(e.to_string()))?;
        // A token the Bot Framework issued for another channel this bot is
        // enabled on must not enter the Teams identity path.
        if let Some(channels) = &endorsements
            && !channels.iter().any(|c| c == TEAMS_CHANNEL)
        {
            return Err(bad(format!(
                "signing key `{kid}` is not endorsed for {TEAMS_CHANNEL}"
            )));
        }
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_issuer(&[&self.issuer]);
        validation.set_audience(&[&self.app_id]);
        validation.leeway = LEEWAY_SECS;
        validation.validate_nbf = true;
        let claims = decode::<Value>(token, &key, &validation)
            .map_err(|e| bad(e.to_string()))?
            .claims;
        let bound = claims["serviceurl"]
            .as_str()
            .or_else(|| claims["serviceUrl"].as_str())
            .ok_or_else(|| bad("no serviceurl claim".into()))?;
        if bound.trim_end_matches('/') != service_url.trim_end_matches('/') {
            return Err(bad("the activity's serviceUrl is not the token's".into()));
        }
        Ok(claims)
    }
}

/// `{serviceUrl}/v3/conversations/{conversation}/members/{user}`: who a user
/// in a conversation is — a 404 (or 403) when they are not in it.
#[must_use]
pub fn member_url(service_url: &str, conversation_id: &str, user_id: &str) -> String {
    format!(
        "{}/v3/conversations/{}/members/{}",
        service_url.trim_end_matches('/'),
        path_segment(conversation_id),
        path_segment(user_id)
    )
}

/// Percent-encode everything but RFC 3986 unreserved characters: Teams
/// conversation ids carry `:`, `;`, `=` and `@`.
#[must_use]
pub fn path_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// A conversation member as the connector names them (a `TeamsChannelAccount`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConversationMember {
    pub aad_object_id: Option<String>,
    pub email: Option<String>,
    pub upn: Option<String>,
    pub tenant_id: Option<String>,
}

impl ConversationMember {
    /// Read the members API's answer; an empty field is absent.
    #[must_use]
    pub fn from_json(member: &Value) -> Self {
        let field = |k: &str| {
            member[k]
                .as_str()
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
        };
        Self {
            aad_object_id: field("aadObjectId"),
            email: field("email"),
            upn: field("userPrincipalName"),
            tenant_id: field("tenantId"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_encoded_as_one_segment_and_the_url_has_one_slash() {
        assert_eq!(path_segment("a:1b;messageid=2"), "a%3A1b%3Bmessageid%3D2");
        assert_eq!(
            member_url("https://smba.example/emea/", "19:x@thread.v2", "29:u"),
            "https://smba.example/emea/v3/conversations/19%3Ax%40thread.v2/members/29%3Au"
        );
    }

    #[test]
    fn a_member_answer_drops_empty_fields() {
        let m = ConversationMember::from_json(&serde_json::json!({
            "aadObjectId": "oid", "email": "", "userPrincipalName": "a@x", "tenantId": "t" }));
        assert_eq!(m.aad_object_id.as_deref(), Some("oid"));
        assert_eq!(m.email, None);
        assert_eq!(m.upn.as_deref(), Some("a@x"));
        assert_eq!(m.tenant_id.as_deref(), Some("t"));
    }
}
