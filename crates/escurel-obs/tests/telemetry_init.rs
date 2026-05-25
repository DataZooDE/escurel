//! Real-layer exercises for the JSON tracing subscriber.
//!
//! A global tracing subscriber can only be installed once per process,
//! so these tests do NOT call the global installer. Instead they build
//! the same JSON layer via [`escurel_obs::json_log_layer`] and install
//! it with `tracing::subscriber::with_default` — a scoped guard that is
//! isolated per test. The layer writes into a real in-test buffer via a
//! real `MakeWriter`, so we exercise the actual formatter (no mock).

use std::io;
use std::sync::{Arc, Mutex};

use escurel_obs::{TelemetryConfig, init_telemetry, json_log_layer};
use tracing_subscriber::layer::SubscriberExt;

/// A `MakeWriter` that appends every byte to a shared buffer so the
/// test can read back exactly what the layer serialised.
#[derive(Clone)]
struct BufWriter(Arc<Mutex<Vec<u8>>>);

impl io::Write for BufWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for BufWriter {
    type Writer = BufWriter;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

fn test_cfg() -> TelemetryConfig {
    TelemetryConfig {
        app: "escurel".to_string(),
        env: "nonprod".to_string(),
        version: "1.2.3".to_string(),
        otlp_endpoint: None,
        json_logs: true,
    }
}

#[test]
fn init_with_json_logs_emits_structured_line_to_stdout() {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let writer = BufWriter(buf.clone());
    let layer = json_log_layer(&test_cfg(), writer);

    tracing::subscriber::with_default(tracing_subscriber::registry().with(layer), || {
        tracing::info!(msg = "tool.completed", "tool.completed");
    });

    let raw = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    let line = raw.lines().next().expect("at least one log line");
    let v: serde_json::Value = serde_json::from_str(line).expect("valid JSON line");

    assert_eq!(v["app"], "escurel");
    assert_eq!(v["env"], "nonprod");
    assert_eq!(v["version"], "1.2.3");
    assert_eq!(v["level"], "info");
    assert_eq!(v["msg"], "tool.completed");
    assert!(v.get("ts").is_some(), "missing ts field: {line}");
    // ts must be RFC3339-ish (contains the date separator + Z/offset).
    let ts = v["ts"].as_str().unwrap();
    assert!(ts.contains('T'), "ts not RFC3339: {ts}");
}

#[tokio::test]
async fn init_without_otlp_endpoint_is_noop_for_traces() {
    // With otlp_endpoint = None the global installer must succeed and
    // produce a guard with no exporter task; dropping it is clean.
    let guard = init_telemetry(test_cfg()).expect("init succeeds without OTLP");
    assert!(
        !guard.has_otlp(),
        "no exporter expected when endpoint unset"
    );
    drop(guard);
}

#[test]
fn request_id_field_appears_when_set_in_span() {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let writer = BufWriter(buf.clone());
    let layer = json_log_layer(&test_cfg(), writer);

    tracing::subscriber::with_default(tracing_subscriber::registry().with(layer), || {
        let span = tracing::info_span!("request", request_id = "req-abc-123");
        let _g = span.enter();
        tracing::info!("handling");
    });

    let raw = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    let line = raw.lines().next().expect("at least one log line");
    let v: serde_json::Value = serde_json::from_str(line).expect("valid JSON line");

    // The span field must be flattened onto the record (substrate
    // collectors key off a top-level `request_id`).
    assert_eq!(
        v["request_id"], "req-abc-123",
        "request_id not present on record: {line}"
    );
}
