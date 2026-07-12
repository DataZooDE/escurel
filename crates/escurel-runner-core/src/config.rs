//! Runtime configuration for the escurel agent runner.
//!
//! Config follows the 12-factor contract (CLAUDE.md principle 3): TOML
//! defaults are overridden by `ESCUREL_RUNNER_*` environment variables.
//! For this skeleton issue only the environment surface exists; the
//! values feed the HTTP listener, the (future) gateway client, and the
//! telemetry/log contract.

use std::net::SocketAddr;
use std::time::Duration;

/// Default address the runner's own HTTP server binds to
/// (`/healthz`, `/version`, and the future `POST /trigger`).
pub const DEFAULT_LISTEN: &str = "0.0.0.0:8088";

/// Default base URL of the escurel gateway exposing `/mcp`.
pub const DEFAULT_GATEWAY_URL: &str = "http://127.0.0.1:8080";

/// Default deployment environment, stamped on every log record.
pub const DEFAULT_ENV: &str = "dev";

/// Default bound on the dispatch queue (channel capacity). The queue is
/// bounded so a backlog applies backpressure rather than growing without
/// limit (see [`crate::DispatchQueue`]).
pub const DEFAULT_QUEUE_CAP: usize = 1024;

/// Default bound on the dedup seen-set. Sized larger than the queue so the
/// webhook/poll overlap window is comfortably covered even when the queue
/// drains slowly.
pub const DEFAULT_SEEN_CAP: usize = 4096;

/// Default inbox-poll interval. The poller is the self-healing fallback for
/// missed webhooks, so a coarse cadence is fine.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(30);

/// Default harness adapter the runner dispatches each packaged trigger to.
/// `echo` is the deterministic real harness (#151); `claude` / `codex` /
/// `adk` arrive in later work-items and are selected by the same env var.
pub const DEFAULT_HARNESS: &str = "echo";

/// Default `claude` binary the Claude Code adapter (#152) spawns. A bare
/// name resolves on `PATH`; a deterministic test overrides it to a stub.
pub const DEFAULT_CLAUDE_BIN: &str = "claude";

/// Default `codex` binary the Codex adapter (#153) spawns. A bare name
/// resolves on `PATH`; a deterministic test overrides it to a stub.
pub const DEFAULT_CODEX_BIN: &str = "codex";

/// Default adk-rust runner binary the Google ADK adapter (#154) spawns.
/// There is no sensible default on `PATH` (the heavy adk-rust runtime lives
/// in an external binary built from the `datazoo-agent-template`), so a
/// deployment MUST point `ESCUREL_RUNNER_ADK_BIN` at its built runner; the
/// deterministic test overrides it to a real scripted runner.
pub const DEFAULT_ADK_BIN: &str = "datazoo-agent-adk-runner";

/// Default cap on how many times the reconciler (#155) attempts a single
/// run before recording it `failed`. The first try plus retries: e.g. `3`
/// means one initial attempt and up to two retries. Sized small — a
/// transient `/mcp`/harness blip clears in a couple of attempts; a run that
/// burns the whole cap is almost certainly permanently broken.
pub const DEFAULT_MAX_ATTEMPTS: u32 = 3;

/// Default base backoff between reconciler retry attempts. Backoff grows
/// per attempt (the reconciler applies a simple exponential), so this is the
/// delay before the first retry.
pub const DEFAULT_RETRY_BACKOFF: Duration = Duration::from_millis(500);

/// Default cascade depth cap (#157). When a trigger's lineage `depth`
/// exceeds this, the dispatch gate dead-letters the run `depth_exceeded`
/// rather than admitting it — the hard backstop that bounds any cascade,
/// even if cycle detection misses. Sized generous enough for legitimate
/// multi-stage chains but far below a runaway.
pub const DEFAULT_MAX_DEPTH: u32 = 8;

/// Default per-root run budget (#157). The dispatch gate counts runs sharing
/// a `root_event_id`; once the budget is spent it dead-letters further runs
/// `budget_exceeded`. A second backstop against a cascade that fans out
/// without deepening (so the depth cap alone wouldn't catch it).
pub const DEFAULT_MAX_RUNS_PER_ROOT: u64 = 64;

/// Default per-tenant runs/min quota (#158). The dispatch gate counts
/// admitted runs per tenant in a fixed one-minute window; over-budget triggers
/// are *throttled* (held, the event stays in the inbox for the poller to
/// backstop), never dead-lettered. Sized generous for legitimate bursts.
pub const DEFAULT_TENANT_RUNS_PER_MIN: u64 = 120;

/// Default per-tenant max concurrent in-flight runs (#158). A second
/// throttle gate: while this many runs execute for a tenant, a new trigger is
/// held back.
pub const DEFAULT_TENANT_MAX_CONCURRENT: u64 = 8;

/// Default global cap on concurrent harness subprocesses across all tenants
/// (#158). Subprocesses are the heaviest resource, so this bounds total spawn
/// concurrency with a process-wide semaphore regardless of per-tenant limits.
pub const DEFAULT_MAX_HARNESS_PROCS: usize = 16;

/// Default SIGTERM drain timeout (#158): on shutdown the runner stops
/// accepting new triggers and lets in-flight runs finish, bounded by this
/// deadline before it exits anyway (a still-pending run is recovered on the
/// next start by read-back).
pub const DEFAULT_DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

/// Default path of the runner-local durable run ledger (its own SQLite
/// file, *never* the tenant store). Relative to the process CWD so a dev
/// run drops it in place; deployments set [`crate::RunnerConfig::ledger_path`]
/// to a host-volume path under `/data` (see the substrate contract).
pub const DEFAULT_LEDGER_PATH: &str = "./escurel-runner-ledger.sqlite";

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
    /// A `usize`-valued env var (`ESCUREL_RUNNER_QUEUE_CAP` /
    /// `ESCUREL_RUNNER_SEEN_CAP`) was set but is not a valid integer.
    #[error("invalid {key} {value:?}: expected a non-negative integer")]
    InvalidUsize {
        /// The offending env var name.
        key: String,
        /// The offending value.
        value: String,
    },
    /// `ESCUREL_RUNNER_POLL_INTERVAL` was set but is not a valid duration
    /// (`30s`, `1500ms`, `2m`, or a bare integer of seconds).
    #[error("invalid ESCUREL_RUNNER_POLL_INTERVAL {value:?}: expected e.g. 30s, 1500ms, 2m")]
    InvalidPollInterval {
        /// The offending value.
        value: String,
    },
    /// `ESCUREL_RUNNER_MAX_ATTEMPTS` was set but is not a positive integer
    /// (the cap must be at least `1` — one attempt is always made).
    #[error("invalid ESCUREL_RUNNER_MAX_ATTEMPTS {value:?}: expected a positive integer")]
    InvalidMaxAttempts {
        /// The offending value.
        value: String,
    },
    /// `ESCUREL_RUNNER_RETRY_BACKOFF` was set but is not a valid duration.
    #[error("invalid ESCUREL_RUNNER_RETRY_BACKOFF {value:?}: expected e.g. 500ms, 2s")]
    InvalidRetryBackoff {
        /// The offending value.
        value: String,
    },
    /// `ESCUREL_RUNNER_MAX_DEPTH` was set but is not a valid integer.
    #[error("invalid ESCUREL_RUNNER_MAX_DEPTH {value:?}: expected a non-negative integer")]
    InvalidMaxDepth {
        /// The offending value.
        value: String,
    },
    /// `ESCUREL_RUNNER_MAX_RUNS_PER_ROOT` was set but is not a positive
    /// integer (the budget must be at least `1`).
    #[error("invalid ESCUREL_RUNNER_MAX_RUNS_PER_ROOT {value:?}: expected a positive integer")]
    InvalidMaxRunsPerRoot {
        /// The offending value.
        value: String,
    },
    /// A positive-`u64` quota env var was set but is not a positive integer.
    #[error("invalid {key} {value:?}: expected a positive integer")]
    InvalidQuotaU64 {
        /// The offending env var name.
        key: String,
        /// The offending value.
        value: String,
    },
    /// `ESCUREL_RUNNER_MAX_HARNESS_PROCS` was set but is not a positive integer.
    #[error("invalid ESCUREL_RUNNER_MAX_HARNESS_PROCS {value:?}: expected a positive integer")]
    InvalidMaxHarnessProcs {
        /// The offending value.
        value: String,
    },
    /// `ESCUREL_RUNNER_DRAIN_TIMEOUT` was set but is not a valid duration.
    #[error("invalid ESCUREL_RUNNER_DRAIN_TIMEOUT {value:?}: expected e.g. 30s, 1500ms")]
    InvalidDrainTimeout {
        /// The offending value.
        value: String,
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
    /// Tenant the runner polls and stamps onto every normalised
    /// [`crate::Trigger`]. The gateway is single-tenant per indexer, so
    /// this is the tenant whose inbox the poller drains.
    /// Source: `ESCUREL_RUNNER_TENANT` (unset → `None`; the poller is
    /// disabled when either this or [`Self::token`] is absent).
    pub tenant: Option<String>,
    /// Tenant-scoped bearer the runner presents to the gateway's `/mcp`
    /// when polling `list_inbox`. Held opaque; never logged.
    /// Source: `ESCUREL_RUNNER_TOKEN` (unset → `None`; the poller is
    /// disabled when either this or [`Self::tenant`] is absent).
    pub token: Option<String>,
    /// Channel capacity (bound) of the dispatch queue.
    /// Source: `ESCUREL_RUNNER_QUEUE_CAP` (default [`DEFAULT_QUEUE_CAP`]).
    pub queue_cap: usize,
    /// Bound on the dedup seen-set.
    /// Source: `ESCUREL_RUNNER_SEEN_CAP` (default [`DEFAULT_SEEN_CAP`]).
    pub seen_cap: usize,
    /// Interval between inbox polls. Accepts a humantime-style duration
    /// (`30s`, `1500ms`, `2m`); tests use a small value (`1s`) to keep the
    /// loop fast.
    /// Source: `ESCUREL_RUNNER_POLL_INTERVAL` (default
    /// [`DEFAULT_POLL_INTERVAL`]).
    pub poll_interval: Duration,
    /// How often the runner synthesizes a `lint` invocation (compile-first-wiki
    /// G2): each tick it `capture_event`s a `label_skill: lint` event with a
    /// deterministic per-window id, so the reactive loop drives the scheduled
    /// semantic-health pass. `None` ⇒ disabled (the default) — a markdown-only
    /// tenant never lints and behaves exactly as before.
    /// Source: `ESCUREL_RUNNER_LINT_INTERVAL` (default: unset/disabled).
    pub lint_interval: Option<Duration>,
    /// Filesystem path of the runner-local durable run ledger (its own
    /// SQLite file — the idempotency authority that survives a restart).
    /// Source: `ESCUREL_RUNNER_LEDGER_PATH` (default
    /// [`DEFAULT_LEDGER_PATH`]).
    pub ledger_path: String,
    /// Which harness adapter dispatches each packaged trigger (`echo` /
    /// `claude` / `codex` / `adk`). Data-driven selection so later
    /// work-items add adapters without touching the dispatch path.
    /// Source: `ESCUREL_RUNNER_HARNESS` (default [`DEFAULT_HARNESS`]).
    pub harness: String,
    /// Path to the `claude` binary the Claude Code adapter (#152) spawns.
    /// Source: `ESCUREL_RUNNER_CLAUDE_BIN` (default [`DEFAULT_CLAUDE_BIN`]).
    pub claude_bin: String,
    /// Optional `--model` the Claude Code adapter passes to `claude` (an
    /// alias like `opus`/`sonnet` or a full model id); `None` lets `claude`
    /// pick its configured default.
    /// Source: `ESCUREL_RUNNER_CLAUDE_MODEL` (unset → `None`).
    pub claude_model: Option<String>,
    /// Path to the `codex` binary the Codex adapter (#153) spawns.
    /// Source: `ESCUREL_RUNNER_CODEX_BIN` (default [`DEFAULT_CODEX_BIN`]).
    pub codex_bin: String,
    /// Optional `-m/--model` the Codex adapter passes to `codex` (a model
    /// id like `o3`/`gpt-5-codex`); `None` lets `codex` pick its configured
    /// default.
    /// Source: `ESCUREL_RUNNER_CODEX_MODEL` (unset → `None`).
    pub codex_model: Option<String>,
    /// Path to the adk-rust runner binary the Google ADK adapter (#154)
    /// spawns.
    /// Source: `ESCUREL_RUNNER_ADK_BIN` (default [`DEFAULT_ADK_BIN`]).
    pub adk_bin: String,
    /// Optional LLM model id the Google ADK adapter passes to the runner via
    /// `LLM_MODEL` (e.g. `gemini-3.5-flash`); `None` lets the runner pick its
    /// configured default.
    /// Source: `ESCUREL_RUNNER_ADK_MODEL` (unset → `None`).
    pub adk_model: Option<String>,
    /// Cap on reconciler attempts per run before recording `failed` (#155).
    /// Always at least `1` (one attempt is made even with retries disabled).
    /// Source: `ESCUREL_RUNNER_MAX_ATTEMPTS` (default [`DEFAULT_MAX_ATTEMPTS`]).
    pub max_attempts: u32,
    /// Base backoff before the first reconciler retry; grows per attempt.
    /// Source: `ESCUREL_RUNNER_RETRY_BACKOFF` (default
    /// [`DEFAULT_RETRY_BACKOFF`]).
    pub retry_backoff: Duration,
    /// Cascade depth cap; a trigger deeper than this is dead-lettered
    /// `depth_exceeded` at the dispatch gate (#157).
    /// Source: `ESCUREL_RUNNER_MAX_DEPTH` (default [`DEFAULT_MAX_DEPTH`]).
    pub max_depth: u32,
    /// Per-root run budget; runs sharing a `root_event_id` beyond this are
    /// dead-lettered `budget_exceeded` (#157).
    /// Source: `ESCUREL_RUNNER_MAX_RUNS_PER_ROOT` (default
    /// [`DEFAULT_MAX_RUNS_PER_ROOT`]).
    pub max_runs_per_root: u64,
    /// Per-tenant runs/min quota; over-budget triggers are throttled (#158).
    /// Source: `ESCUREL_RUNNER_TENANT_RUNS_PER_MIN` (default
    /// [`DEFAULT_TENANT_RUNS_PER_MIN`]).
    pub tenant_runs_per_min: u64,
    /// Per-tenant max concurrent in-flight runs; over-limit triggers are
    /// throttled (#158).
    /// Source: `ESCUREL_RUNNER_TENANT_MAX_CONCURRENT` (default
    /// [`DEFAULT_TENANT_MAX_CONCURRENT`]).
    pub tenant_max_concurrent: u64,
    /// Global cap on concurrent harness subprocesses across all tenants (#158).
    /// Source: `ESCUREL_RUNNER_MAX_HARNESS_PROCS` (default
    /// [`DEFAULT_MAX_HARNESS_PROCS`]).
    pub max_harness_procs: usize,
    /// SIGTERM drain timeout: how long shutdown lets in-flight runs finish
    /// before exiting anyway (#158).
    /// Source: `ESCUREL_RUNNER_DRAIN_TIMEOUT` (default
    /// [`DEFAULT_DRAIN_TIMEOUT`]).
    pub drain_timeout: Duration,
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

        let tenant = lookup("ESCUREL_RUNNER_TENANT").filter(|s| !s.is_empty());
        let token = lookup("ESCUREL_RUNNER_TOKEN").filter(|s| !s.is_empty());

        let queue_cap = parse_usize("ESCUREL_RUNNER_QUEUE_CAP", &lookup, DEFAULT_QUEUE_CAP)?;
        let seen_cap = parse_usize("ESCUREL_RUNNER_SEEN_CAP", &lookup, DEFAULT_SEEN_CAP)?;

        let poll_interval = match lookup("ESCUREL_RUNNER_POLL_INTERVAL") {
            Some(raw) if !raw.is_empty() => {
                parse_duration(&raw).ok_or(ConfigError::InvalidPollInterval { value: raw })?
            }
            _ => DEFAULT_POLL_INTERVAL,
        };

        // Opt-in lint schedule: unset ⇒ disabled. Reuses the poll-interval
        // duration grammar/error (both are "how often to tick").
        let lint_interval = match lookup("ESCUREL_RUNNER_LINT_INTERVAL") {
            Some(raw) if !raw.is_empty() => Some(
                parse_duration(&raw).ok_or(ConfigError::InvalidPollInterval { value: raw })?,
            ),
            _ => None,
        };

        let ledger_path = lookup("ESCUREL_RUNNER_LEDGER_PATH")
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_LEDGER_PATH.to_owned());

        let harness = lookup("ESCUREL_RUNNER_HARNESS")
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_HARNESS.to_owned());

        let claude_bin = lookup("ESCUREL_RUNNER_CLAUDE_BIN")
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_CLAUDE_BIN.to_owned());
        let claude_model = lookup("ESCUREL_RUNNER_CLAUDE_MODEL").filter(|s| !s.is_empty());

        let codex_bin = lookup("ESCUREL_RUNNER_CODEX_BIN")
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_CODEX_BIN.to_owned());
        let codex_model = lookup("ESCUREL_RUNNER_CODEX_MODEL").filter(|s| !s.is_empty());

        let adk_bin = lookup("ESCUREL_RUNNER_ADK_BIN")
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_ADK_BIN.to_owned());
        let adk_model = lookup("ESCUREL_RUNNER_ADK_MODEL").filter(|s| !s.is_empty());

        let max_attempts = match lookup("ESCUREL_RUNNER_MAX_ATTEMPTS") {
            Some(raw) if !raw.is_empty() => match raw.parse::<u32>() {
                Ok(n) if n >= 1 => n,
                _ => return Err(ConfigError::InvalidMaxAttempts { value: raw }),
            },
            _ => DEFAULT_MAX_ATTEMPTS,
        };
        let retry_backoff = match lookup("ESCUREL_RUNNER_RETRY_BACKOFF") {
            Some(raw) if !raw.is_empty() => {
                parse_duration(&raw).ok_or(ConfigError::InvalidRetryBackoff { value: raw })?
            }
            _ => DEFAULT_RETRY_BACKOFF,
        };

        let max_depth = match lookup("ESCUREL_RUNNER_MAX_DEPTH") {
            Some(raw) if !raw.is_empty() => raw
                .parse::<u32>()
                .map_err(|_| ConfigError::InvalidMaxDepth { value: raw })?,
            _ => DEFAULT_MAX_DEPTH,
        };
        let max_runs_per_root = match lookup("ESCUREL_RUNNER_MAX_RUNS_PER_ROOT") {
            Some(raw) if !raw.is_empty() => match raw.parse::<u64>() {
                Ok(n) if n >= 1 => n,
                _ => return Err(ConfigError::InvalidMaxRunsPerRoot { value: raw }),
            },
            _ => DEFAULT_MAX_RUNS_PER_ROOT,
        };

        let tenant_runs_per_min = parse_positive_u64(
            "ESCUREL_RUNNER_TENANT_RUNS_PER_MIN",
            &lookup,
            DEFAULT_TENANT_RUNS_PER_MIN,
        )?;
        let tenant_max_concurrent = parse_positive_u64(
            "ESCUREL_RUNNER_TENANT_MAX_CONCURRENT",
            &lookup,
            DEFAULT_TENANT_MAX_CONCURRENT,
        )?;
        let max_harness_procs = match lookup("ESCUREL_RUNNER_MAX_HARNESS_PROCS") {
            Some(raw) if !raw.is_empty() => match raw.parse::<usize>() {
                Ok(n) if n >= 1 => n,
                _ => return Err(ConfigError::InvalidMaxHarnessProcs { value: raw }),
            },
            _ => DEFAULT_MAX_HARNESS_PROCS,
        };
        let drain_timeout = match lookup("ESCUREL_RUNNER_DRAIN_TIMEOUT") {
            Some(raw) if !raw.is_empty() => {
                parse_duration(&raw).ok_or(ConfigError::InvalidDrainTimeout { value: raw })?
            }
            _ => DEFAULT_DRAIN_TIMEOUT,
        };

        Ok(Self {
            listen,
            gateway_url,
            env,
            version: env!("CARGO_PKG_VERSION").to_owned(),
            webhook_secret,
            tenant,
            token,
            queue_cap,
            seen_cap,
            poll_interval,
            lint_interval,
            ledger_path,
            harness,
            claude_bin,
            claude_model,
            codex_bin,
            codex_model,
            adk_bin,
            adk_model,
            max_attempts,
            retry_backoff,
            max_depth,
            max_runs_per_root,
            tenant_runs_per_min,
            tenant_max_concurrent,
            max_harness_procs,
            drain_timeout,
        })
    }
}

/// Parse a positive-`u64` quota env var (must be ≥ 1), falling back to
/// `default` when unset/empty.
fn parse_positive_u64<F>(key: &str, lookup: &F, default: u64) -> Result<u64, ConfigError>
where
    F: Fn(&str) -> Option<String>,
{
    match lookup(key) {
        Some(raw) if !raw.is_empty() => match raw.parse::<u64>() {
            Ok(n) if n >= 1 => Ok(n),
            _ => Err(ConfigError::InvalidQuotaU64 {
                key: key.to_owned(),
                value: raw,
            }),
        },
        _ => Ok(default),
    }
}

/// Parse a `usize` env var, falling back to `default` when unset/empty.
fn parse_usize<F>(key: &str, lookup: &F, default: usize) -> Result<usize, ConfigError>
where
    F: Fn(&str) -> Option<String>,
{
    match lookup(key) {
        Some(raw) if !raw.is_empty() => {
            raw.parse::<usize>().map_err(|_| ConfigError::InvalidUsize {
                key: key.to_owned(),
                value: raw,
            })
        }
        _ => Ok(default),
    }
}

/// Parse a humantime-lite duration: a non-negative integer with a unit
/// suffix `ms`, `s`, or `m` (e.g. `30s`, `1500ms`, `2m`). A bare integer is
/// treated as seconds. Returns `None` for anything unparseable.
fn parse_duration(raw: &str) -> Option<Duration> {
    let s = raw.trim();
    if let Some(num) = s.strip_suffix("ms") {
        return num.trim().parse::<u64>().ok().map(Duration::from_millis);
    }
    if let Some(num) = s.strip_suffix('s') {
        return num.trim().parse::<u64>().ok().map(Duration::from_secs);
    }
    if let Some(num) = s.strip_suffix('m') {
        return num
            .trim()
            .parse::<u64>()
            .ok()
            .map(|m| Duration::from_secs(m * 60));
    }
    s.parse::<u64>().ok().map(Duration::from_secs)
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
        assert_eq!(cfg.tenant, None);
        assert_eq!(cfg.token, None);
        assert_eq!(cfg.queue_cap, DEFAULT_QUEUE_CAP);
        assert_eq!(cfg.seen_cap, DEFAULT_SEEN_CAP);
        assert_eq!(cfg.poll_interval, DEFAULT_POLL_INTERVAL);
        assert_eq!(cfg.ledger_path, DEFAULT_LEDGER_PATH);
        assert_eq!(cfg.harness, DEFAULT_HARNESS);
        assert_eq!(cfg.claude_bin, DEFAULT_CLAUDE_BIN);
        assert_eq!(cfg.claude_model, None);
        assert_eq!(cfg.codex_bin, DEFAULT_CODEX_BIN);
        assert_eq!(cfg.codex_model, None);
        assert_eq!(cfg.adk_bin, DEFAULT_ADK_BIN);
        assert_eq!(cfg.adk_model, None);
        assert_eq!(cfg.max_attempts, DEFAULT_MAX_ATTEMPTS);
        assert_eq!(cfg.retry_backoff, DEFAULT_RETRY_BACKOFF);
        assert_eq!(cfg.max_depth, DEFAULT_MAX_DEPTH);
        assert_eq!(cfg.max_runs_per_root, DEFAULT_MAX_RUNS_PER_ROOT);
        assert_eq!(cfg.tenant_runs_per_min, DEFAULT_TENANT_RUNS_PER_MIN);
        assert_eq!(cfg.tenant_max_concurrent, DEFAULT_TENANT_MAX_CONCURRENT);
        assert_eq!(cfg.max_harness_procs, DEFAULT_MAX_HARNESS_PROCS);
        assert_eq!(cfg.drain_timeout, DEFAULT_DRAIN_TIMEOUT);
    }

    #[test]
    fn quota_config_loads_and_validates() {
        let set = RunnerConfig::from_env_with(|key| match key {
            "ESCUREL_RUNNER_TENANT_RUNS_PER_MIN" => Some("5".to_owned()),
            "ESCUREL_RUNNER_TENANT_MAX_CONCURRENT" => Some("3".to_owned()),
            "ESCUREL_RUNNER_MAX_HARNESS_PROCS" => Some("4".to_owned()),
            "ESCUREL_RUNNER_DRAIN_TIMEOUT" => Some("10s".to_owned()),
            _ => None,
        })
        .expect("quota config must parse");
        assert_eq!(set.tenant_runs_per_min, 5);
        assert_eq!(set.tenant_max_concurrent, 3);
        assert_eq!(set.max_harness_procs, 4);
        assert_eq!(set.drain_timeout, Duration::from_secs(10));

        let zero = RunnerConfig::from_env_with(|key| {
            (key == "ESCUREL_RUNNER_TENANT_RUNS_PER_MIN").then(|| "0".to_owned())
        })
        .expect_err("runs_per_min=0 must fail");
        assert!(matches!(zero, ConfigError::InvalidQuotaU64 { .. }));

        let bad_procs = RunnerConfig::from_env_with(|key| {
            (key == "ESCUREL_RUNNER_MAX_HARNESS_PROCS").then(|| "lots".to_owned())
        })
        .expect_err("a bad harness-proc cap must fail");
        assert!(matches!(
            bad_procs,
            ConfigError::InvalidMaxHarnessProcs { .. }
        ));
    }

    #[test]
    fn loop_control_config_loads_and_validates() {
        let set = RunnerConfig::from_env_with(|key| match key {
            "ESCUREL_RUNNER_MAX_DEPTH" => Some("3".to_owned()),
            "ESCUREL_RUNNER_MAX_RUNS_PER_ROOT" => Some("16".to_owned()),
            _ => None,
        })
        .expect("loop-control config must parse");
        assert_eq!(set.max_depth, 3);
        assert_eq!(set.max_runs_per_root, 16);

        let bad_depth = RunnerConfig::from_env_with(|key| {
            (key == "ESCUREL_RUNNER_MAX_DEPTH").then(|| "deep".to_owned())
        })
        .expect_err("a bad max depth must fail");
        assert!(matches!(bad_depth, ConfigError::InvalidMaxDepth { .. }));

        let zero_budget = RunnerConfig::from_env_with(|key| {
            (key == "ESCUREL_RUNNER_MAX_RUNS_PER_ROOT").then(|| "0".to_owned())
        })
        .expect_err("a zero per-root budget must fail");
        assert!(matches!(
            zero_budget,
            ConfigError::InvalidMaxRunsPerRoot { .. }
        ));
    }

    #[test]
    fn retry_config_loads_and_validates() {
        let set = RunnerConfig::from_env_with(|key| match key {
            "ESCUREL_RUNNER_MAX_ATTEMPTS" => Some("5".to_owned()),
            "ESCUREL_RUNNER_RETRY_BACKOFF" => Some("250ms".to_owned()),
            _ => None,
        })
        .expect("retry config must parse");
        assert_eq!(set.max_attempts, 5);
        assert_eq!(set.retry_backoff, Duration::from_millis(250));

        let zero = RunnerConfig::from_env_with(|key| {
            (key == "ESCUREL_RUNNER_MAX_ATTEMPTS").then(|| "0".to_owned())
        })
        .expect_err("max_attempts=0 must fail (at least one attempt is made)");
        assert!(matches!(zero, ConfigError::InvalidMaxAttempts { .. }));

        let bad_backoff = RunnerConfig::from_env_with(|key| {
            (key == "ESCUREL_RUNNER_RETRY_BACKOFF").then(|| "soon".to_owned())
        })
        .expect_err("a bad backoff must fail");
        assert!(matches!(
            bad_backoff,
            ConfigError::InvalidRetryBackoff { .. }
        ));
    }

    #[test]
    fn adk_bin_and_model_load_when_set_and_ignore_empty() {
        let set = RunnerConfig::from_env_with(|key| match key {
            "ESCUREL_RUNNER_ADK_BIN" => Some("/opt/datazoo-agent".to_owned()),
            "ESCUREL_RUNNER_ADK_MODEL" => Some("gemini-3.5-flash".to_owned()),
            _ => None,
        })
        .expect("adk config must parse");
        assert_eq!(set.adk_bin, "/opt/datazoo-agent");
        assert_eq!(set.adk_model, Some("gemini-3.5-flash".to_owned()));

        let empty = RunnerConfig::from_env_with(|key| match key {
            "ESCUREL_RUNNER_ADK_BIN" => Some(String::new()),
            "ESCUREL_RUNNER_ADK_MODEL" => Some(String::new()),
            _ => None,
        })
        .expect("empty adk config must parse");
        assert_eq!(
            empty.adk_bin, DEFAULT_ADK_BIN,
            "empty adk bin falls back to the default"
        );
        assert_eq!(empty.adk_model, None, "empty model is treated as unset");
    }

    #[test]
    fn codex_bin_and_model_load_when_set_and_ignore_empty() {
        let set = RunnerConfig::from_env_with(|key| match key {
            "ESCUREL_RUNNER_CODEX_BIN" => Some("/usr/local/bin/codex".to_owned()),
            "ESCUREL_RUNNER_CODEX_MODEL" => Some("o3".to_owned()),
            _ => None,
        })
        .expect("codex config must parse");
        assert_eq!(set.codex_bin, "/usr/local/bin/codex");
        assert_eq!(set.codex_model, Some("o3".to_owned()));

        let empty = RunnerConfig::from_env_with(|key| match key {
            "ESCUREL_RUNNER_CODEX_BIN" => Some(String::new()),
            "ESCUREL_RUNNER_CODEX_MODEL" => Some(String::new()),
            _ => None,
        })
        .expect("empty codex config must parse");
        assert_eq!(
            empty.codex_bin, DEFAULT_CODEX_BIN,
            "empty codex bin falls back to the default"
        );
        assert_eq!(empty.codex_model, None, "empty model is treated as unset");
    }

    #[test]
    fn claude_bin_and_model_load_when_set_and_ignore_empty() {
        let set = RunnerConfig::from_env_with(|key| match key {
            "ESCUREL_RUNNER_CLAUDE_BIN" => Some("/usr/local/bin/claude".to_owned()),
            "ESCUREL_RUNNER_CLAUDE_MODEL" => Some("opus".to_owned()),
            _ => None,
        })
        .expect("claude config must parse");
        assert_eq!(set.claude_bin, "/usr/local/bin/claude");
        assert_eq!(set.claude_model, Some("opus".to_owned()));

        let empty = RunnerConfig::from_env_with(|key| match key {
            "ESCUREL_RUNNER_CLAUDE_BIN" => Some(String::new()),
            "ESCUREL_RUNNER_CLAUDE_MODEL" => Some(String::new()),
            _ => None,
        })
        .expect("empty claude config must parse");
        assert_eq!(
            empty.claude_bin, DEFAULT_CLAUDE_BIN,
            "empty claude bin falls back to the default"
        );
        assert_eq!(empty.claude_model, None, "empty model is treated as unset");
    }

    #[test]
    fn harness_loads_when_set_and_ignores_empty() {
        let set = RunnerConfig::from_env_with(|key| {
            (key == "ESCUREL_RUNNER_HARNESS").then(|| "codex".to_owned())
        })
        .expect("a harness selector must parse");
        assert_eq!(set.harness, "codex");

        let empty =
            RunnerConfig::from_env_with(|key| (key == "ESCUREL_RUNNER_HARNESS").then(String::new))
                .expect("an empty harness selector must parse");
        assert_eq!(
            empty.harness, DEFAULT_HARNESS,
            "empty harness falls back to the default"
        );
    }

    #[test]
    fn ledger_path_loads_when_set_and_ignores_empty() {
        let set = RunnerConfig::from_env_with(|key| {
            (key == "ESCUREL_RUNNER_LEDGER_PATH").then(|| "/data/runner.sqlite".to_owned())
        })
        .expect("a ledger path must parse");
        assert_eq!(set.ledger_path, "/data/runner.sqlite");

        let empty = RunnerConfig::from_env_with(|key| {
            (key == "ESCUREL_RUNNER_LEDGER_PATH").then(String::new)
        })
        .expect("an empty ledger path must parse");
        assert_eq!(
            empty.ledger_path, DEFAULT_LEDGER_PATH,
            "empty ledger path falls back to the default"
        );
    }

    #[test]
    fn poller_config_loads_when_set() {
        let cfg = RunnerConfig::from_env_with(|key| match key {
            "ESCUREL_RUNNER_TENANT" => Some("carl".to_owned()),
            "ESCUREL_RUNNER_TOKEN" => Some("tok".to_owned()),
            "ESCUREL_RUNNER_QUEUE_CAP" => Some("8".to_owned()),
            "ESCUREL_RUNNER_SEEN_CAP" => Some("16".to_owned()),
            "ESCUREL_RUNNER_POLL_INTERVAL" => Some("250ms".to_owned()),
            _ => None,
        })
        .expect("poller config must parse");
        assert_eq!(cfg.tenant, Some("carl".to_owned()));
        assert_eq!(cfg.token, Some("tok".to_owned()));
        assert_eq!(cfg.queue_cap, 8);
        assert_eq!(cfg.seen_cap, 16);
        assert_eq!(cfg.poll_interval, Duration::from_millis(250));
    }

    #[test]
    fn lint_interval_is_disabled_by_default_and_parses_when_set() {
        // Default: unset ⇒ disabled ⇒ markdown-only tenants never lint.
        let off = RunnerConfig::from_env_with(|_| None).expect("defaults");
        assert_eq!(off.lint_interval, None);
        // Set ⇒ parsed with the same duration grammar as the poller.
        let on = RunnerConfig::from_env_with(|key| {
            (key == "ESCUREL_RUNNER_LINT_INTERVAL").then(|| "1h".to_owned())
        })
        .expect("lint interval must parse");
        assert_eq!(on.lint_interval, Some(Duration::from_secs(3600)));
    }

    #[test]
    fn poll_interval_accepts_units_and_bare_seconds() {
        assert_eq!(parse_duration("30s"), Some(Duration::from_secs(30)));
        assert_eq!(parse_duration("1500ms"), Some(Duration::from_millis(1500)));
        assert_eq!(parse_duration("2m"), Some(Duration::from_secs(120)));
        assert_eq!(parse_duration("5"), Some(Duration::from_secs(5)));
        assert_eq!(parse_duration("nope"), None);
    }

    #[test]
    fn invalid_poll_interval_is_an_error() {
        let err = RunnerConfig::from_env_with(|key| {
            (key == "ESCUREL_RUNNER_POLL_INTERVAL").then(|| "soon".to_owned())
        })
        .expect_err("a bad poll interval must fail");
        assert!(matches!(err, ConfigError::InvalidPollInterval { .. }));
    }

    #[test]
    fn invalid_queue_cap_is_an_error() {
        let err = RunnerConfig::from_env_with(|key| {
            (key == "ESCUREL_RUNNER_QUEUE_CAP").then(|| "lots".to_owned())
        })
        .expect_err("a bad queue cap must fail");
        assert!(matches!(err, ConfigError::InvalidUsize { .. }));
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
