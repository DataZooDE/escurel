//! Prometheus metrics registry the gateway increments and renders.
//!
//! Each [`Metrics`] owns its own `prometheus::Registry`; there is no
//! process-global recorder. This keeps the type test-isolatable and
//! lets the gateway hold exactly one instance behind an `Arc`.

use prometheus::{
    Encoder, Gauge, HistogramOpts, HistogramVec, IntCounterVec, Opts, Registry, TextEncoder,
};

/// Latency histogram buckets, in seconds. Chosen to straddle the
/// tail-latency budget in `platform.md` (sub-ms reads up to slow
/// writes near 1 s).
const LATENCY_BUCKETS: &[f64] = &[
    0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5,
];

/// A metrics registry the gateway increments; renders Prometheus text.
///
/// Cloneable handles are not provided — wrap in `Arc` to share.
pub struct Metrics {
    registry: Registry,
    requests: IntCounterVec,
    latency: HistogramVec,
    up: Gauge,
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

        registry
            .register(Box::new(requests.clone()))
            .expect("register escurel_requests_total");
        registry
            .register(Box::new(latency.clone()))
            .expect("register escurel_request_latency_seconds");
        registry
            .register(Box::new(up.clone()))
            .expect("register escurel_up");

        Self {
            registry,
            requests,
            latency,
            up,
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
