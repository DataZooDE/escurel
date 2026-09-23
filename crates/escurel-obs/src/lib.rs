//! Observability for Escurel: structured JSON logs matching the
//! substrate contract, an optional OTLP trace exporter, and a
//! Prometheus metrics registry rendered as text.
//!
//! See `docs/spec/platform.md` §Observability for the field contract
//! (`ts`, `level`, `msg`, `app`, `env`, `version`, `request_id`) and
//! the metric families.

mod log_fields;
mod metrics;
mod telemetry;
mod trace_ctx;

pub use metrics::Metrics;
pub use telemetry::{Error, TelemetryConfig, TelemetryGuard, init_telemetry, json_log_layer};
pub use trace_ctx::{OpenTelemetrySpanExt, attach_run_trace, remote_run_context};
