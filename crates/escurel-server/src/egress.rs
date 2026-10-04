//! The one place outbound calls to registered remote endpoints are policed.
//!
//! The gateway reaches `openapi` / `mcp` upstreams on behalf of tenants. Endpoint URLs are
//! registered by an admin (never taken from page content), but a registered host can still be
//! pointed — or re-pointed by DNS — at something it must never reach: the cloud metadata service,
//! a private network, the gateway itself. This module is the egress policy:
//!
//! - **https only.** Plain `http` is allowed solely to a loopback address and only when the policy
//!   says `allow_loopback` (tests and local development).
//! - **IP checks after DNS resolution.** Every address the host resolves to must be public;
//!   loopback, private (RFC 1918 / ULA), link-local (including 169.254.169.254), CGNAT, unspecified,
//!   multicast and documentation ranges are refused. The connection is then PINNED to the checked
//!   addresses, so a second, different DNS answer cannot be used at connect time (rebinding).
//! - **No redirects.** A 3xx is an error, never followed: a public host cannot bounce us inward.
//! - **Caps.** A response is read as a stream and refused past `max_response_bytes`; every call has
//!   a timeout; each endpoint has a concurrency limit and a token-bucket rate limit.
//! - **Quiet errors.** [`EgressError`] never carries a URL query, a header or a secret.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use reqwest::Url;
use tokio::sync::Semaphore;

/// Default cap on one upstream response (4 MiB).
pub const DEFAULT_MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
/// Default timeout of one upstream call.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
/// Hard ceiling for a configured timeout.
pub const MAX_TIMEOUT: Duration = Duration::from_secs(30);
/// Default simultaneous calls per endpoint.
pub const DEFAULT_MAX_CONCURRENCY: usize = 8;
/// Default calls per second per endpoint (token bucket, burst = the same number).
pub const DEFAULT_RATE_PER_SEC: u32 = 50;

/// What an outbound call may do. [`Default`] is the strict production policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressPolicy {
    /// Allow http to, and calls into, loopback addresses. OFF in production; the test harness and
    /// local development turn it on (`ESCUREL_EGRESS_ALLOW_LOOPBACK=1`).
    pub allow_loopback: bool,
    pub max_response_bytes: usize,
    pub timeout: Duration,
    pub max_concurrency: usize,
    pub rate_per_sec: u32,
    /// The pause between the attempts of a write-back (jittered, doubled each time).
    pub write_retry_backoff: Duration,
    /// What a tenant may name as a credential (`secret_ref`): see [`crate::secret_policy`].
    pub secrets: crate::secret_policy::SecretPolicy,
    /// The directories a tenant admin may attach SQLite / DuckDB database FILES from
    /// (`ESCUREL_SQL_FILE_DIRS`, `:`-separated). Empty (the default) means file databases are not
    /// attachable at all: a path in a credential is a server file, and which ones are exposed is the
    /// operator's decision.
    pub sql_file_dirs: Vec<std::path::PathBuf>,
}

impl Default for EgressPolicy {
    fn default() -> Self {
        Self {
            allow_loopback: false,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
            timeout: DEFAULT_TIMEOUT,
            max_concurrency: DEFAULT_MAX_CONCURRENCY,
            rate_per_sec: DEFAULT_RATE_PER_SEC,
            write_retry_backoff: Duration::from_millis(500),
            secrets: crate::secret_policy::SecretPolicy::default(),
            sql_file_dirs: Vec::new(),
        }
    }
}

/// An `ESCUREL_EGRESS_*` value that cannot be used. The boot fails with this rather than silently
/// keeping the default: an operator who sets `TIMEOUT_MS=5s` must not believe it applied.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid value for {var}: {value:?} ({reason})")]
pub struct EgressConfigError {
    pub var: &'static str,
    pub value: String,
    pub reason: &'static str,
}

impl EgressPolicy {
    /// The policy from `ESCUREL_EGRESS_*` (12-factor): `ALLOW_LOOPBACK` (`1`/`true`/`0`/`false`),
    /// `MAX_RESPONSE_BYTES`, `TIMEOUT_MS` (clamped to [`MAX_TIMEOUT`]), `MAX_CONCURRENCY`,
    /// `RATE_PER_SEC`, `WRITE_RETRY_BACKOFF_MS`. Unset keeps the strict default; a value that does not
    /// parse (or a zero limit) is an ERROR, never silently ignored.
    ///
    /// # Errors
    /// [`EgressConfigError`] naming the offending variable.
    pub fn from_source(get: &dyn Fn(&str) -> Option<String>) -> Result<Self, EgressConfigError> {
        fn num<T: std::str::FromStr + PartialOrd + Default>(
            get: &dyn Fn(&str) -> Option<String>,
            var: &'static str,
            positive: bool,
        ) -> Result<Option<T>, EgressConfigError> {
            let Some(raw) = get(var) else { return Ok(None) };
            let bad = |reason| EgressConfigError {
                var,
                value: raw.clone(),
                reason,
            };
            let n: T = raw
                .trim()
                .parse()
                .map_err(|_| bad("expected a whole number"))?;
            if positive && n <= T::default() {
                return Err(bad("must be at least 1"));
            }
            Ok(Some(n))
        }
        let mut p = Self::default();
        if let Some(raw) = get("ESCUREL_EGRESS_ALLOW_LOOPBACK") {
            p.allow_loopback = match raw.trim().to_ascii_lowercase().as_str() {
                "1" | "true" => true,
                "0" | "false" => false,
                _ => {
                    return Err(EgressConfigError {
                        var: "ESCUREL_EGRESS_ALLOW_LOOPBACK",
                        value: raw,
                        reason: "expected 1, true, 0 or false",
                    });
                }
            };
            if p.allow_loopback {
                tracing::warn!(
                    "ESCUREL_EGRESS_ALLOW_LOOPBACK is ON: outbound calls to loopback addresses are \
                     allowed. This is for development and tests, never for production."
                );
            }
        }
        if let Some(n) = num(get, "ESCUREL_EGRESS_MAX_RESPONSE_BYTES", true)? {
            p.max_response_bytes = n;
        }
        if let Some(ms) = num::<u64>(get, "ESCUREL_EGRESS_TIMEOUT_MS", true)? {
            p.timeout = Duration::from_millis(ms).min(MAX_TIMEOUT);
        }
        if let Some(n) = num(get, "ESCUREL_EGRESS_MAX_CONCURRENCY", true)? {
            p.max_concurrency = n;
        }
        if let Some(n) = num(get, "ESCUREL_EGRESS_RATE_PER_SEC", true)? {
            p.rate_per_sec = n;
        }
        if let Some(ms) = num::<u64>(get, "ESCUREL_EGRESS_WRITE_RETRY_BACKOFF_MS", false)? {
            p.write_retry_backoff = Duration::from_millis(ms);
        }
        p.secrets = crate::secret_policy::SecretPolicy::from_env();
        if let Some(v) = get("ESCUREL_SQL_FILE_DIRS") {
            p.sql_file_dirs = v
                .split(':')
                .filter(|s| !s.trim().is_empty())
                .map(|s| std::path::PathBuf::from(s.trim()))
                .collect();
        }
        Ok(p)
    }

    /// [`Self::from_source`] over the process environment.
    ///
    /// # Errors
    /// [`EgressConfigError`] naming the offending variable.
    pub fn try_from_env() -> Result<Self, EgressConfigError> {
        Self::from_source(&|k| std::env::var(k).ok())
    }

    /// [`Self::try_from_env`] for callers with no error channel (test tooling): an unusable value is a
    /// loud panic naming the variable, never a silent default.
    #[must_use]
    pub fn from_env() -> Self {
        Self::try_from_env().unwrap_or_else(|e| panic!("{e}"))
    }
}

/// Why an outbound call was refused or failed. The messages are safe to hand to an agent: no URL
/// query, no header, no secret.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EgressError {
    #[error("egress policy: the endpoint URL is not usable ({0})")]
    BadUrl(&'static str),
    #[error("egress policy: only https endpoints are allowed")]
    SchemeNotAllowed,
    #[error("egress policy: the endpoint resolves to a non-public address")]
    AddressNotAllowed,
    #[error("egress policy: the endpoint host did not resolve")]
    Unresolvable,
    #[error("egress policy: the upstream tried to redirect (status {0}); redirects are refused")]
    Redirect(u16),
    #[error("egress policy: the upstream response is larger than {0} bytes")]
    TooLarge(usize),
    #[error("egress policy: the upstream did not answer within {0} ms")]
    Timeout(u128),
    #[error("egress policy: too many calls to this endpoint, try again shortly")]
    RateLimited,
    #[error("transport error: {0}")]
    Transport(String),
}

/// Is `ip` an address the gateway may call out to? Everything not clearly public is refused.
#[must_use]
pub fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => {
            // An IPv6 address that WRAPS an IPv4 one is judged as that IPv4 address, in every
            // spelling: mapped (`::ffff:a.b.c.d`), compatible (`::a.b.c.d`), NAT64
            // (`64:ff9b::a.b.c.d`, how an IPv6-only cluster reaches the metadata address) and 6to4
            // (`2002:aabb:ccdd::/48`). Otherwise `::ffff:169.254.169.254` and its cousins slip through.
            if let Some(v4) = embedded_ipv4(v6) {
                return is_public_v4(v4);
            }
            is_public_v6(v6)
        }
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local() // 169.254/16, includes the cloud metadata address
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
        || o[0] == 0
        || (o[0] == 100 && (64..=127).contains(&o[1])) // CGNAT 100.64/10
        || (o[0] == 192 && o[1] == 0 && o[2] == 0) // IETF protocol assignments 192.0.0/24
        || (o[0] == 198 && (o[1] == 18 || o[1] == 19)) // benchmarking 198.18/15
        || o[0] >= 240) // reserved
}

/// The IPv4 address an IPv6 address wraps, if it wraps one.
fn embedded_ipv4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    let s = ip.segments();
    let from =
        |hi: u16, lo: u16| Ipv4Addr::new((hi >> 8) as u8, hi as u8, (lo >> 8) as u8, lo as u8);
    if let Some(v4) = ip.to_ipv4_mapped() {
        return Some(v4);
    }
    // IPv4-compatible `::a.b.c.d` (deprecated). `::` and `::1` are handled as unspecified/loopback.
    if s[..6].iter().all(|&x| x == 0) && (s[6] != 0 || s[7] > 1) {
        return Some(from(s[6], s[7]));
    }
    // NAT64 well-known prefix 64:ff9b::/96.
    if s[0] == 0x0064 && s[1] == 0xff9b && s[2..6].iter().all(|&x| x == 0) {
        return Some(from(s[6], s[7]));
    }
    // 6to4 2002::/16 embeds the IPv4 address in bits 16..48.
    if s[0] == 0x2002 {
        return Some(from(s[1], s[2]));
    }
    None
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    let s = ip.segments();
    !(ip.is_unspecified()
        || (s[0] & 0xffc0) == 0xfec0 // site-local fec0::/10 (deprecated, still routed by some stacks)
        || (s[0] == 0x0064 && s[1] == 0xff9b) // NAT64 prefixes that did not embed a public IPv4
        || (s[0] == 0x2001 && s[1] == 0x0000) // Teredo 2001::/32
        || ip.is_loopback()
        || ip.is_multicast()
        || (s[0] & 0xfe00) == 0xfc00 // unique local fc00::/7
        || (s[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
        || (s[0] == 0x2001 && s[1] == 0x0db8)) // documentation 2001:db8::/32
}

/// Per-endpoint call limiter: a concurrency semaphore plus a token bucket.
struct Limiter {
    sem: Arc<Semaphore>,
    bucket: Mutex<(f64, Instant)>,
}

/// Whether `body` (the bytes of an event stream so far) holds a COMPLETE event whose `data:` is a
/// JSON-RPC response (`result` or `error`): complete means the blank line that ends an event has
/// arrived, so a half-received event is never mistaken for it.
fn sse_has_response(body: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(body) else {
        return false;
    };
    let text = text.replace("\r\n", "\n");
    let Some(end) = text.rfind("\n\n") else {
        return false;
    };
    text[..end].split("\n\n").any(|event| {
        let data: String = event
            .lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .map(|d| d.strip_prefix(' ').unwrap_or(d))
            .collect::<Vec<_>>()
            .join("\n");
        serde_json::from_str::<serde_json::Value>(&data)
            .is_ok_and(|v| v.get("result").is_some() || v.get("error").is_some())
    })
}

/// A response read within the policy: status, headers and the capped body.
pub struct Capped {
    pub status: reqwest::StatusCode,
    pub headers: reqwest::header::HeaderMap,
    pub body: Vec<u8>,
}

/// An MCP streamable-HTTP session with one endpoint: the `Mcp-Session-Id` the server assigned (none
/// for a stateless server) and the protocol version negotiated at `initialize`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpSession {
    pub id: Option<String>,
    /// `None` for a legacy upstream that does not speak `initialize` at all.
    pub protocol: Option<String>,
}

/// The outbound runtime: the policy, the per-endpoint limiters and the MCP session cache.
/// How long the host's name may take to resolve before the call is refused.
const DNS_TIMEOUT: Duration = Duration::from_secs(5);

/// Calls the policy has refused since boot (an address, scheme or URL the policy does not allow).
static REFUSALS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The number of outbound calls the egress policy has refused since boot, for `/metrics`.
#[must_use]
pub fn egress_refusals() -> u64 {
    REFUSALS.load(std::sync::atomic::Ordering::Relaxed)
}

fn log_refusal(host: &str, reason: &str) {
    REFUSALS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // Host and reason only: never a URL query, header or secret.
    tracing::warn!(%host, %reason, "egress policy refused an outbound call");
}

pub struct Egress {
    policy: EgressPolicy,
    limiters: Mutex<HashMap<String, Arc<Limiter>>>,
    mcp_sessions: Mutex<HashMap<String, McpSession>>,
    /// One handshake at a time per endpoint (see [`Self::session_lock`]).
    session_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    rpc_id: AtomicU64,
    /// Where outcomes are counted (`escurel_egress_total{outcome}`); `None` in tools that build an
    /// `Egress` without a registry.
    metrics: Option<Arc<escurel_obs::Metrics>>,
}

impl Egress {
    #[must_use]
    pub fn new(policy: EgressPolicy) -> Self {
        Self {
            metrics: None,
            policy,
            limiters: Mutex::new(HashMap::new()),
            mcp_sessions: Mutex::new(HashMap::new()),
            session_locks: Mutex::new(HashMap::new()),
            rpc_id: AtomicU64::new(1),
        }
    }

    /// Count this runtime's outbound outcomes on `metrics` (`escurel_egress_total{outcome}`).
    #[must_use]
    pub fn with_metrics(mut self, metrics: Arc<escurel_obs::Metrics>) -> Self {
        self.metrics = Some(metrics);
        self
    }

    fn count(&self, outcome: &str) {
        if let Some(m) = &self.metrics {
            m.inc_egress(outcome);
        }
    }

    /// The cached MCP session for an endpoint key, if one was established.
    #[must_use]
    pub fn mcp_session(&self, key: &str) -> Option<McpSession> {
        self.mcp_sessions
            .lock()
            .expect("sessions")
            .get(key)
            .cloned()
    }

    pub fn set_mcp_session(&self, key: &str, session: McpSession) {
        self.mcp_sessions
            .lock()
            .expect("sessions")
            .insert(key.to_owned(), session);
    }

    /// Forget a session (it expired, or the endpoint was re-registered).
    pub fn drop_mcp_session(&self, key: &str) {
        self.mcp_sessions.lock().expect("sessions").remove(key);
    }

    /// Forget a session only if it is still the one the caller failed with: a task that got a 404 on
    /// the OLD session must not discard the fresh one another task has just established.
    pub fn drop_mcp_session_if(&self, key: &str, failed: &McpSession) {
        let mut g = self.mcp_sessions.lock().expect("sessions");
        if g.get(key) == Some(failed) {
            g.remove(key);
        }
    }

    /// The lock that serialises the `initialize` handshake with one endpoint, so callers racing for
    /// the first session share ONE handshake instead of each opening a session of their own.
    #[must_use]
    pub fn session_lock(&self, key: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut g = self.session_locks.lock().expect("session locks");
        Arc::clone(g.entry(key.to_owned()).or_default())
    }

    /// A fresh JSON-RPC request id.
    #[must_use]
    pub fn next_rpc_id(&self) -> u64 {
        self.rpc_id.fetch_add(1, Ordering::SeqCst)
    }

    #[must_use]
    pub fn policy(&self) -> &EgressPolicy {
        &self.policy
    }

    fn limiter(&self, key: &str) -> Arc<Limiter> {
        let mut map = self.limiters.lock().expect("limiter map");
        Arc::clone(map.entry(key.to_owned()).or_insert_with(|| {
            Arc::new(Limiter {
                sem: Arc::new(Semaphore::new(self.policy.max_concurrency.max(1))),
                bucket: Mutex::new((f64::from(self.policy.rate_per_sec.max(1)), Instant::now())),
            })
        }))
    }

    /// Admit one call to `endpoint` (a tenant-scoped key): take a token from the bucket and a
    /// concurrency permit. The permit is held for the call.
    ///
    /// # Errors
    /// [`EgressError::RateLimited`] when the bucket is empty or every concurrency slot is busy.
    pub fn admit(&self, endpoint: &str) -> Result<tokio::sync::OwnedSemaphorePermit, EgressError> {
        let lim = self.limiter(endpoint);
        // The slot first: a call refused because every slot is busy must not spend a token it never
        // used, or a burst of refusals would empty the bucket for the calls that follow.
        let permit = Arc::clone(&lim.sem).try_acquire_owned().map_err(|_| {
            self.count("limited");
            EgressError::RateLimited
        })?;
        let rate = f64::from(self.policy.rate_per_sec.max(1));
        let mut b = lim.bucket.lock().expect("bucket");
        let now = Instant::now();
        let refill = now.duration_since(b.1).as_secs_f64() * rate;
        b.0 = (b.0 + refill).min(rate);
        b.1 = now;
        if b.0 < 1.0 {
            // `permit` is dropped here: the slot is released, the bucket untouched.
            self.count("limited");
            return Err(EgressError::RateLimited);
        }
        b.0 -= 1.0;
        Ok(permit)
    }

    /// Validate `url` against the policy, resolve it, and return a client PINNED to the checked
    /// addresses (no redirects, the policy timeout) together with the parsed URL.
    ///
    /// # Errors
    /// Any [`EgressError`] describing why the URL may not be called.
    pub async fn client_for(&self, url: &str) -> Result<(reqwest::Client, Url), EgressError> {
        let parsed = Url::parse(url).map_err(|_| EgressError::BadUrl("not a URL"))?;
        let host = parsed
            .host_str()
            .ok_or(EgressError::BadUrl("no host"))?
            .to_owned();
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Err(EgressError::BadUrl("credentials in the URL"));
        }
        let port = parsed
            .port_or_known_default()
            .ok_or(EgressError::BadUrl("no port"))?;
        // A literal IP needs no DNS; a name is resolved here and the answers are pinned below.
        let addrs: Vec<SocketAddr> = if let Ok(ip) = host.trim_matches(['[', ']']).parse::<IpAddr>()
        {
            vec![SocketAddr::new(ip, port)]
        } else {
            tokio::time::timeout(DNS_TIMEOUT, tokio::net::lookup_host((host.as_str(), port)))
                .await
                .map_err(|_| EgressError::Unresolvable)?
                .map_err(|_| EgressError::Unresolvable)?
                .collect()
        };
        if addrs.is_empty() {
            return Err(EgressError::Unresolvable);
        }
        for a in &addrs {
            let ip = a.ip();
            let loopback_ok = self.policy.allow_loopback && ip.is_loopback();
            if !loopback_ok && !is_public_ip(ip) {
                log_refusal(&host, "non-public address");
                self.count("refused");
                return Err(EgressError::AddressNotAllowed);
            }
        }
        // Scheme last, so an http URL to a private host is reported as the address problem it is.
        match parsed.scheme() {
            "https" => {}
            "http" if self.policy.allow_loopback && addrs.iter().all(|a| a.ip().is_loopback()) => {}
            _ => {
                log_refusal(&host, "scheme not allowed");
                self.count("refused");
                return Err(EgressError::SchemeNotAllowed);
            }
        }
        // `no_proxy`: an HTTPS_PROXY in the environment would send the request to the proxy with the
        // hostname, bypassing the addresses we checked and pinned.
        let mut builder = reqwest::Client::builder()
            .no_proxy()
            .timeout(self.policy.timeout)
            .redirect(reqwest::redirect::Policy::none());
        // Pin the connection to what was checked: a DNS answer that changes between the check and
        // the connect (rebinding) is never used.
        if host.parse::<IpAddr>().is_err() {
            builder = builder.resolve_to_addrs(&host, &addrs);
        }
        let client = builder
            .build()
            .map_err(|e| EgressError::Transport(sanitize(&e)))?;
        Ok((client, parsed))
    }

    /// Send `req`, refuse a redirect, and read the body as a stream up to the response cap.
    ///
    /// # Errors
    /// Any [`EgressError`]: transport, redirect, timeout, or an oversize body.
    pub async fn send_capped(&self, req: reqwest::RequestBuilder) -> Result<Capped, EgressError> {
        let out = self.send_capped_inner(req).await;
        self.count(match &out {
            Ok(_) => "ok",
            Err(EgressError::Timeout(_)) => "timeout",
            Err(EgressError::Redirect(_)) => "refused",
            Err(_) => "error",
        });
        out
    }

    async fn send_capped_inner(&self, req: reqwest::RequestBuilder) -> Result<Capped, EgressError> {
        let mut resp = req.send().await.map_err(|e| self.map_err(&e))?;
        let status = resp.status();
        let headers = resp.headers().clone();
        if status.is_redirection() {
            return Err(EgressError::Redirect(status.as_u16()));
        }
        let cap = self.policy.max_response_bytes;
        if resp.content_length().is_some_and(|n| n as usize > cap) {
            return Err(EgressError::TooLarge(cap));
        }
        let streaming = headers
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|c| c.to_ascii_lowercase().contains("text/event-stream"));
        let mut body = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(|e| self.map_err(&e))? {
            if body.len() + chunk.len() > cap {
                return Err(EgressError::TooLarge(cap));
            }
            body.extend_from_slice(&chunk);
            // A server may answer on an event stream and never close it. The caller wants the
            // JSON-RPC response, so stop as soon as one COMPLETE event carries it, instead of
            // waiting for the stream to end (which, past the timeout, reports a failure for a
            // request the upstream may already have applied).
            if streaming && sse_has_response(&body) {
                break;
            }
        }
        Ok(Capped {
            status,
            headers,
            body,
        })
    }

    fn map_err(&self, e: &reqwest::Error) -> EgressError {
        if e.is_timeout() {
            EgressError::Timeout(self.policy.timeout.as_millis())
        } else {
            EgressError::Transport(sanitize(e))
        }
    }
}

/// A transport error as text with the URL removed: a reqwest error otherwise embeds the full URL,
/// query string included.
fn sanitize(e: &reqwest::Error) -> String {
    let mut s = e.to_string();
    if let Some(url) = e.url() {
        s = s.replace(url.as_str(), "<endpoint>");
        if let Some(q) = url.query() {
            s = s.replace(q, "<query>");
        }
    }
    s
}

#[cfg(test)]
mod tests {
    fn from(pairs: &[(&str, &str)]) -> Result<EgressPolicy, EgressConfigError> {
        let map: std::collections::HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        EgressPolicy::from_source(&|k| map.get(k).cloned())
    }

    /// `ESCUREL_EGRESS_TIMEOUT_MS=5s` and `ALLOW_LOOPBACK=yes` used to be IGNORED silently (strict
    /// defaults kept), so an operator believed a setting applied that did not. They fail the boot
    /// instead, naming the variable.
    #[test]
    fn an_unparsable_value_fails_fast_naming_the_variable() {
        for (var, bad) in [
            ("ESCUREL_EGRESS_TIMEOUT_MS", "5s"),
            ("ESCUREL_EGRESS_ALLOW_LOOPBACK", "yes"),
            ("ESCUREL_EGRESS_MAX_RESPONSE_BYTES", "4MiB"),
            ("ESCUREL_EGRESS_MAX_CONCURRENCY", "-1"),
            ("ESCUREL_EGRESS_RATE_PER_SEC", "fast"),
            ("ESCUREL_EGRESS_WRITE_RETRY_BACKOFF_MS", "1.5"),
        ] {
            let err = from(&[(var, bad)]).expect_err(var);
            assert_eq!(err.var, var, "{err}");
            assert!(err.to_string().contains(var), "{err}");
            assert!(err.to_string().contains(bad), "{err}");
        }
    }

    /// A zero would silently disable the connector (no concurrency, no rate, no bytes, no time).
    #[test]
    fn zero_limits_are_refused() {
        for var in [
            "ESCUREL_EGRESS_TIMEOUT_MS",
            "ESCUREL_EGRESS_MAX_RESPONSE_BYTES",
            "ESCUREL_EGRESS_MAX_CONCURRENCY",
            "ESCUREL_EGRESS_RATE_PER_SEC",
        ] {
            assert!(from(&[(var, "0")]).is_err(), "{var}=0 must be refused");
        }
    }

    #[test]
    fn valid_values_apply_and_the_defaults_stay_strict() {
        let d = from(&[]).unwrap();
        assert!(!d.allow_loopback, "the default must not allow loopback");
        let p = from(&[
            ("ESCUREL_EGRESS_ALLOW_LOOPBACK", "TRUE"),
            ("ESCUREL_EGRESS_TIMEOUT_MS", "2500"),
            ("ESCUREL_EGRESS_MAX_RESPONSE_BYTES", "1024"),
        ])
        .unwrap();
        assert!(p.allow_loopback);
        assert_eq!(p.timeout, Duration::from_millis(2500));
        assert_eq!(p.max_response_bytes, 1024);
        // The 0/false spellings are explicit "off", not an error.
        assert!(
            !from(&[("ESCUREL_EGRESS_ALLOW_LOOPBACK", "0")])
                .unwrap()
                .allow_loopback
        );
        assert!(
            !from(&[("ESCUREL_EGRESS_ALLOW_LOOPBACK", "false")])
                .unwrap()
                .allow_loopback
        );
    }

    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn public_addresses_are_allowed() {
        for a in [
            "8.8.8.8",
            "1.1.1.1",
            "93.184.216.34",
            "2606:4700:4700::1111",
        ] {
            assert!(is_public_ip(ip(a)), "{a} should be public");
        }
    }

    #[test]
    fn everything_internal_is_refused() {
        for a in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254", // cloud metadata
            "100.64.0.1",      // CGNAT
            "0.0.0.0",
            "224.0.0.1",
            "255.255.255.255",
            "198.18.0.1",
            "240.0.0.1",
            "::1",
            "fe80::1",
            "fc00::1",
            "fd12:3456::1",
            "::",
            "2001:db8::1",
            "::ffff:169.254.169.254", // IPv4-mapped metadata
            "::ffff:10.0.0.1",
        ] {
            assert!(!is_public_ip(ip(a)), "{a} must be refused");
        }
    }

    #[tokio::test]
    async fn the_strict_policy_refuses_loopback_and_http() {
        let e = Egress::new(EgressPolicy::default());
        assert_eq!(
            e.client_for("https://127.0.0.1:9/x").await.err(),
            Some(EgressError::AddressNotAllowed)
        );
        assert_eq!(
            e.client_for("http://127.0.0.1:9/x").await.err(),
            Some(EgressError::AddressNotAllowed)
        );
        assert_eq!(
            e.client_for("https://169.254.169.254/latest/meta-data")
                .await
                .err(),
            Some(EgressError::AddressNotAllowed)
        );
        // A NAME that resolves to loopback is judged by what it resolves to.
        assert_eq!(
            e.client_for("https://localhost:9/x").await.err(),
            Some(EgressError::AddressNotAllowed)
        );
        assert_eq!(
            e.client_for("https://user:pw@8.8.8.8/x").await.err(),
            Some(EgressError::BadUrl("credentials in the URL"))
        );
    }

    #[tokio::test]
    async fn loopback_is_allowed_only_when_the_policy_says_so_and_only_to_loopback() {
        let p = EgressPolicy {
            allow_loopback: true,
            ..EgressPolicy::default()
        };
        let e = Egress::new(p);
        assert!(e.client_for("http://127.0.0.1:9/x").await.is_ok());
        // The flag opens loopback, nothing else: metadata and private ranges stay shut and plain
        // http to a public address stays refused.
        assert_eq!(
            e.client_for("http://169.254.169.254/x").await.err(),
            Some(EgressError::AddressNotAllowed)
        );
        assert_eq!(
            e.client_for("http://8.8.8.8/x").await.err(),
            Some(EgressError::SchemeNotAllowed)
        );
    }

    #[test]
    fn the_rate_limit_admits_a_burst_then_refuses() {
        let e = Egress::new(EgressPolicy {
            rate_per_sec: 3,
            max_concurrency: 100,
            ..EgressPolicy::default()
        });
        let held: Vec<_> = (0..3).map(|_| e.admit("t:crm").unwrap()).collect();
        assert_eq!(e.admit("t:crm").err(), Some(EgressError::RateLimited));
        // Another endpoint has its own bucket.
        assert!(e.admit("t:other").is_ok());
        drop(held);
    }

    #[test]
    fn a_call_refused_for_concurrency_does_not_burn_a_rate_token() {
        let e = Egress::new(EgressPolicy {
            rate_per_sec: 3,
            max_concurrency: 1,
            ..EgressPolicy::default()
        });
        let held = e.admit("t:ep").expect("the first call is admitted");
        // Every attempt while the only slot is busy is refused, and must not spend a token.
        for _ in 0..10 {
            assert!(e.admit("t:ep").is_err());
        }
        drop(held);
        // The bucket still holds the burst minus the one real call (3 - 1 = 2).
        assert!(e.admit("t:ep").is_ok(), "token left after the refusals");
        // The slot is held again by the previous permit being dropped above; free it each time.
    }

    #[test]
    fn a_session_is_dropped_only_if_it_is_still_the_one_that_failed() {
        let e = Egress::new(EgressPolicy::default());
        let old = McpSession {
            id: Some("old".to_owned()),
            protocol: None,
        };
        let fresh = McpSession {
            id: Some("fresh".to_owned()),
            protocol: None,
        };
        e.set_mcp_session("k", fresh.clone());
        // A task that failed with the OLD session must not throw away the session another task has
        // just established.
        e.drop_mcp_session_if("k", &old);
        assert_eq!(e.mcp_session("k"), Some(fresh.clone()));
        e.drop_mcp_session_if("k", &fresh);
        assert_eq!(e.mcp_session("k"), None);
    }

    #[test]
    fn the_concurrency_limit_refuses_the_call_over_the_limit() {
        let e = Egress::new(EgressPolicy {
            rate_per_sec: 1000,
            max_concurrency: 2,
            ..EgressPolicy::default()
        });
        let a = e.admit("t:crm").unwrap();
        let b = e.admit("t:crm").unwrap();
        assert_eq!(e.admit("t:crm").err(), Some(EgressError::RateLimited));
        drop(a);
        assert!(e.admit("t:crm").is_ok());
        drop(b);
    }
}
