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
    #[error("{key}: {reason}")]
    Refused { key: &'static str, reason: String },
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
    /// The Entra tenants whose people the bridge answers
    /// (`ESCUREL_BRIDGE_TEAMS_ALLOWED_TENANTS`). Required: a multi-tenant bot
    /// app would otherwise give anyone in any tenant an escurel identity.
    pub allowed_tenants: Vec<String>,
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
    /// Validated at boot by [`validate_principal_template`].
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
                allowed_tenants: list(&req("ESCUREL_BRIDGE_TEAMS_ALLOWED_TENANTS")?),
            },
            principal_template: {
                let t = req("ESCUREL_BRIDGE_PRINCIPAL_TEMPLATE")?;
                validate_principal_template(&t).map_err(|reason| ConfigError::Refused {
                    key: "ESCUREL_BRIDGE_PRINCIPAL_TEMPLATE",
                    reason,
                })?;
                t
            },
            chat_skill: or("ESCUREL_BRIDGE_CHAT_SKILL", "chat"),
            conversation_skill: or("ESCUREL_BRIDGE_CONVERSATION_SKILL", "conversation"),
            poll_interval: dur("ESCUREL_BRIDGE_POLL_INTERVAL", Duration::from_secs(1))?,
            typing_interval: dur("ESCUREL_BRIDGE_TYPING_INTERVAL", Duration::from_secs(3))?,
            reply_timeout: dur("ESCUREL_BRIDGE_REPLY_TIMEOUT", Duration::from_secs(600))?,
            attachment_max_bytes: max_bytes,
        })
    }
}

/// The placeholders a principal template may name.
pub const PRINCIPAL_PLACEHOLDERS: [&str; 3] = ["{aadObjectId}", "{userId}", "{tenantId}"];

/// Refuse a principal template that could map two people to one subject:
/// every `{…}` must be a known placeholder, and at least one of them must
/// name the person (`{aadObjectId}` or `{userId}`) — a constant, a misspelt
/// placeholder (left in as a literal) or a tenant-only template would give
/// everyone the same identity, and with it each other's private pages.
///
/// # Errors
/// Why the template is refused.
pub fn validate_principal_template(template: &str) -> Result<(), String> {
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        let after = &rest[open..];
        let close = after
            .find('}')
            .ok_or_else(|| format!("unclosed `{{` in `{template}`"))?;
        let placeholder = &after[..=close];
        if !PRINCIPAL_PLACEHOLDERS.contains(&placeholder) {
            return Err(format!(
                "unknown placeholder `{placeholder}` (known: {})",
                PRINCIPAL_PLACEHOLDERS.join(", ")
            ));
        }
        rest = &after[close + 1..];
    }
    if rest.contains('}') {
        return Err(format!("stray `}}` in `{template}`"));
    }
    if !(template.contains("{aadObjectId}") || template.contains("{userId}")) {
        return Err(
            "must name the person (`{aadObjectId}` or `{userId}`): otherwise every sender \
             maps to one subject"
                .to_owned(),
        );
    }
    Ok(())
}

/// A comma-separated list, trimmed, empties dropped.
fn list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
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
            "ESCUREL_BRIDGE_TEAMS_ALLOWED_TENANTS" => Some("t-1, t-2".into()),
            "ESCUREL_BRIDGE_PRINCIPAL_TEMPLATE" => Some("user:{aadObjectId}".into()),
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
    fn the_allowed_tenants_are_required_and_listed() {
        let c = BridgeConfig::from_env_with(minimal).unwrap();
        assert_eq!(c.teams.allowed_tenants, ["t-1", "t-2"]);
        let err = BridgeConfig::from_env_with(|k| {
            (k != "ESCUREL_BRIDGE_TEAMS_ALLOWED_TENANTS")
                .then(|| minimal(k))
                .flatten()
        })
        .unwrap_err();
        assert!(err.to_string().contains("ALLOWED_TENANTS"), "{err}");
    }

    #[test]
    fn the_principal_template_is_required_and_has_no_default() {
        let err = BridgeConfig::from_env_with(|k| {
            (k != "ESCUREL_BRIDGE_PRINCIPAL_TEMPLATE")
                .then(|| minimal(k))
                .flatten()
        })
        .unwrap_err();
        assert!(err.to_string().contains("PRINCIPAL_TEMPLATE"), "{err}");
    }

    #[test]
    fn a_template_that_maps_everyone_to_one_subject_is_refused() {
        for (template, why) in [
            ("alice", "must name the person"),
            ("user:{aadObjectID}", "unknown placeholder `{aadObjectID}`"),
            ("{tenantId}", "must name the person"),
            ("team:{tenantId}:all", "must name the person"),
            ("user:{aadObjectId", "unclosed"),
            ("user:aadObjectId}", "stray"),
        ] {
            let err = validate_principal_template(template).unwrap_err();
            assert!(err.contains(why), "{template}: {err}");
        }
        for ok in [
            "user:{aadObjectId}",
            "{aadObjectId}",
            "{tenantId}/{userId}",
            "teams:{tenantId}:{aadObjectId}",
        ] {
            validate_principal_template(ok).unwrap_or_else(|e| panic!("{ok}: {e}"));
        }
        let err = BridgeConfig::from_env_with(|k| match k {
            "ESCUREL_BRIDGE_PRINCIPAL_TEMPLATE" => Some("everyone".into()),
            _ => minimal(k),
        })
        .unwrap_err();
        assert!(err.to_string().contains("must name the person"), "{err}");
    }

    #[test]
    fn durations_parse() {
        assert_eq!(parse_duration("250ms"), Some(Duration::from_millis(250)));
        assert_eq!(parse_duration("2m"), Some(Duration::from_secs(120)));
        assert_eq!(parse_duration("7"), Some(Duration::from_secs(7)));
        assert_eq!(parse_duration("soon"), None);
    }
}
