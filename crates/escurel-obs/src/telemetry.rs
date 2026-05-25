//! Telemetry init: a JSON (or pretty) stdout subscriber matching the
//! substrate log contract, plus an optional OTLP trace exporter.

use std::io;

use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::WithExportConfig as _;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing::Subscriber;
use tracing_subscriber::Layer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;

use crate::log_fields::{JsonContract, SpanFieldLayer, StaticFields};

/// Configuration for [`init_telemetry`].
pub struct TelemetryConfig {
    /// Logical app name stamped on every log record (e.g. `"escurel"`).
    pub app: String,
    /// Deployment environment: `"nonprod" | "prod" | "dev"`.
    pub env: String,
    /// Build version string.
    pub version: String,
    /// OTLP/gRPC endpoint. `None` → traces are a no-op (dev/laptop).
    pub otlp_endpoint: Option<String>,
    /// `true` → JSON logs (production); `false` → pretty logs (dev).
    pub json_logs: bool,
}

/// Errors raised while installing telemetry.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A global subscriber was already installed in this process.
    #[error("a global tracing subscriber is already installed: {0}")]
    AlreadyInstalled(String),
    /// The OTLP exporter could not be built.
    #[error("failed to build OTLP exporter: {0}")]
    Otlp(String),
}

/// Dropped on shutdown to flush the OTLP exporter. Holding `None` for
/// the provider means traces were a no-op.
pub struct TelemetryGuard {
    provider: Option<SdkTracerProvider>,
}

impl TelemetryGuard {
    /// Whether an OTLP exporter is active (false in the no-op case).
    pub fn has_otlp(&self) -> bool {
        self.provider.is_some()
    }
}

impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        if let Some(provider) = self.provider.take() {
            // Best-effort flush; shutdown errors are not actionable here.
            let _ = provider.shutdown();
        }
    }
}

/// Build the JSON log layer matching the substrate contract, writing to
/// `writer`. Exposed for tests so they can install a scoped subscriber
/// with an in-memory `MakeWriter` and exercise the real formatter.
///
/// The returned layer also captures span fields (e.g. `request_id`) so
/// they are flattened onto child event records.
pub fn json_log_layer<S, W>(cfg: &TelemetryConfig, writer: W) -> Box<dyn Layer<S> + Send + Sync>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    W: for<'a> tracing_subscriber::fmt::MakeWriter<'a> + Send + Sync + 'static,
{
    let statics = StaticFields {
        app: cfg.app.clone(),
        env: cfg.env.clone(),
        version: cfg.version.clone(),
    };
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_writer(writer)
        .event_format(JsonContract { statics });

    // SpanFieldLayer must run so span fields are stashed for the
    // formatter to hoist onto records.
    Box::new(SpanFieldLayer.and_then(fmt_layer))
}

/// Build the global subscriber and install it. Returns a guard that
/// flushes the OTLP exporter (if any) on drop.
///
/// This installs a **process-global** subscriber and may be called at
/// most once per process. Tests that need the formatter in isolation
/// use [`json_log_layer`] with `with_default` instead.
pub fn init_telemetry(cfg: TelemetryConfig) -> Result<TelemetryGuard, Error> {
    let statics = StaticFields {
        app: cfg.app.clone(),
        env: cfg.env.clone(),
        version: cfg.version.clone(),
    };

    // Optional OTLP trace pipeline.
    let (otel_layer, provider) = match &cfg.otlp_endpoint {
        Some(endpoint) => {
            let exporter = opentelemetry_otlp::SpanExporter::builder()
                .with_tonic()
                .with_endpoint(endpoint.clone())
                .build()
                .map_err(|e| Error::Otlp(e.to_string()))?;
            let resource = Resource::builder()
                .with_service_name(cfg.app.clone())
                .build();
            let provider = SdkTracerProvider::builder()
                .with_batch_exporter(exporter)
                .with_resource(resource)
                .build();
            let tracer = provider.tracer(cfg.app.clone());
            let layer = tracing_opentelemetry::layer().with_tracer(tracer);
            (Some(layer), Some(provider))
        }
        None => (None, None),
    };

    let registry = tracing_subscriber::registry()
        .with(SpanFieldLayer)
        .with(otel_layer);

    let install = if cfg.json_logs {
        let fmt_layer = tracing_subscriber::fmt::layer()
            .with_writer(io::stdout)
            .event_format(JsonContract { statics });
        registry.with(fmt_layer).try_init()
    } else {
        let fmt_layer = tracing_subscriber::fmt::layer().with_writer(io::stdout);
        registry.with(fmt_layer).try_init()
    };

    install.map_err(|e| Error::AlreadyInstalled(e.to_string()))?;

    Ok(TelemetryGuard { provider })
}
