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
/// Where Teams puts files people share (SharePoint / OneDrive) and the
/// media it hosts itself.
pub const DEFAULT_ATTACHMENT_HOSTS: &str = "*.sharepoint.com,*.asm.skype.com";

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
    pub auth: GatewayAuth,
    /// How long an asker's token lives; the bridge re-asks after half of it.
    pub token_ttl_secs: u64,
}

/// How the bridge gets each turn's token.
#[derive(Clone)]
pub enum GatewayAuth {
    /// The gateway mints it (`mint_user_token`, #830): the bridge holds only
    /// its own `escurel:bridge` credential (`ESCUREL_BRIDGE_GATEWAY_TOKEN[_FILE]`)
    /// and no signing key. The gateway decides the subject, the groups and,
    /// in a group chat or channel, the group principal.
    Service { credential: ServiceCredential },
    /// DEPRECATED (removed in the next release): the bridge signs the asker's
    /// token with its OWN key, under an issuer the gateway names in
    /// `ESCUREL_AUTH_BRIDGE_ISSUER` pinned to `kid` (`ESCUREL_AUTH_BRIDGE_KID`).
    /// Such a token carries no groups, and only 1:1 chats are answered.
    SelfSigned {
        issuer: String,
        audience: String,
        kid: String,
        /// Never the gateway's main signing key.
        signing_key: String,
        /// `{aadObjectId}` / `{userId}` / `{tenantId}` → the escurel subject.
        principal_template: String,
    },
}

/// The bridge's own `escurel:bridge` credential: a value, or a file a
/// sidecar keeps fresh (`ESCUREL_BRIDGE_GATEWAY_TOKEN_FILE`) — re-read when it
/// changes and whenever the gateway rejects the token in hand, so a rotation
/// needs no restart.
#[derive(Clone)]
pub enum ServiceCredential {
    Static(String),
    File(std::sync::Arc<escurel_auth::TokenFile>),
}

impl ServiceCredential {
    /// The credential to present now.
    #[must_use]
    pub fn current(&self) -> String {
        match self {
            Self::Static(token) => token.clone(),
            Self::File(file) => file.current(),
        }
    }

    /// The gateway rejected the credential: re-read the file. `true` when
    /// there is a different one to try.
    #[must_use]
    pub fn reload(&self) -> bool {
        match self {
            Self::Static(_) => false,
            Self::File(file) => file.reload(),
        }
    }
}

impl std::fmt::Debug for GatewayAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Service { .. } => f.write_str("Service"),
            Self::SelfSigned { issuer, kid, .. } => f
                .debug_struct("SelfSigned")
                .field("issuer", issuer)
                .field("kid", kid)
                .finish_non_exhaustive(),
        }
    }
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
    /// Hosts other than the connector an attachment may be downloaded from
    /// (`ESCUREL_BRIDGE_TEAMS_ATTACHMENT_HOSTS`; `*.suffix` wildcards).
    pub attachment_hosts: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct BridgeConfig {
    pub listen: String,
    pub env: String,
    pub version: String,
    pub state_path: String,
    pub gateway: GatewayConfig,
    pub teams: TeamsConfig,
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
                auth: match service_credential(&get)? {
                    Some(credential) => GatewayAuth::Service { credential },
                    None => GatewayAuth::SelfSigned {
                        issuer: req("ESCUREL_BRIDGE_AUTH_ISSUER")
                            .map_err(|_| ConfigError::Missing("ESCUREL_BRIDGE_GATEWAY_TOKEN"))?,
                        audience: or("ESCUREL_BRIDGE_AUTH_AUDIENCE", "escurel"),
                        kid: req("ESCUREL_BRIDGE_AUTH_KID")?,
                        signing_key: secret(
                            "ESCUREL_BRIDGE_AUTH_SIGNING_KEY",
                            "ESCUREL_BRIDGE_AUTH_SIGNING_KEY_FILE",
                        )?,
                        principal_template: {
                            let t = req("ESCUREL_BRIDGE_PRINCIPAL_TEMPLATE")?;
                            validate_principal_template(&t).map_err(|reason| {
                                ConfigError::Refused {
                                    key: "ESCUREL_BRIDGE_PRINCIPAL_TEMPLATE",
                                    reason,
                                }
                            })?;
                            t
                        },
                    },
                },
                // At most a minute under the gateway's bound, so a bridge clock
                // that runs a little ahead never mints a token "too long-lived".
                token_ttl_secs: {
                    let max = Duration::from_secs(escurel_types::bridge::MAX_TTL_SECS - 60);
                    let ttl = dur("ESCUREL_BRIDGE_TOKEN_TTL", max)?;
                    if ttl > max || ttl < Duration::from_secs(2) {
                        return Err(ConfigError::Refused {
                            key: "ESCUREL_BRIDGE_TOKEN_TTL",
                            reason: format!("between 2s and {}s", max.as_secs()),
                        });
                    }
                    ttl.as_secs()
                },
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
                attachment_hosts: list(&or(
                    "ESCUREL_BRIDGE_TEAMS_ATTACHMENT_HOSTS",
                    DEFAULT_ATTACHMENT_HOSTS,
                )),
            },
            chat_skill: or(
                "ESCUREL_BRIDGE_CHAT_SKILL",
                escurel_types::bridge::DEFAULT_CHAT_SKILL,
            ),
            conversation_skill: or("ESCUREL_BRIDGE_CONVERSATION_SKILL", "conversation"),
            poll_interval: dur("ESCUREL_BRIDGE_POLL_INTERVAL", Duration::from_secs(1))?,
            typing_interval: dur("ESCUREL_BRIDGE_TYPING_INTERVAL", Duration::from_secs(3))?,
            reply_timeout: dur("ESCUREL_BRIDGE_REPLY_TIMEOUT", Duration::from_secs(600))?,
            attachment_max_bytes: max_bytes,
        })
    }
}

pub use escurel_types::bridge::validate_principal_template;

/// `ESCUREL_BRIDGE_GATEWAY_TOKEN`, else `…_FILE` (read now, so a missing file
/// refuses to start); `None` when neither is set.
fn service_credential(
    get: &impl Fn(&str) -> Option<String>,
) -> Result<Option<ServiceCredential>, ConfigError> {
    const KEY: &str = "ESCUREL_BRIDGE_GATEWAY_TOKEN_FILE";
    if let Some(token) = get("ESCUREL_BRIDGE_GATEWAY_TOKEN") {
        return Ok(Some(ServiceCredential::Static(token)));
    }
    let Some(path) = get(KEY) else {
        return Ok(None);
    };
    escurel_auth::TokenFile::open(KEY, path)
        .map(|f| Some(ServiceCredential::File(std::sync::Arc::new(f))))
        .map_err(|e| ConfigError::Refused {
            key: KEY,
            reason: format!("{}: {}", e.path, e.reason),
        })
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
            "ESCUREL_BRIDGE_AUTH_KID" => Some("bridge-key".into()),
            "ESCUREL_BRIDGE_TEAMS_APP_ID" => Some("app".into()),
            "ESCUREL_BRIDGE_TEAMS_APP_PASSWORD" => Some("pw".into()),
            "ESCUREL_BRIDGE_TEAMS_ALLOWED_TENANTS" => Some("t-1, t-2".into()),
            "ESCUREL_BRIDGE_PRINCIPAL_TEMPLATE" => Some("user:{aadObjectId}".into()),
            _ => None,
        }
    }

    #[test]
    fn a_gateway_token_selects_the_gateway_minted_path_and_needs_no_key() {
        let c = BridgeConfig::from_env_with(|k| match k {
            "ESCUREL_BRIDGE_GATEWAY_TOKEN" => Some("svc-token".into()),
            "ESCUREL_BRIDGE_AUTH_ISSUER"
            | "ESCUREL_BRIDGE_AUTH_SIGNING_KEY"
            | "ESCUREL_BRIDGE_AUTH_KID"
            | "ESCUREL_BRIDGE_PRINCIPAL_TEMPLATE" => None,
            _ => minimal(k),
        })
        .unwrap();
        assert!(
            matches!(c.gateway.auth, GatewayAuth::Service { ref credential }
                if credential.current() == "svc-token")
        );
        let err = BridgeConfig::from_env_with(|k| match k {
            "ESCUREL_BRIDGE_AUTH_ISSUER" => None,
            _ => minimal(k),
        })
        .unwrap_err();
        assert!(
            err.to_string().contains("ESCUREL_BRIDGE_GATEWAY_TOKEN"),
            "{err}"
        );
    }

    #[test]
    fn defaults_point_at_microsofts_public_endpoints() {
        let c = BridgeConfig::from_env_with(minimal).unwrap();
        assert_eq!(c.teams.openid_metadata_url, BOT_FRAMEWORK_OPENID_METADATA);
        assert_eq!(c.teams.issuer, BOT_FRAMEWORK_ISSUER);
        assert_eq!(c.listen, "0.0.0.0:3978");
        assert_eq!(c.typing_interval, Duration::from_secs(3));
        assert_eq!(
            c.gateway.token_ttl_secs,
            escurel_types::bridge::MAX_TTL_SECS - 60,
            "minted under the gateway's bound (crew C-F3)"
        );
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
    fn the_self_signed_path_needs_its_principal_template() {
        let err = BridgeConfig::from_env_with(|k| {
            (k != "ESCUREL_BRIDGE_PRINCIPAL_TEMPLATE")
                .then(|| minimal(k))
                .flatten()
        })
        .unwrap_err();
        assert!(err.to_string().contains("PRINCIPAL_TEMPLATE"), "{err}");
    }

    #[test]
    fn a_template_that_maps_everyone_to_one_subject_is_refused_at_boot() {
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

    /// The bridge credential from a file is read at boot and re-read on demand.
    #[test]
    fn the_gateway_token_file_is_read_now_and_again_after_a_rotation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bridge-token");
        std::fs::write(&path, "first\n").unwrap();
        let p = path.display().to_string();
        let cfg = |file: String| {
            BridgeConfig::from_env_with(move |k| match k {
                "ESCUREL_BRIDGE_GATEWAY_TOKEN_FILE" => Some(file.clone()),
                "ESCUREL_BRIDGE_AUTH_ISSUER" => None,
                _ => minimal(k),
            })
        };
        let GatewayAuth::Service { credential } = cfg(p.clone()).unwrap().gateway.auth else {
            panic!("the gateway-minted path");
        };
        assert_eq!(credential.current(), "first");
        std::fs::write(&path, "second").unwrap();
        assert!(credential.reload());
        assert_eq!(credential.current(), "second");
        let err = cfg(dir.path().join("missing").display().to_string()).unwrap_err();
        assert!(
            err.to_string()
                .contains("ESCUREL_BRIDGE_GATEWAY_TOKEN_FILE"),
            "{err}"
        );
    }
}
