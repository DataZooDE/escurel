//! Prometheus metrics registry the gateway increments and renders.
//!
//! Each [`Metrics`] owns its own `prometheus::Registry`; there is no
//! process-global recorder. This keeps the type test-isolatable and
//! lets the gateway hold exactly one instance behind an `Arc`.

use prometheus::{
    Encoder, Gauge, HistogramOpts, HistogramVec, IntCounterVec, IntGauge, IntGaugeVec, Opts,
    Registry, TextEncoder,
};

/// Latency histogram buckets, in seconds. Chosen to straddle the
/// tail-latency budget in `platform.md` (sub-ms reads up to slow
/// writes near 1 s).
const LATENCY_BUCKETS: &[f64] = &[
    0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5,
];

/// Per-tool latency buckets, in **milliseconds** — the unit
/// `platform.md` names for `escurel_tool_latency_ms`. Straddles the
/// sub-ms read budget through ~1 s writes.
const TOOL_LATENCY_MS_BUCKETS: &[f64] = &[
    1.0, 2.5, 5.0, 10.0, 25.0, 50.0, 100.0, 250.0, 500.0, 1000.0, 2500.0,
];

/// A metrics registry the gateway increments; renders Prometheus text.
///
/// Cloneable handles are not provided — wrap in `Arc` to share.
pub struct Metrics {
    registry: Registry,
    requests: IntCounterVec,
    latency: HistogramVec,
    up: Gauge,
    /// `escurel_tool_calls{tenant,tool,transport,status}` — the
    /// per-tool call counter from `platform.md §Observability`.
    tool_calls: IntCounterVec,
    /// `escurel_tool_latency_ms{tenant,tool,transport}`.
    tool_latency_ms: HistogramVec,
    /// `escurel_live_sessions_open` — open live-CRDT sessions.
    live_sessions: IntGauge,
    /// `escurel_audit_drift{tenant,category}` — last-observed drift
    /// counts from an `audit` run.
    audit_drift: IntGaugeVec,
}

impl Metrics {
    /// Build a fresh registry with the escurel metric families
    /// registered.
    pub fn new() -> Self {
        let registry = Registry::new();

        let requests = IntCounterVec::new(
            Opts::new(
                "escurel_requests_total",
                "Total gateway requests by route and status.",
            ),
            &["route", "status"],
        )
        .expect("valid counter opts");

        let latency = HistogramVec::new(
            HistogramOpts::new(
                "escurel_request_latency_seconds",
                "Gateway request latency in seconds by route.",
            )
            .buckets(LATENCY_BUCKETS.to_vec()),
            &["route"],
        )
        .expect("valid histogram opts");

        let up = Gauge::with_opts(Opts::new(
            "escurel_up",
            "1 when the gateway is serving, 0 otherwise.",
        ))
        .expect("valid gauge opts");

        let tool_calls = IntCounterVec::new(
            Opts::new(
                "escurel_tool_calls",
                "Agent tool calls by tenant, tool, transport, and status.",
            ),
            &["tenant", "tool", "transport", "status"],
        )
        .expect("valid counter opts");

        let tool_latency_ms = HistogramVec::new(
            HistogramOpts::new(
                "escurel_tool_latency_ms",
                "Agent tool latency in milliseconds by tenant, tool, transport.",
            )
            .buckets(TOOL_LATENCY_MS_BUCKETS.to_vec()),
            &["tenant", "tool", "transport"],
        )
        .expect("valid histogram opts");

        let live_sessions = IntGauge::with_opts(Opts::new(
            "escurel_live_sessions_open",
            "Currently-open live-CRDT sessions.",
        ))
        .expect("valid gauge opts");

        let audit_drift = IntGaugeVec::new(
            Opts::new(
                "escurel_audit_drift",
                "Last-observed markdown/index drift counts by tenant and category.",
            ),
            &["tenant", "category"],
        )
        .expect("valid gauge opts");

        registry
            .register(Box::new(requests.clone()))
            .expect("register escurel_requests_total");
        registry
            .register(Box::new(latency.clone()))
            .expect("register escurel_request_latency_seconds");
        registry
            .register(Box::new(up.clone()))
            .expect("register escurel_up");
        registry
            .register(Box::new(tool_calls.clone()))
            .expect("register escurel_tool_calls");
        registry
            .register(Box::new(tool_latency_ms.clone()))
            .expect("register escurel_tool_latency_ms");
        registry
            .register(Box::new(live_sessions.clone()))
            .expect("register escurel_live_sessions_open");
        registry
            .register(Box::new(audit_drift.clone()))
            .expect("register escurel_audit_drift");

        Self {
            registry,
            requests,
            latency,
            up,
            tool_calls,
            tool_latency_ms,
            live_sessions,
            audit_drift,
        }
    }

    /// Increment the request counter for `(route, status)`.
    pub fn inc_request(&self, route: &str, status: u16) {
        self.requests
            .with_label_values(&[route, &status.to_string()])
            .inc();
    }

    /// Observe a request's wall-clock latency, in seconds.
    pub fn observe_latency(&self, route: &str, seconds: f64) {
        self.latency.with_label_values(&[route]).observe(seconds);
    }

    /// Set the liveness gauge.
    pub fn set_up(&self, up: bool) {
        self.up.set(if up { 1.0 } else { 0.0 });
    }

    /// Record one agent tool call: bump the counter and observe its
    /// latency. `status` is a short label (`"ok"`, `"error"`,
    /// `"quota_exhausted"`); `transport` is `"mcp_http"` / `"ws"`.
    pub fn record_tool_call(
        &self,
        tenant: &str,
        tool: &str,
        transport: &str,
        status: &str,
        latency_ms: f64,
    ) {
        self.tool_calls
            .with_label_values(&[tenant, tool, transport, status])
            .inc();
        self.tool_latency_ms
            .with_label_values(&[tenant, tool, transport])
            .observe(latency_ms);
    }

    /// Set the open-live-sessions gauge (sampled at scrape time).
    pub fn set_live_sessions(&self, n: i64) {
        self.live_sessions.set(n);
    }

    /// Record the drift counts an `audit` run found for `tenant`.
    pub fn set_audit_drift(
        &self,
        tenant: &str,
        markdown_not_in_index: i64,
        index_not_in_markdown: i64,
    ) {
        self.audit_drift
            .with_label_values(&[tenant, "markdown_not_in_index"])
            .set(markdown_not_in_index);
        self.audit_drift
            .with_label_values(&[tenant, "index_not_in_markdown"])
            .set(index_not_in_markdown);
    }

    /// Render the registry in Prometheus text exposition format
    /// (the `text/plain; version=0.0.4` body served at `/metrics`).
    pub fn render_prometheus(&self) -> String {
        let mut buf = Vec::new();
        let encoder = TextEncoder::new();
        let families = self.registry.gather();
        encoder
            .encode(&families, &mut buf)
            .expect("prometheus text encoding is infallible for in-memory families");
        String::from_utf8(buf).expect("prometheus text encoder emits UTF-8")
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_tool_call_renders_labelled_counter_and_histogram() {
        let m = Metrics::new();
        m.record_tool_call("acme", "search", "mcp_http", "ok", 12.5);
        let body = m.render_prometheus();
        assert!(body.lines().any(|l| l.starts_with("escurel_tool_calls{")
            && l.contains(r#"tenant="acme""#)
            && l.contains(r#"tool="search""#)
            && l.contains(r#"transport="mcp_http""#)
            && l.contains(r#"status="ok""#)
            && l.trim_end().ends_with(" 1")));
        assert!(body.contains("# TYPE escurel_tool_latency_ms histogram"));
        assert!(body.contains("escurel_tool_latency_ms_count{"));
    }

    #[test]
    fn audit_drift_and_live_sessions_gauges_render() {
        let m = Metrics::new();
        m.set_audit_drift("acme", 3, 1);
        m.set_live_sessions(2);
        let body = m.render_prometheus();
        assert!(body.lines().any(|l| l.starts_with("escurel_audit_drift{")
            && l.contains(r#"category="markdown_not_in_index""#)
            && l.trim_end().ends_with(" 3")));
        assert!(body.contains("escurel_live_sessions_open 2"));
    }
}
