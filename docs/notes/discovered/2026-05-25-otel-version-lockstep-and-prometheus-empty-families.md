# OTel crate version-lockstep + Prometheus empty-family rendering

Discovered while building `escurel-obs` (M5).

## Symptom 1 — OTel crates must move as a matched set

The OpenTelemetry Rust crates do **not** share a version with each
other linearly, and `tracing-opentelemetry` carries its own number that
lags the core crates by one. Mixing them (e.g. `opentelemetry 0.32`
with `tracing-opentelemetry 0.31`) produces obscure trait-mismatch
errors at the `layer().with_tracer(...)` call site, because the
`opentelemetry::trace::Tracer` impl referenced by the bridge is from a
different `opentelemetry` semver and is therefore a *different type*.

### The matched set that resolves today (2026-05-25)

```toml
opentelemetry        = "0.32"
opentelemetry_sdk    = { version = "0.32", features = ["rt-tokio"] }
opentelemetry-otlp   = { version = "0.32", features = ["grpc-tonic", "trace"] }
tracing-opentelemetry = "0.33"   # pairs with the 0.32 core set
```

### How to recognise it next time

If the trace layer won't typecheck after a bump, check that all three
`opentelemetry*` crates are on the *same* minor and that
`tracing-opentelemetry` is exactly one minor ahead. The
`tracing-opentelemetry` changelog states which `opentelemetry` minor it
targets — trust that over guessing.

Also note the SDK 0.32 builder API: `SdkTracerProvider::builder()
.with_batch_exporter(exporter).with_resource(Resource::builder()
.with_service_name(...).build()).build()`, and `provider.shutdown()`
(not `force_flush` / global shutdown) for the drop-guard flush.

## Symptom 2 — Prometheus `*Vec` families render nothing until touched

The `prometheus` crate's `IntCounterVec` / `HistogramVec` emit **no**
HELP/TYPE/sample lines from `TextEncoder` until at least one label
combination has been observed. A freshly-constructed `Metrics` therefore
renders only the plain `escurel_up` gauge. This is correct Prometheus
exposition behaviour (a label-vec with no series is empty), but it will
surprise a test that asserts the families are present on an untouched
registry — touch each family first, or assert presence only after
traffic. `escurel-obs` chose the `prometheus` crate over the
`metrics` + `metrics-exporter-prometheus` pair specifically because it
exposes a per-instance `Registry` (no process-global recorder), which
keeps `Metrics` test-isolatable.

## Symptom 3 — global tracing subscriber is install-once

`init_telemetry` installs a process-global subscriber and can run at
most once per process. Tests that need the JSON formatter in isolation
build the layer via the exposed `json_log_layer(&cfg, writer)` and
install it with `tracing::subscriber::with_default(...)` (a scoped
guard), feeding a custom in-memory `MakeWriter` so they read back the
real serialised bytes. No mock of the formatter.
