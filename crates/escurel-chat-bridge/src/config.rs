//! The `ESCUREL_BRIDGE_*` surface (12-factor: env only). Every key is
//! registered in `escurel-server/src/config_keys.rs` and rendered into
//! `docs/deploy/env.md`.

use std::time::Duration;

/// Microsoft's public Bot Framework endpoints — the defaults a real tenant uses.
pub const BOT_FRAMEWORK_OPENID_METADATA: &str =
    "https://login.botframework.com/v1/.well-known/openidconfiguration";
pub const BOT_FRAMEWORK_ISSUER: &str = "https://api.botframework.com";
pub const BOT_FRAMEWORK_TOKEN_URL: &str =
    "https://login.microsoftonline.com/botframework.com/oauth2/v2.0/token";
pub const BOT_FRAMEWORK_SCOPE: &str = "https://api.botframework.com/.default";

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{0} is required")]
    Missing(&'static str),
    #[error("{key}: cannot parse `{value}`")]
    Invalid { key: &'static str, value: String },
    #[error("{key}: {source}")]
    Read {
        key: &'static str,
        source: std::io::Error,
    },
}

/// How the bridge reaches and acts on the gateway.
#[derive(Debug, Clone)]
pub struct GatewayConfig {
    pub url: String,
    pub tenant: String,
    /// The issuer the gateway trusts for this key (its JWKS publishes it).
    pub issuer: String,
    pub audience: String,
    pub kid: Option<String>,
    /// RSA private key (PEM) the bridge signs the asker's short-lived token with.
    pub signing_key: String,
    pub token_ttl_secs: u64,
}

/// The Teams (Bot Framework) channel.
#[derive(Debug, Clone)]
pub struct TeamsConfig {
    pub app_id: String,
    pub app_password: String,
    pub openid_metadata_url: String,
    pub issuer: String,
    pub token_url: String,
    pub token_scope: String,
}

#[derive(Debug, Clone)]
pub struct BridgeConfig {
    pub listen: String,
    pub env: String,
    pub version: String,
    pub state_path: String,
    pub gateway: GatewayConfig,
    pub teams: TeamsConfig,
    /// `{aadObjectId}` / `{userId}` / `{tenantId}` → the escurel subject.
    pub principal_template: String,
    pub chat_skill: String,
    pub conversation_skill: String,
    pub poll_interval: Duration,
    pub typing_interval: Duration,
    pub reply_timeout: Duration,
    pub attachment_max_bytes: usize,
}

impl BridgeConfig {
    /// # Errors
    /// A required key is unset or a value does not parse.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_env_with(|k| std::env::var(k).ok())
    }

    /// # Errors
    /// A required key is unset or a value does not parse.
    pub fn from_env_with(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let get = |k: &str| lookup(k).filter(|v| !v.trim().is_empty());
        let req = |k: &'static str| get(k).ok_or(ConfigError::Missing(k));
        let or = |k: &str, d: &str| get(k).unwrap_or_else(|| d.to_owned());
        let dur = |k: &'static str, d: Duration| match get(k) {
            None => Ok(d),
            Some(v) => parse_duration(&v).ok_or(ConfigError::Invalid { key: k, value: v }),
        };
        let secret = |k: &'static str, file_key: &'static str| match (get(k), get(file_key)) {
            (Some(v), _) => Ok(v),
            (None, Some(path)) => std::fs::read_to_string(&path)
                .map(|s| s.trim().to_owned())
                .map_err(|source| ConfigError::Read {
                    key: file_key,
                    source,
                }),
            (None, None) => Err(ConfigError::Missing(k)),
        };
        let max_bytes = match get("ESCUREL_BRIDGE_ATTACHMENT_MAX_BYTES") {
            None => 10 * 1024 * 1024,
            Some(v) => v.parse().map_err(|_| ConfigError::Invalid {
                key: "ESCUREL_BRIDGE_ATTACHMENT_MAX_BYTES",
                value: v,
            })?,
        };
        Ok(Self {
            listen: or("ESCUREL_BRIDGE_LISTEN", "0.0.0.0:3978"),
            env: or("ESCUREL_ENV", "dev"),
            version: or("ESCUREL_VERSION", env!("CARGO_PKG_VERSION")),
            state_path: or(
                "ESCUREL_BRIDGE_STATE_PATH",
                "/data/escurel-chat-bridge.duckdb",
            ),
            gateway: GatewayConfig {
                url: req("ESCUREL_BRIDGE_GATEWAY_URL")?,
                tenant: req("ESCUREL_BRIDGE_TENANT")?,
                issuer: req("ESCUREL_BRIDGE_AUTH_ISSUER")?,
                audience: or("ESCUREL_BRIDGE_AUTH_AUDIENCE", "escurel"),
                kid: get("ESCUREL_BRIDGE_AUTH_KID"),
                signing_key: secret(
                    "ESCUREL_BRIDGE_AUTH_SIGNING_KEY",
                    "ESCUREL_BRIDGE_AUTH_SIGNING_KEY_FILE",
                )?,
                token_ttl_secs: 900,
            },
            teams: TeamsConfig {
                app_id: req("ESCUREL_BRIDGE_TEAMS_APP_ID")?,
                app_password: secret(
                    "ESCUREL_BRIDGE_TEAMS_APP_PASSWORD",
                    "ESCUREL_BRIDGE_TEAMS_APP_PASSWORD_FILE",
                )?,
                openid_metadata_url: or(
                    "ESCUREL_BRIDGE_TEAMS_OPENID_METADATA_URL",
                    BOT_FRAMEWORK_OPENID_METADATA,
                ),
                issuer: or("ESCUREL_BRIDGE_TEAMS_ISSUER", BOT_FRAMEWORK_ISSUER),
                token_url: or("ESCUREL_BRIDGE_TEAMS_TOKEN_URL", BOT_FRAMEWORK_TOKEN_URL),
                token_scope: or("ESCUREL_BRIDGE_TEAMS_TOKEN_SCOPE", BOT_FRAMEWORK_SCOPE),
            },
            principal_template: or("ESCUREL_BRIDGE_PRINCIPAL_TEMPLATE", "{aadObjectId}"),
            chat_skill: or("ESCUREL_BRIDGE_CHAT_SKILL", "chat"),
            conversation_skill: or("ESCUREL_BRIDGE_CONVERSATION_SKILL", "conversation"),
            poll_interval: dur("ESCUREL_BRIDGE_POLL_INTERVAL", Duration::from_secs(1))?,
            typing_interval: dur("ESCUREL_BRIDGE_TYPING_INTERVAL", Duration::from_secs(3))?,
            reply_timeout: dur("ESCUREL_BRIDGE_REPLY_TIMEOUT", Duration::from_secs(600))?,
            attachment_max_bytes: max_bytes,
        })
    }
}

/// `250ms`, `3s`, `10m`, or bare seconds.
fn parse_duration(raw: &str) -> Option<Duration> {
    let s = raw.trim();
    if let Some(n) = s.strip_suffix("ms") {
        return n.trim().parse().ok().map(Duration::from_millis);
    }
    if let Some(n) = s.strip_suffix('s') {
        return n.trim().parse().ok().map(Duration::from_secs);
    }
    if let Some(n) = s.strip_suffix('m') {
        return n
            .trim()
            .parse::<u64>()
            .ok()
            .map(|m| Duration::from_secs(m * 60));
    }
    s.parse().ok().map(Duration::from_secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal(k: &str) -> Option<String> {
        match k {
            "ESCUREL_BRIDGE_GATEWAY_URL" => Some("http://gw".into()),
            "ESCUREL_BRIDGE_TENANT" => Some("acme".into()),
            "ESCUREL_BRIDGE_AUTH_ISSUER" => Some("https://issuer".into()),
            "ESCUREL_BRIDGE_AUTH_SIGNING_KEY" => Some("pem".into()),
            "ESCUREL_BRIDGE_TEAMS_APP_ID" => Some("app".into()),
            "ESCUREL_BRIDGE_TEAMS_APP_PASSWORD" => Some("pw".into()),
            _ => None,
        }
    }

    #[test]
    fn defaults_point_at_microsofts_public_endpoints() {
        let c = BridgeConfig::from_env_with(minimal).unwrap();
        assert_eq!(c.teams.openid_metadata_url, BOT_FRAMEWORK_OPENID_METADATA);
        assert_eq!(c.teams.issuer, BOT_FRAMEWORK_ISSUER);
        assert_eq!(c.listen, "0.0.0.0:3978");
        assert_eq!(c.typing_interval, Duration::from_secs(3));
    }

    #[test]
    fn a_missing_app_id_is_named() {
        let err = BridgeConfig::from_env_with(|k| {
            (k != "ESCUREL_BRIDGE_TEAMS_APP_ID")
                .then(|| minimal(k))
                .flatten()
        })
        .unwrap_err();
        assert!(
            err.to_string().contains("ESCUREL_BRIDGE_TEAMS_APP_ID"),
            "{err}"
        );
    }

    #[test]
    fn durations_parse() {
        assert_eq!(parse_duration("250ms"), Some(Duration::from_millis(250)));
        assert_eq!(parse_duration("2m"), Some(Duration::from_secs(120)));
        assert_eq!(parse_duration("7"), Some(Duration::from_secs(7)));
        assert_eq!(parse_duration("soon"), None);
    }
}
