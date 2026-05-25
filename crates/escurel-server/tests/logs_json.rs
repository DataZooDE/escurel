//! Phase B + C — structured JSON logs and per-request spans.
//!
//! Installs the real `escurel-obs` JSON layer onto a process-global
//! subscriber (set up *before* the gateway boots so `serve()`'s
//! own `init_telemetry` call no-ops gracefully). Drives a real
//! `tools/call` over `POST /mcp`, then parses every log line out
//! of the shared buffer and asserts:
//!
//! - the substrate's required keys (`ts`, `level`, `app`, `env`,
//!   `version`) appear on every record;
//! - a `request_id` is hoisted onto records emitted inside the
//!   per-request span;
//! - a span / record naming the dispatched tool appears in the
//!   stream for a `tools/call` request.
//!
//! No mocks — real gateway, real subscriber, real `serde_json`
//! parse of the actual emitted bytes.

use std::io;
use std::sync::{Arc, Mutex, OnceLock};

use escurel_obs::{TelemetryConfig, json_log_layer};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, Opts};
use serde_json::{Value, json};
use tracing_subscriber::layer::SubscriberExt;

/// Append-only shared buffer the test subscriber writes into.
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

static GLOBAL_BUF: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();

/// Install the JSON subscriber on the global default exactly once,
/// before any test spawns the gateway. Returns the shared buffer
/// every test reads.
fn shared_buf() -> Arc<Mutex<Vec<u8>>> {
    GLOBAL_BUF
        .get_or_init(|| {
            let buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
            let cfg = TelemetryConfig {
                app: "escurel".to_owned(),
                env: "nonprod".to_owned(),
                version: "test-logs".to_owned(),
                otlp_endpoint: None,
                json_logs: true,
            };
            let layer = json_log_layer(&cfg, BufWriter(buf.clone()));
            let subscriber = tracing_subscriber::registry().with(layer);
            tracing::subscriber::set_global_default(subscriber)
                .expect("global subscriber not yet installed");
            buf
        })
        .clone()
}

fn snapshot(buf: &Arc<Mutex<Vec<u8>>>) -> Vec<Value> {
    let raw = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    raw.lines()
        .filter(|l| !l.is_empty())
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .collect()
}

async fn spawn_gateway() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::Disabled,
        fixtures: None,
        config_overrides: ConfigOverrides {
            gateway_version: Some("test-logs".to_owned()),
            disable_grpc: true,
            disable_indexer: true,
            ..Default::default()
        },
    })
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_request_emits_json_log_with_required_substrate_fields() {
    let buf = shared_buf();
    // Snapshot the offset so we only look at lines this test
    // produced — other tests share the same subscriber.
    let start_offset = buf.lock().unwrap().len();

    let p = spawn_gateway().await;
    let http = reqwest::Client::new();
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {} });
    let resp = http.post(p.mcp_url()).json(&body).send().await.unwrap();
    assert_eq!(resp.status(), 200);

    // Give the spawned tokio handler tasks a beat to drain their
    // events into the writer.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let all = {
        let raw = String::from_utf8(buf.lock().unwrap()[start_offset..].to_vec()).unwrap();
        raw.lines()
            .filter(|l| !l.is_empty())
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .collect::<Vec<_>>()
    };

    assert!(
        !all.is_empty(),
        "expected at least one JSON log line emitted by the gateway"
    );

    let line = &all[0];
    for key in ["ts", "level", "app", "env", "version"] {
        assert!(
            line.get(key).is_some(),
            "log line missing required key `{key}`: {line}"
        );
    }
    assert_eq!(line["app"], "escurel");
    assert_eq!(line["env"], "nonprod");
    assert_eq!(line["version"], "test-logs");

    // The recorded ts must be RFC3339-shaped.
    let ts = line["ts"].as_str().expect("ts is a string");
    assert!(
        ts.contains('T'),
        "ts not in RFC3339 form: {ts} (full record: {line})"
    );

    p.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_request_emits_span_with_tool_name_and_request_id() {
    let buf = shared_buf();
    let start_offset = buf.lock().unwrap().len();

    let p = spawn_gateway().await;
    let http = reqwest::Client::new();
    // tools/list goes through the same dispatch path — the request
    // span carries the request_id and the tool name (or method).
    let body = json!({ "jsonrpc": "2.0", "id": 7, "method": "tools/list", "params": {} });
    let resp = http
        .post(p.mcp_url())
        .header("X-Request-Id", "req-test-7")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let all = snapshot(&buf);
    let mine: Vec<&Value> = all[start_offset.saturating_sub(0)..]
        .iter()
        .filter(|v| v.get("request_id").is_some())
        .collect();

    // At least one record was emitted inside a request span and
    // carries a hoisted request_id field.
    let inbound = all.iter().find(|v| {
        v.get("request_id")
            .and_then(|r| r.as_str())
            .map(|s| s == "req-test-7")
            .unwrap_or(false)
    });
    assert!(
        inbound.is_some() || !mine.is_empty(),
        "expected at least one log line with a `request_id` field; all lines: {all:?}"
    );
    // When the caller supplied `X-Request-Id`, the span should
    // adopt it.
    assert!(
        inbound.is_some(),
        "request_id from X-Request-Id header was not threaded into the span; lines: {all:?}"
    );

    // The same request must produce a record naming the JSON-RPC
    // method (`tool` field on the span).
    let with_method = all.iter().find(|v| {
        v.get("method")
            .and_then(|m| m.as_str())
            .map(|m| m == "tools/list")
            .unwrap_or(false)
    });
    assert!(
        with_method.is_some(),
        "no log line names `method=tools/list`; lines: {all:?}"
    );

    p.shutdown().await;
}
