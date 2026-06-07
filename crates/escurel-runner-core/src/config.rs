//! Runtime configuration for the escurel agent runner.
//!
//! Config follows the 12-factor contract (CLAUDE.md principle 3): TOML
//! defaults are overridden by `ESCUREL_RUNNER_*` environment variables.
//! For this skeleton issue only the environment surface exists; the
//! values feed the HTTP listener, the (future) gateway client, and the
//! telemetry/log contract.

use std::net::SocketAddr;

/// Default address the runner's own HTTP server binds to
/// (`/healthz`, `/version`, and the future `POST /trigger`).
pub const DEFAULT_LISTEN: &str = "0.0.0.0:8088";

/// Default base URL of the escurel gateway exposing `/mcp`.
pub const DEFAULT_GATEWAY_URL: &str = "http://127.0.0.1:8080";

/// Default deployment environment, stamped on every log record.
pub const DEFAULT_ENV: &str = "dev";

/// Errors raised while loading [`RunnerConfig`] from the environment.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// `ESCUREL_RUNNER_LISTEN` was set but is not a valid socket address.
    #[error("invalid ESCUREL_RUNNER_LISTEN {value:?}: {source}")]
    InvalidListen {
        /// The offending value.
        value: String,
        /// The underlying parse error.
        #[source]
        source: std::net::AddrParseError,
    },
}

/// Runtime configuration for the runner process.
///
/// Construct it with [`RunnerConfig::from_env`]; every field has a sane
/// default so an unconfigured `escurel-runner` starts in dev mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerConfig {
    /// Address the runner's own HTTP server binds to.
    /// Source: `ESCUREL_RUNNER_LISTEN` (default [`DEFAULT_LISTEN`]).
    pub listen: SocketAddr,
    /// Base URL of the escurel gateway exposing `/mcp`. Unused this
    /// issue, wired for later work-items.
    /// Source: `ESCUREL_RUNNER_GATEWAY_URL` (default
    /// [`DEFAULT_GATEWAY_URL`]).
    pub gateway_url: String,
    /// Deployment environment for the log/telemetry contract.
    /// Source: `ESCUREL_RUNNER_ENV` (default [`DEFAULT_ENV`]).
    pub env: String,
    /// Build version string, taken from `CARGO_PKG_VERSION` at compile
    /// time. Stamped on every log record per the substrate contract.
    pub version: String,
    /// Optional shared secret the webhook listener requires on inbound
    /// `POST /trigger` requests. When set, the request must carry a valid
    /// HMAC-SHA256 signature of the raw body in the header
    /// `X-Escurel-Webhook-Signature: sha256=<hex>` (#147); an unsigned or
    /// mismatched request is rejected `401`. `None` leaves the endpoint
    /// open (dev mode).
    /// Source: `ESCUREL_WEBHOOK_SECRET` (unset → `None`).
    pub webhook_secret: Option<String>,
}

impl RunnerConfig {
    /// Load configuration from `ESCUREL_RUNNER_*` environment variables,
    /// falling back to the documented defaults.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_env_with(|key| std::env::var(key).ok())
    }

    /// Load configuration using an explicit lookup closure. Exposed so
    /// unit tests can exercise parsing/defaults without mutating the
    /// process-global environment (which would race other tests).
    pub fn from_env_with<F>(lookup: F) -> Result<Self, ConfigError>
    where
        F: Fn(&str) -> Option<String>,
    {
        let listen_raw =
            lookup("ESCUREL_RUNNER_LISTEN").unwrap_or_else(|| DEFAULT_LISTEN.to_owned());
        let listen =
            listen_raw
                .parse::<SocketAddr>()
                .map_err(|source| ConfigError::InvalidListen {
                    value: listen_raw,
                    source,
                })?;

        let gateway_url =
            lookup("ESCUREL_RUNNER_GATEWAY_URL").unwrap_or_else(|| DEFAULT_GATEWAY_URL.to_owned());
        let env = lookup("ESCUREL_RUNNER_ENV").unwrap_or_else(|| DEFAULT_ENV.to_owned());
        let webhook_secret = lookup("ESCUREL_WEBHOOK_SECRET").filter(|s| !s.is_empty());

        Ok(Self {
            listen,
            gateway_url,
            env,
            version: env!("CARGO_PKG_VERSION").to_owned(),
            webhook_secret,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_apply_when_env_is_empty() {
        let cfg = RunnerConfig::from_env_with(|_| None).expect("defaults must parse");
        assert_eq!(cfg.listen, DEFAULT_LISTEN.parse::<SocketAddr>().unwrap());
        assert_eq!(cfg.gateway_url, DEFAULT_GATEWAY_URL);
        assert_eq!(cfg.env, DEFAULT_ENV);
        assert_eq!(cfg.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(cfg.webhook_secret, None);
    }

    #[test]
    fn webhook_secret_loads_when_set_and_ignores_empty() {
        let set = RunnerConfig::from_env_with(|key| {
            (key == "ESCUREL_WEBHOOK_SECRET").then(|| "s3cret".to_owned())
        })
        .expect("a webhook secret must parse");
        assert_eq!(set.webhook_secret, Some("s3cret".to_owned()));

        let empty =
            RunnerConfig::from_env_with(|key| (key == "ESCUREL_WEBHOOK_SECRET").then(String::new))
                .expect("an empty webhook secret must parse");
        assert_eq!(
            empty.webhook_secret, None,
            "empty secret is treated as unset"
        );
    }

    #[test]
    fn env_vars_override_defaults() {
        let cfg = RunnerConfig::from_env_with(|key| match key {
            "ESCUREL_RUNNER_LISTEN" => Some("127.0.0.1:9099".to_owned()),
            "ESCUREL_RUNNER_GATEWAY_URL" => Some("https://gw.example:8443".to_owned()),
            "ESCUREL_RUNNER_ENV" => Some("prod".to_owned()),
            _ => None,
        })
        .expect("explicit values must parse");
        assert_eq!(cfg.listen, "127.0.0.1:9099".parse::<SocketAddr>().unwrap());
        assert_eq!(cfg.gateway_url, "https://gw.example:8443");
        assert_eq!(cfg.env, "prod");
    }

    #[test]
    fn invalid_listen_is_an_error() {
        let err = RunnerConfig::from_env_with(|key| {
            (key == "ESCUREL_RUNNER_LISTEN").then(|| "not-an-addr".to_owned())
        })
        .expect_err("a bad listen address must fail");
        assert!(matches!(err, ConfigError::InvalidListen { .. }));
    }
}
