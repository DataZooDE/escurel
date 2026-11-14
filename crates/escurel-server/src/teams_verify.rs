//! The gateway checks a Teams asker itself (#835).
//!
//! Without this, `mint_user_token` trusts the bridge's `escurel:bridge`
//! credential about who asked, so a stolen credential could mint a token for
//! anyone in the tenant. With `ESCUREL_AUTH_TEAMS_APP_ID` (the bot's own app
//! registration), every mint must carry the activity's Bot Framework token,
//! and the gateway:
//!
//! 1. verifies it ([`escurel_auth::bot_framework::ActivityVerifier`], the
//!    bridge's own check) and that its `serviceurl` is the turn's
//!    `service_url`, whose host must be on `ESCUREL_AUTH_TEAMS_SERVICE_URL_HOSTS`
//!    — the gateway never sends its bot bearer anywhere else;
//! 2. asks that connector who the user is in that conversation
//!    (`GET …/v3/conversations/{id}/members/{user}`, through the egress
//!    policy, with the bot's client-credentials token): a 404 / 403 means
//!    they are not in it — the roster check, for 1:1, group and channel turns;
//! 3. hands back MICROSOFT's answer, which the mint then uses instead of what
//!    the bridge asserted.
//!
//! A positive answer is cached for [`ROSTER_TTL`] (bounded), so a busy chat
//! does not call the connector on every turn.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use escurel_auth::ClientCredentials;
use escurel_auth::bot_framework::{
    self as bf, ActivityTokenError, ActivityVerifier, ConversationMember,
};
use tokio::sync::Mutex;

use crate::config::{ConfigError, EnvSource, secret_or_file};
use crate::egress::Egress;
use crate::group_directory::{DirectoryError, authorized_get};
use crate::rate_limit::MintRateLimits;

/// The connector hosts a public-cloud Teams bot is called back on.
pub const DEFAULT_SERVICE_URL_HOSTS: &[&str] = &["smba.trafficmanager.net", "*.botframework.com"];
/// How long a positive roster answer is reused.
pub const ROSTER_TTL: Duration = Duration::from_secs(5 * 60);
/// The most roster answers kept at once.
const MAX_ROSTER_ENTRIES: usize = 10_000;
/// The egress limiter key the connector calls share.
const LIMITER: &str = "teams-connector";

/// Every `ESCUREL_AUTH_TEAMS_*` key: setting any one names the feature.
pub(crate) const TEAMS_KEYS: &[&str] = &[
    "ESCUREL_AUTH_TEAMS_APP_ID",
    "ESCUREL_AUTH_TEAMS_APP_SECRET",
    "ESCUREL_AUTH_TEAMS_APP_SECRET_FILE",
    "ESCUREL_AUTH_TEAMS_OPENID_METADATA",
    "ESCUREL_AUTH_TEAMS_ISSUER",
    "ESCUREL_AUTH_TEAMS_TOKEN_URL",
    "ESCUREL_AUTH_TEAMS_TOKEN_SCOPE",
    "ESCUREL_AUTH_TEAMS_SERVICE_URL_HOSTS",
];
/// The mint caps' keys.
pub(crate) const RATE_KEYS: &[&str] = &[
    "ESCUREL_AUTH_BRIDGE_MINT_PER_MINUTE",
    "ESCUREL_AUTH_BRIDGE_MINT_PER_ASKER_PER_MINUTE",
];

/// `ESCUREL_AUTH_TEAMS_*`: the bot's app registration, the Bot Framework
/// endpoints, and the connector hosts the gateway may call.
#[derive(Clone, PartialEq, Eq)]
pub struct TeamsVerifyConfig {
    pub app_id: String,
    pub app_secret: String,
    pub openid_metadata_url: String,
    pub issuer: String,
    pub token_url: String,
    pub token_scope: String,
    /// Exact hosts, or `*.suffix` for any host under it.
    pub service_url_hosts: Vec<String>,
    pub roster_ttl: Duration,
}

impl std::fmt::Debug for TeamsVerifyConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TeamsVerifyConfig")
            .field("app_id", &self.app_id)
            .field("issuer", &self.issuer)
            .field("service_url_hosts", &self.service_url_hosts)
            .finish_non_exhaustive()
    }
}

impl TeamsVerifyConfig {
    /// The public-cloud defaults for the bot `app_id`.
    #[must_use]
    pub fn new(app_id: impl Into<String>, app_secret: impl Into<String>) -> Self {
        Self {
            app_id: app_id.into(),
            app_secret: app_secret.into(),
            openid_metadata_url: bf::DEFAULT_OPENID_METADATA.to_owned(),
            issuer: bf::DEFAULT_ISSUER.to_owned(),
            token_url: bf::DEFAULT_TOKEN_URL.to_owned(),
            token_scope: bf::DEFAULT_TOKEN_SCOPE.to_owned(),
            service_url_hosts: DEFAULT_SERVICE_URL_HOSTS
                .iter()
                .map(|h| (*h).to_owned())
                .collect(),
            roster_ttl: ROSTER_TTL,
        }
    }
}

/// The first `ESCUREL_AUTH_TEAMS_*` / mint-cap key that is set, for a
/// gateway that does not mint for a bridge at all.
pub(crate) fn stray_key(env: &dyn EnvSource) -> Option<&'static str> {
    TEAMS_KEYS
        .iter()
        .chain(RATE_KEYS)
        .find(|k| env.get(k).is_some_and(|v| !v.trim().is_empty()))
        .copied()
}

/// `ESCUREL_AUTH_TEAMS_*`, validated whole: unset is `None` (with one boot
/// WARN, the gateway then trusts the bridge for who asked); the app id set
/// makes everything else required or defaulted; any other key without it
/// refuses to start.
///
/// # Errors
/// A key without the app id, a missing secret, a URL that is not https, an
/// empty host list or a malformed host.
pub(crate) fn teams_from(env: &dyn EnvSource) -> Result<Option<TeamsVerifyConfig>, ConfigError> {
    let get = |k: &str| env.get(k).filter(|v| !v.trim().is_empty());
    let refuse = |why: String| Err(ConfigError::Bridge(why));
    let Some(app_id) = get("ESCUREL_AUTH_TEAMS_APP_ID") else {
        if let Some(k) = TEAMS_KEYS.iter().find(|k| get(k).is_some()) {
            return refuse(format!(
                "{k} is set but ESCUREL_AUTH_TEAMS_APP_ID is not: the Teams check needs the bot's \
                 app id"
            ));
        }
        tracing::warn!(
            "mint_user_token trusts the chat bridge for WHO asked: a stolen escurel:bridge \
             credential could mint a token for anyone. Set ESCUREL_AUTH_TEAMS_APP_ID and \
             ESCUREL_AUTH_TEAMS_APP_SECRET[_FILE] (the bot's app registration) so the gateway \
             verifies the Teams activity and its roster itself"
        );
        return Ok(None);
    };
    let app_secret = secret_or_file(
        "ESCUREL_AUTH_TEAMS_APP_SECRET_FILE",
        "set either ESCUREL_AUTH_TEAMS_APP_SECRET or …_FILE, not both",
        get("ESCUREL_AUTH_TEAMS_APP_SECRET"),
        get("ESCUREL_AUTH_TEAMS_APP_SECRET_FILE"),
    )?
    .ok_or_else(|| {
        ConfigError::Bridge(
            "ESCUREL_AUTH_TEAMS_APP_SECRET (or …_FILE) is required with ESCUREL_AUTH_TEAMS_APP_ID"
                .to_owned(),
        )
    })?;
    let mut cfg = TeamsVerifyConfig::new(app_id, app_secret);
    for (key, slot) in [
        (
            "ESCUREL_AUTH_TEAMS_OPENID_METADATA",
            &mut cfg.openid_metadata_url,
        ),
        ("ESCUREL_AUTH_TEAMS_TOKEN_URL", &mut cfg.token_url),
    ] {
        if let Some(url) = get(key) {
            *slot = url;
        }
        crate::group_directory::config::directory_url(key, slot)?;
    }
    if let Some(issuer) = get("ESCUREL_AUTH_TEAMS_ISSUER") {
        cfg.issuer = issuer;
    }
    if let Some(scope) = get("ESCUREL_AUTH_TEAMS_TOKEN_SCOPE") {
        cfg.token_scope = scope;
    }
    if let Some(raw) = get("ESCUREL_AUTH_TEAMS_SERVICE_URL_HOSTS") {
        cfg.service_url_hosts = raw
            .split(',')
            .map(|h| h.trim().to_ascii_lowercase())
            .filter(|h| !h.is_empty())
            .collect();
        if cfg.service_url_hosts.is_empty() {
            return refuse("ESCUREL_AUTH_TEAMS_SERVICE_URL_HOSTS names no host".to_owned());
        }
        if let Some(bad) = cfg.service_url_hosts.iter().find(|h| {
            let name = h.strip_prefix("*.").unwrap_or(h);
            name.is_empty()
                || name.contains('*')
                || !name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':'))
        }) {
            return refuse(format!(
                "ESCUREL_AUTH_TEAMS_SERVICE_URL_HOSTS: `{bad}` is a host or `*.suffix`"
            ));
        }
    }
    Ok(Some(cfg))
}

/// `ESCUREL_AUTH_BRIDGE_MINT_PER_MINUTE` / `…_PER_ASKER_PER_MINUTE`: whole
/// numbers, at least 1.
///
/// # Errors
/// A value that is not a positive whole number.
pub(crate) fn rate_from(env: &dyn EnvSource) -> Result<MintRateLimits, ConfigError> {
    let mut limits = MintRateLimits::default();
    for (key, slot) in [
        (RATE_KEYS[0], &mut limits.per_credential_per_minute),
        (RATE_KEYS[1], &mut limits.per_asker_per_minute),
    ] {
        let Some(raw) = env.get(key).filter(|v| !v.trim().is_empty()) else {
            continue;
        };
        match raw.trim().parse::<u32>() {
            Ok(n) if n > 0 => *slot = n,
            _ => {
                return Err(ConfigError::InvalidValue {
                    var: key,
                    value: raw,
                    reason: "a whole number of mints per minute, at least 1",
                });
            }
        }
    }
    Ok(limits)
}

/// Why the gateway would not accept the bridge's account of a Teams turn.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TeamsVerifyError {
    /// The activity token is missing or bad, or binds another connector, or
    /// the connector's host is not allow-listed. Never retryable.
    #[error("the Teams activity could not be verified: {0}")]
    Unverified(String),
    /// The connector says the user is not in the conversation (404 / 403).
    #[error("the sender is not in this Teams conversation")]
    NotInConversation,
    /// The connector (or its token endpoint) did not answer.
    #[error("the Teams connector cannot check the sender: {why}")]
    Unavailable { why: String, retryable: bool },
}

type RosterKey = (String, String, String);

/// The built check: the activity verifier, the bot's connector credential,
/// and the roster cache.
pub struct TeamsVerifier {
    hosts: Vec<String>,
    ttl: Duration,
    activity: ActivityVerifier,
    connector: ClientCredentials,
    egress: Arc<Egress>,
    roster: Mutex<HashMap<RosterKey, (ConversationMember, Instant)>>,
}

impl TeamsVerifier {
    #[must_use]
    pub fn new(cfg: &TeamsVerifyConfig, egress: &Arc<Egress>) -> Self {
        Self {
            hosts: cfg.service_url_hosts.clone(),
            ttl: cfg.roster_ttl.min(ROSTER_TTL),
            activity: ActivityVerifier::new(
                cfg.app_id.clone(),
                cfg.issuer.clone(),
                cfg.openid_metadata_url.clone(),
            ),
            connector: ClientCredentials::new(
                cfg.token_url.clone(),
                cfg.app_id.clone(),
                cfg.app_secret.clone(),
                cfg.token_scope.clone(),
            ),
            egress: Arc::clone(egress),
            roster: Mutex::new(HashMap::new()),
        }
    }

    /// Who `user_id` is in `conversation_id`, as Microsoft says — after
    /// `activity_token` proved the turn came through the connector at
    /// `service_url`.
    ///
    /// # Errors
    /// See [`TeamsVerifyError`].
    pub async fn member(
        &self,
        activity_token: Option<&str>,
        service_url: &str,
        conversation_id: &str,
        user_id: &str,
    ) -> Result<ConversationMember, TeamsVerifyError> {
        let unverified = |why: &str| Err(TeamsVerifyError::Unverified(why.to_owned()));
        let Some(token) = activity_token.filter(|t| !t.is_empty()) else {
            return unverified("the bridge forwarded no activity token");
        };
        if conversation_id.is_empty() || user_id.is_empty() {
            return unverified("the turn names no conversation or user id");
        }
        self.activity
            .verify(token, service_url)
            .await
            .map_err(|e| match e {
                ActivityTokenError::BadToken(why) => TeamsVerifyError::Unverified(why),
                ActivityTokenError::Keys(why) => TeamsVerifyError::Unavailable {
                    why,
                    retryable: true,
                },
            })?;
        let base = reqwest::Url::parse(service_url)
            .map_err(|_| TeamsVerifyError::Unverified("the serviceUrl is not a URL".into()))?;
        if !base
            .host_str()
            .is_some_and(|h| host_allowed(&self.hosts, h))
        {
            tracing::warn!(host = ?base.host_str(),
                "mint_user_token: a Teams serviceUrl off ESCUREL_AUTH_TEAMS_SERVICE_URL_HOSTS");
            return unverified("the serviceUrl's host is not an allowed Teams connector");
        }
        let key = (
            service_url.trim_end_matches('/').to_owned(),
            conversation_id.to_owned(),
            user_id.to_owned(),
        );
        if let Some((m, at)) = self.roster.lock().await.get(&key)
            && at.elapsed() < self.ttl
        {
            return Ok(m.clone());
        }
        let url = bf::member_url(service_url, conversation_id, user_id);
        let (answer, _) = authorized_get(&self.egress, &self.connector, LIMITER, &base, &url, &[])
            .await
            .map_err(|e| match e {
                DirectoryError::NotFound(_) | DirectoryError::Forbidden(_) => {
                    TeamsVerifyError::NotInConversation
                }
                DirectoryError::Retryable(why) => TeamsVerifyError::Unavailable {
                    why,
                    retryable: true,
                },
                DirectoryError::Unauthorized(why) | DirectoryError::Permanent(why) => {
                    tracing::error!(error = %why,
                        "mint_user_token: the Teams connector refused the gateway; check \
                         ESCUREL_AUTH_TEAMS_APP_ID / _APP_SECRET");
                    TeamsVerifyError::Unavailable {
                        why,
                        retryable: false,
                    }
                }
            })?;
        let member = ConversationMember::from_json(&answer);
        let mut roster = self.roster.lock().await;
        if roster.len() >= MAX_ROSTER_ENTRIES {
            let ttl = self.ttl;
            roster.retain(|_, (_, at)| at.elapsed() < ttl);
            if roster.len() >= MAX_ROSTER_ENTRIES
                && let Some(oldest) = roster
                    .iter()
                    .min_by_key(|(_, (_, at))| *at)
                    .map(|(k, _)| k.clone())
            {
                roster.remove(&oldest);
            }
        }
        roster.insert(key, (member.clone(), Instant::now()));
        Ok(member)
    }
}

/// Whether `host` is on the allow-list: an exact entry, or under a `*.suffix`.
fn host_allowed(hosts: &[String], host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    hosts.iter().any(|h| match h.strip_prefix("*.") {
        Some(suffix) => host
            .strip_suffix(suffix)
            .is_some_and(|rest| rest.ends_with('.') && rest.len() > 1),
        None => *h == host,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_allow_list_matches_exact_hosts_and_dot_suffixes_only() {
        let hosts: Vec<String> = DEFAULT_SERVICE_URL_HOSTS
            .iter()
            .map(|h| (*h).to_owned())
            .collect();
        assert!(host_allowed(&hosts, "smba.trafficmanager.net"));
        assert!(host_allowed(&hosts, "SMBA.trafficmanager.net"));
        assert!(host_allowed(&hosts, "emea.botframework.com"));
        assert!(!host_allowed(&hosts, "botframework.com"));
        assert!(!host_allowed(&hosts, "evilbotframework.com"));
        assert!(!host_allowed(
            &hosts,
            "smba.trafficmanager.net.evil.example"
        ));
    }

    #[test]
    fn the_config_is_all_or_nothing_and_the_caps_are_positive() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == k)
                    .map(|(_, v)| (*v).to_owned())
            }
        };
        assert_eq!(teams_from(&env(&[])).unwrap(), None);
        assert!(teams_from(&env(&[("ESCUREL_AUTH_TEAMS_APP_SECRET", "s")])).is_err());
        assert!(teams_from(&env(&[("ESCUREL_AUTH_TEAMS_APP_ID", "a")])).is_err());
        let cfg = teams_from(&env(&[
            ("ESCUREL_AUTH_TEAMS_APP_ID", "a"),
            ("ESCUREL_AUTH_TEAMS_APP_SECRET", "s"),
        ]))
        .unwrap()
        .unwrap();
        assert_eq!(cfg.service_url_hosts.len(), 2);
        assert!(
            teams_from(&env(&[
                ("ESCUREL_AUTH_TEAMS_APP_ID", "a"),
                ("ESCUREL_AUTH_TEAMS_APP_SECRET", "s"),
                ("ESCUREL_AUTH_TEAMS_SERVICE_URL_HOSTS", "*.*.x"),
            ]))
            .is_err()
        );
        assert_eq!(rate_from(&env(&[])).unwrap(), MintRateLimits::default());
        assert!(rate_from(&env(&[("ESCUREL_AUTH_BRIDGE_MINT_PER_MINUTE", "0")])).is_err());
        assert!(
            rate_from(&env(&[(
                "ESCUREL_AUTH_BRIDGE_MINT_PER_ASKER_PER_MINUTE",
                "ten"
            )]))
            .is_err()
        );
    }
}
