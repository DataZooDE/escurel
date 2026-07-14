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
    /// `escurel_runner_runs_total{tenant,status}` — agent-runner runs by
    /// terminal status (`processed`/`failed`/`dead_letter`/`converged`).
    runner_runs: IntCounterVec,
    /// Confirmed page writes by origin (WI-6 absorption instrumentation).
    writes: IntCounterVec,
    /// `escurel_runner_throttled_total{reason}` — quota throttles by reason.
    runner_throttled: IntCounterVec,
    /// `escurel_runner_queue_depth` — current dispatch-queue depth.
    runner_queue_depth: IntGauge,
    /// `escurel_runner_cascade_depth_max` — deepest cascade hop seen.
    runner_cascade_depth_max: IntGauge,
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

        let runner_runs = IntCounterVec::new(
            Opts::new(
                "escurel_runner_runs_total",
                "Agent-runner runs by tenant and terminal status.",
            ),
            &["tenant", "status"],
        )
        .expect("valid counter opts");

        let writes = IntCounterVec::new(
            Opts::new(
                "escurel_writes_total",
                "Confirmed page writes by tenant and origin (human | runner) — \
                 the L2 absorption signal (WI-6): the runner/human ratio over \
                 time is the interlocked-loops convergence curve.",
            ),
            &["tenant", "origin"],
        )
        .expect("valid counter opts");

        let runner_throttled = IntCounterVec::new(
            Opts::new(
                "escurel_runner_throttled_total",
                "Agent-runner triggers throttled by quota, by reason.",
            ),
            &["reason"],
        )
        .expect("valid counter opts");

        let runner_queue_depth = IntGauge::with_opts(Opts::new(
            "escurel_runner_queue_depth",
            "Current agent-runner dispatch-queue depth.",
        ))
        .expect("valid gauge opts");

        let runner_cascade_depth_max = IntGauge::with_opts(Opts::new(
            "escurel_runner_cascade_depth_max",
            "Deepest cascade hop the agent-runner has processed.",
        ))
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
        registry
            .register(Box::new(runner_runs.clone()))
            .expect("register escurel_runner_runs_total");
        registry
            .register(Box::new(writes.clone()))
            .expect("register writes");
        registry
            .register(Box::new(runner_throttled.clone()))
            .expect("register escurel_runner_throttled_total");
        registry
            .register(Box::new(runner_queue_depth.clone()))
            .expect("register escurel_runner_queue_depth");
        registry
            .register(Box::new(runner_cascade_depth_max.clone()))
            .expect("register escurel_runner_cascade_depth_max");

        Self {
            registry,
            requests,
            latency,
            up,
            tool_calls,
            tool_latency_ms,
            live_sessions,
            audit_drift,
            runner_runs,
            writes,
            runner_throttled,
            runner_queue_depth,
            runner_cascade_depth_max,
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

    /// Record one agent-runner run reaching a terminal `status`
    /// (`processed` / `failed` / `dead_letter` / `converged`) for `tenant`.
    /// Count one CONFIRMED page write (WI-6): `origin` is `"runner"`
    /// when the write carried runner/workflow provenance, else
    /// `"human"`. Refused writes never count.
    pub fn inc_write(&self, tenant: &str, origin: &str) {
        self.writes.with_label_values(&[tenant, origin]).inc();
    }

    pub fn inc_runner_run(&self, tenant: &str, status: &str) {
        self.runner_runs.with_label_values(&[tenant, status]).inc();
    }

    /// Record one quota throttle, by `reason` (`runs_per_min` /
    /// `max_concurrent`).
    pub fn inc_runner_throttled(&self, reason: &str) {
        self.runner_throttled.with_label_values(&[reason]).inc();
    }

    /// Set the current dispatch-queue depth gauge.
    pub fn set_runner_queue_depth(&self, depth: i64) {
        self.runner_queue_depth.set(depth);
    }

    /// Bump the max-cascade-depth gauge if `depth` exceeds the last high-water.
    pub fn observe_runner_cascade_depth(&self, depth: i64) {
        if depth > self.runner_cascade_depth_max.get() {
            self.runner_cascade_depth_max.set(depth);
        }
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
    fn runner_metrics_render() {
        let m = Metrics::new();
        m.inc_runner_run("acme", "processed");
        m.inc_runner_run("acme", "dead_letter");
        m.inc_runner_throttled("runs_per_min");
        m.set_runner_queue_depth(3);
        m.observe_runner_cascade_depth(2);
        m.observe_runner_cascade_depth(1); // does not lower the high-water
        let body = m.render_prometheus();
        assert!(
            body.lines()
                .any(|l| l.starts_with("escurel_runner_runs_total{")
                    && l.contains(r#"tenant="acme""#)
                    && l.contains(r#"status="dead_letter""#)
                    && l.trim_end().ends_with(" 1"))
        );
        assert!(
            body.lines()
                .any(|l| l.starts_with("escurel_runner_throttled_total{")
                    && l.contains(r#"reason="runs_per_min""#))
        );
        assert!(body.contains("escurel_runner_queue_depth 3"));
        assert!(
            body.contains("escurel_runner_cascade_depth_max 2"),
            "cascade depth gauge keeps the high-water mark"
        );
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
