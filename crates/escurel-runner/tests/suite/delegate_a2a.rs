//! No-mock control-channel test for the A2A `DelegateHarness` (async-ops
//! Phase 4 slice 3c).
//!
//! escurel is the orchestrator; a `harness: delegate` step hands the domain
//! work to the agent over A2A and waits for a result reference. Here a real
//! `DelegateHarness` speaks JSON-RPC 2.0 (`message/send` + `tasks/get`) to a
//! REAL local HTTP server standing in for the agent's A2A endpoint — no mock of
//! the harness, the transport, or the lifecycle. It asserts:
//!
//! - a delegated task polls through a non-terminal state to `completed` and the
//!   agent's `metadata.result_ref` comes back on the [`HarnessOutcome`], with
//!   the delegation bearer + the requested capability reaching the agent;
//! - a `failed` task is reported as a harness failure (not an adapter error),
//!   carrying no result;
//! - a task routed to the delegate harness with NO delegation parameters fails
//!   closed (`Unsupported`), rather than delegating with no endpoint/authority.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use escurel_client::SecretString;
use escurel_runner_core::{Delegation, TaskContext};
use escurel_runner_harness::{DelegateHarness, Harness, HarnessError, HarnessStatus};
use serde_json::{Value, json};

/// What the stub agent recorded about the delegation it was handed.
#[derive(Default)]
struct Seen {
    authorization: Option<String>,
    capability: Option<String>,
    input_text: Option<String>,
    methods: Vec<String>,
}

/// A stub agent A2A endpoint. `message/send` returns a `working` task; the
/// first `tasks/get` still reports `working` (proving the poll loop iterates on
/// a non-terminal state), and the next reports `completed` with the given
/// `result_ref` in its metadata. When `fail` is set, the task ends `failed`.
async fn spawn_stub_agent(result_ref: Value, fail: bool, seen: Arc<Mutex<Seen>>) -> String {
    use axum::{Router, extract::State};

    #[derive(Clone)]
    struct St {
        result_ref: Value,
        fail: bool,
        seen: Arc<Mutex<Seen>>,
        polls: Arc<AtomicUsize>,
    }

    async fn rpc(
        State(st): State<St>,
        headers: axum::http::HeaderMap,
        body: String,
    ) -> axum::Json<Value> {
        let req: Value = serde_json::from_str(&body).expect("agent got JSON");
        let method = req["method"].as_str().unwrap_or_default().to_owned();
        {
            let mut s = st.seen.lock().expect("lock");
            s.methods.push(method.clone());
            if method == "message/send" {
                s.authorization = headers
                    .get(axum::http::header::AUTHORIZATION)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned);
                s.capability = req["params"]["metadata"]["capability"]
                    .as_str()
                    .map(str::to_owned);
                s.input_text = req["params"]["message"]["parts"][0]["text"]
                    .as_str()
                    .map(str::to_owned);
            }
        }

        let task_id = "task-1";
        let state = if method == "message/send" {
            "working"
        } else {
            // tasks/get: stay `working` once, then terminate.
            let n = st.polls.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                "working"
            } else if st.fail {
                "failed"
            } else {
                "completed"
            }
        };

        let mut task = json!({
            "id": task_id,
            "status": { "state": state },
        });
        if state == "completed" {
            task["metadata"] = json!({ "result_ref": st.result_ref });
        }
        axum::Json(json!({ "jsonrpc": "2.0", "id": 1, "result": task }))
    }

    let app = Router::new().fallback(rpc).with_state(St {
        result_ref,
        fail,
        seen,
        polls: Arc::new(AtomicUsize::new(0)),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stub agent");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}/a2a")
}

/// A delegate task pointed at `a2a_url` asking for `capability` with `input`.
fn delegate_task(a2a_url: String, capability: &str, input: &str) -> TaskContext {
    let delegation = Delegation::new(
        a2a_url,
        capability.to_owned(),
        SecretString::from("delegation-bearer-xyz".to_owned()),
    );
    TaskContext::for_test(
        "instructions".to_owned(),
        input.to_owned(),
        "http://gw/mcp".to_owned(),
        vec![],
        SecretString::from("escurel-scoped-token".to_owned()),
    )
    .with_delegation(delegation)
}

#[tokio::test]
async fn a_delegated_task_polls_to_completed_and_returns_the_result_ref() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let expected_ref = json!({ "kind": "result_table", "producer": "scenario", "id": "r1" });
    let url = spawn_stub_agent(expected_ref.clone(), false, seen.clone()).await;

    let harness =
        DelegateHarness::new().with_timeouts(Duration::from_millis(10), Duration::from_secs(5));
    let task = delegate_task(url, "scenario", "run the port-strike what-if");
    let outcome = harness.run(&task).await.expect("delegate run");

    assert!(outcome.ok, "a completed delegated task is ok");
    assert_eq!(outcome.status, HarnessStatus::Ok);
    assert_eq!(
        outcome.result_ref.as_ref(),
        Some(&expected_ref),
        "the agent's metadata.result_ref reaches the outcome verbatim"
    );

    let s = seen.lock().expect("lock");
    assert_eq!(
        s.authorization.as_deref(),
        Some("Bearer delegation-bearer-xyz"),
        "the delegation token authenticates the A2A call"
    );
    assert_eq!(s.capability.as_deref(), Some("scenario"));
    assert_eq!(s.input_text.as_deref(), Some("run the port-strike what-if"));
    // message/send + at least two tasks/get (one non-terminal, one terminal).
    assert_eq!(s.methods.first().map(String::as_str), Some("message/send"));
    assert!(
        s.methods.iter().filter(|m| *m == "tasks/get").count() >= 2,
        "the loop polled a non-terminal task before it completed: {:?}",
        s.methods
    );
}

#[tokio::test]
async fn a_failed_delegated_task_reports_harness_failure_not_an_adapter_error() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let url = spawn_stub_agent(Value::Null, /*fail=*/ true, seen).await;

    let harness =
        DelegateHarness::new().with_timeouts(Duration::from_millis(10), Duration::from_secs(5));
    let task = delegate_task(url, "scenario", "run it");
    let outcome = harness
        .run(&task)
        .await
        .expect("a failed task is a clean outcome, not an adapter error");

    assert!(!outcome.ok);
    assert_eq!(outcome.status, HarnessStatus::Failed);
    assert!(
        outcome.result_ref.is_none(),
        "a failed task names no result"
    );
}

#[tokio::test]
async fn a_task_without_delegation_params_is_unsupported() {
    let harness = DelegateHarness::new();
    // A plain task (no `.with_delegation`) — as if a non-delegate step were
    // routed here by a mis-configuration.
    let task = TaskContext::for_test(
        "i".to_owned(),
        "in".to_owned(),
        "http://gw/mcp".to_owned(),
        vec![],
        SecretString::from("t".to_owned()),
    );
    let err = harness
        .run(&task)
        .await
        .expect_err("no delegation params must fail closed");
    assert!(
        matches!(
            err,
            HarnessError::Unsupported {
                harness: "delegate",
                ..
            }
        ),
        "expected Unsupported, got {err:?}"
    );
}

#[tokio::test]
async fn a_hung_agent_endpoint_times_out_instead_of_blocking_forever() {
    // A server that ACCEPTS connections but never responds — the #569 hang
    // shape. Without a per-request timeout, `send()` would block forever.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind hung server");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((sock, _)) = listener.accept().await {
            held.push(sock); // hold the socket open, never reply
        }
    });

    let harness = DelegateHarness::new()
        .with_timeouts(Duration::from_millis(10), Duration::from_secs(30))
        .with_request_timeout(Duration::from_millis(400));
    let task = delegate_task(format!("http://{addr}/a2a"), "scenario", "run it");

    let started = std::time::Instant::now();
    let err = harness
        .run(&task)
        .await
        .expect_err("a hung endpoint must error, not hang");
    let elapsed = started.elapsed();

    assert!(
        matches!(
            err,
            HarnessError::Timeout {
                harness: "delegate",
                ..
            }
        ),
        "expected Timeout, got {err:?}"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "must fail near the per-request timeout, took {elapsed:?}"
    );
}
