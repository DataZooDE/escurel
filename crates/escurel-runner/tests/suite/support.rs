//! Shared harness for the runner suite: one way to start a real
//! `escurel-runner` against a real gateway, and the MCP / run-control
//! helpers most files need.
//!
//! Before this existed every file carried its own `ChildGuard` and a
//! hand-rolled `Command` with the same dozen env vars, so a new runner knob
//! meant touching dozens of copies. New tests use [`RunnerProcess`], which
//! now lives in `escurel-test-support` so other crates' tests start a runner
//! the same way.

use std::time::{Duration, Instant};

use escurel_test_support::EscurelProcess;
pub use escurel_test_support::{RunnerProcess, RunningRunner};
use serde_json::{Value, json};

/// `tools/call` `name` as `token`; the structured result. Fails the test on
/// a JSON-RPC error.
pub async fn call(gw: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    let body: Value = reqwest::Client::new()
        .post(gw.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": { "name": name, "arguments": args } }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json");
    assert!(body.get("error").is_none(), "{name}: {body}");
    body["result"]["structuredContent"].clone()
}

/// A system event's JSON body.
pub fn body_of(e: &Value) -> Value {
    serde_json::from_str(e["body"].as_str().unwrap()).unwrap()
}

/// Wait until the run's `run-started` exists: the gateway authorises a
/// run-control request through it, and the runner writes it best-effort
/// right after claiming the run, so a request sent the instant `/debug/run`
/// flips to pending can precede it.
pub async fn wait_for_run_started(gw: &EscurelProcess, token: &str, run_id: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let own = call(gw, token, "list_events", json!({ "run_id": run_id })).await;
        if own["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["title"] == "run-started")
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "no run-started for {run_id}: {own}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// File an `escurel:run-control` request as `token`; its event id.
pub async fn control(gw: &EscurelProcess, token: &str, body: Value) -> String {
    let r = call(
        gw,
        token,
        "capture_event",
        json!({ "source": "workbench", "mime": "application/json",
                "label_skill": "escurel:run-control", "title": body["action"],
                "body": body.to_string() }),
    )
    .await;
    r["event_id"].as_str().unwrap().to_owned()
}

/// The `escurel:run-control-result` the runner wrote for `request_id`.
pub async fn wait_for_result(gw: &EscurelProcess, token: &str, request_id: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let r = call(
            gw,
            token,
            "list_events",
            json!({ "label_skill": "escurel:run-control-result" }),
        )
        .await;
        if let Some(e) = r["events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["provenance"]["control"]["request_event_id"] == request_id)
        {
            return e.clone();
        }
        assert!(Instant::now() < deadline, "no result for {request_id}: {r}");
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

/// What a [`GatewayProxy`] does to calls of one tool; every other request is
/// forwarded untouched.
#[derive(Clone, Copy)]
pub struct ProxyRule {
    /// The tool whose `tools/call` the rule applies to.
    pub tool: &'static str,
    /// Hold each such call this long before forwarding it.
    pub delay: Duration,
    /// Answer each such call with this HTTP status instead of forwarding it.
    pub fail_with: Option<u16>,
}

/// A stand-in in front of the real gateway, for a test that needs one tool
/// slow or failing. It also counts how many calls of that tool were in flight
/// at once, so a concurrency test measures overlap rather than wall time.
pub struct GatewayProxy {
    /// The stand-in's base URL: point `ESCUREL_RUNNER_GATEWAY_URL` here.
    pub url: String,
    max_in_flight: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl GatewayProxy {
    /// Start the stand-in, forwarding to `upstream`.
    pub async fn start(upstream: &str, rule: ProxyRule) -> Self {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        use axum::body::Bytes;
        use axum::extract::State;
        use axum::http::{HeaderMap, Method, StatusCode, Uri};
        use axum::response::IntoResponse;

        #[derive(Clone)]
        struct Shared {
            upstream: String,
            rule: ProxyRule,
            in_flight: Arc<AtomicUsize>,
            max_in_flight: Arc<AtomicUsize>,
        }

        async fn forward(
            State(s): State<Shared>,
            method: Method,
            uri: Uri,
            headers: HeaderMap,
            body: Bytes,
        ) -> axum::response::Response {
            let needle = format!("\"{}\"", s.rule.tool);
            let ruled = body.windows(needle.len()).any(|w| w == needle.as_bytes());
            if ruled {
                let now = s.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                s.max_in_flight.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(s.rule.delay).await;
            }
            let resp = match (ruled, s.rule.fail_with) {
                (true, Some(status)) => (
                    StatusCode::from_u16(status).unwrap(),
                    "the stand-in fails this call",
                )
                    .into_response(),
                _ => {
                    let mut req = reqwest::Client::new().request(
                        reqwest::Method::from_bytes(method.as_str().as_bytes()).unwrap(),
                        format!(
                            "{}{}",
                            s.upstream,
                            uri.path_and_query().map_or("/", |p| p.as_str())
                        ),
                    );
                    for name in ["authorization", "content-type", "accept"] {
                        if let Some(v) = headers.get(name).and_then(|v| v.to_str().ok()) {
                            req = req.header(name, v);
                        }
                    }
                    match req.body(body.to_vec()).send().await {
                        Ok(resp) => {
                            let status = StatusCode::from_u16(resp.status().as_u16()).unwrap();
                            let ctype = resp
                                .headers()
                                .get("content-type")
                                .and_then(|v| v.to_str().ok())
                                .unwrap_or("application/json")
                                .to_owned();
                            let bytes = resp.bytes().await.unwrap_or_default();
                            (status, [("content-type", ctype)], bytes.to_vec()).into_response()
                        }
                        Err(e) => (StatusCode::BAD_GATEWAY, e.to_string()).into_response(),
                    }
                }
            };
            if ruled {
                s.in_flight.fetch_sub(1, Ordering::SeqCst);
            }
            resp
        }

        let max_in_flight = Arc::new(AtomicUsize::new(0));
        let shared = Shared {
            upstream: upstream.trim_end_matches('/').to_owned(),
            rule,
            in_flight: Arc::new(AtomicUsize::new(0)),
            max_in_flight: max_in_flight.clone(),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = axum::Router::new().fallback(forward).with_state(shared);
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            url: format!("http://{addr}"),
            max_in_flight,
        }
    }

    /// The most calls of the rule's tool that were in flight at once.
    pub fn max_in_flight(&self) -> usize {
        self.max_in_flight.load(std::sync::atomic::Ordering::SeqCst)
    }
}
