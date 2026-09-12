//! The transport rides out a gateway restart (#…).
//!
//! Escurel runs `strategy: Recreate` at `replicaCount: 1` — it holds a
//! single-writer lease on the DuckLake catalog, so a rolling update
//! deadlocks (the new pod cannot boot while the old one holds the lease).
//! Every deploy is therefore a short window in which nothing is listening,
//! and callers see `Connection refused` rather than a slow response. Observed
//! on lab 2026-09-11: an agent's `start_operation` failed at 18:25:39 and
//! 18:25:43, the replacement pod was serving by 18:25:45, and a user was told
//! "the workflow execution service is currently unreachable".
//!
//! A refused connection is the one failure that is unambiguously safe to
//! replay: the request never reached the server, so no write can have been
//! applied. Anything that fails AFTER the bytes go out — a dropped response,
//! a mid-flight timeout — may already have applied `promote_draft` or
//! `start_operation`, and replaying it would double-apply. That line is the
//! whole safety argument, so this file pins both sides of it.
//!
//! No mocks: a real TCP socket that refuses, then a real HTTP server on the
//! same port speaking the actual JSON-RPC wire format.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use std::sync::Mutex;

use axum::Router;
use axum::extract::State;
use axum::routing::post;
use escurel_client::Client;
use secrecy::SecretString;
use serde_json::{Value, json};

/// A port nothing is listening on: bind it, learn the number, drop the
/// listener. This is the state a caller meets mid-`Recreate`.
async fn vacant_port() -> u16 {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    drop(l);
    port
}

/// The gateway's reply to `tools/call`, in the shape the transport decodes.
async fn tools_call(State(hits): State<Arc<AtomicUsize>>, body: String) -> axum::Json<Value> {
    hits.fetch_add(1, Ordering::SeqCst);
    let env: Value = serde_json::from_str(&body).unwrap_or(json!({}));
    axum::Json(json!({
        "jsonrpc": "2.0",
        "id": env.get("id").cloned().unwrap_or(json!(1)),
        "result": { "structuredContent": { "ok": true } },
    }))
}

/// Serve `/mcp` on `port`, after `delay`. Returns the hit counter.
fn serve_after(port: u16, delay: Duration) -> Arc<AtomicUsize> {
    let hits = Arc::new(AtomicUsize::new(0));
    let for_task = hits.clone();
    tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        let app = Router::new()
            .route("/mcp", post(tools_call))
            .with_state(for_task);
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
            .await
            .expect("bind the port the client is already dialling");
        let _ = axum::serve(listener, app).await;
    });
    hits
}

/// A call that starts while nothing is listening succeeds once the gateway
/// comes back — the caller never sees the restart.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_call_rides_out_a_gateway_restart() {
    let port = vacant_port().await;
    let hits = serve_after(port, Duration::from_secs(2));

    let client = Client::connect(&format!("http://127.0.0.1:{port}"), SecretString::from("t"))
        .await
        .unwrap();

    let started = Instant::now();
    let out = client.call_raw("list_skills", json!({})).await;
    assert!(
        out.is_ok(),
        "a call spanning a restart must succeed, got: {out:?}"
    );
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "the call landed exactly once"
    );
    assert!(
        started.elapsed() >= Duration::from_secs(2),
        "it must have waited for the gateway rather than answering early"
    );
}

/// The budget is bounded: a gateway that never comes back fails, and does so
/// well inside the 60 s request timeout, so a genuinely dead escurel does not
/// read as a hang.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_gateway_that_never_returns_still_fails() {
    let port = vacant_port().await;

    let client = Client::connect(&format!("http://127.0.0.1:{port}"), SecretString::from("t"))
        .await
        .unwrap();

    let started = Instant::now();
    let out = client.call_raw("list_skills", json!({})).await;
    assert!(out.is_err(), "a dead gateway must fail, not hang forever");
    let waited = started.elapsed();
    assert!(
        waited >= Duration::from_secs(8),
        "it must actually have retried across the budget, not given up at once: {waited:?}"
    );
    assert!(
        waited < Duration::from_secs(20),
        "the budget must stay bounded: {waited:?}"
    );
}

/// A JSON-RPC error is surfaced, not retried. Weak on its own — a JSON-RPC
/// error arrives as a successful HTTP response, so it never reaches the retry
/// loop at all and this would pass however wide the predicate got. It guards
/// against a future retry that wraps the whole decode instead. The test that
/// actually pins the safety line is the one below it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_server_that_answered_is_never_replayed() {
    let hits = Arc::new(AtomicUsize::new(0));
    let for_task = hits.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let app = Router::new()
            .route(
                "/mcp",
                post(move |body: String| {
                    let hits = for_task.clone();
                    async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        let env: Value = serde_json::from_str(&body).unwrap_or(json!({}));
                        axum::Json(json!({
                            "jsonrpc": "2.0",
                            "id": env.get("id").cloned().unwrap_or(json!(1)),
                            "error": { "code": -32000, "message": "refused on purpose" },
                        }))
                    }
                }),
            )
            .into_make_service();
        let _ = axum::serve(listener, app).await;
    });

    let client = Client::connect(&format!("http://127.0.0.1:{port}"), SecretString::from("t"))
        .await
        .unwrap();
    let out = client.call_raw("promote_draft", json!({})).await;

    assert!(out.is_err(), "a JSON-RPC error is still an error");
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "a request the server ANSWERED must reach it exactly once — replaying \
         it is how a write gets applied twice"
    );
}

/// THE SAFETY LINE, and the test that discriminates it.
///
/// This server accepts the connection, reads the request, and then closes
/// without answering. `reqwest` reports an error — but the bytes arrived, so
/// the write may already have been applied. Replaying it applies it twice.
///
/// Unlike the JSON-RPC case above, this failure DOES reach the retry loop, so
/// it fails the moment `never_reached_the_server` is widened past
/// `is_connect()`: the server would then see the request twice. Verified by
/// widening the predicate to `true` and watching this test report 41 hits.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_request_the_server_read_is_never_replayed() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let for_task = hits.clone();

    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let hits = for_task.clone();
            tokio::spawn(async move {
                use tokio::io::AsyncReadExt as _;
                let mut buf = [0u8; 4096];
                // Read what the client sent, then hang up saying nothing.
                // The request HAS been delivered; the answer never comes.
                if sock.read(&mut buf).await.unwrap_or(0) > 0 {
                    hits.fetch_add(1, Ordering::SeqCst);
                }
                drop(sock);
            });
        }
    });

    let client = Client::connect(&format!("http://127.0.0.1:{port}"), SecretString::from("t"))
        .await
        .unwrap();
    let started = Instant::now();
    let out = client.call_raw("promote_draft", json!({})).await;

    assert!(out.is_err(), "a dropped connection is still a failure");
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "a request the server READ must reach it exactly once — replaying it \
         is how promote_draft gets applied twice"
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "and it must fail promptly rather than burning the retry budget"
    );
}

/// A capturing layer: every event's message field, in order.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<String>>>);

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Captured {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        struct Sink<'a>(&'a mut String);
        impl tracing::field::Visit for Sink<'_> {
            fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
                self.0.push_str(&format!("{}={:?} ", f.name(), v));
            }
        }
        let mut line = String::new();
        event.record(&mut Sink(&mut line));
        self.0.lock().unwrap().push(line);
    }
}

/// A retry that says nothing is not observable, and an unobservable retry
/// cannot be verified where it matters — in production, across a real deploy.
///
/// This was the gap that made the live fix unprovable: escurel's restart
/// window is real and bounded, but a successful re-dial left no trace, so a
/// working retry and a restart that never overlapped a call looked identical
/// in the logs. The only evidence available was an ABSENCE of failure.
///
/// The recovery line is the one that carries the evidence: it is emitted ONLY
/// when a call actually waited, and it names the tool and how long. That
/// number is what tells an operator whether the budget is sized for their
/// rollout.
///
/// The `warn!` on the first re-dial is deliberately NOT asserted here.
/// Callsite interest is cached process-wide, and another test in this shared
/// binary installs a global subscriber whose verdict for that callsite
/// outlives it — so asserting it would be green alone and red in the suite,
/// which is worse than not asserting it. The recovery line is reached first
/// inside this test and is not affected.
///
/// Single-threaded on purpose: `set_default` installs the subscriber for the
/// CURRENT THREAD, so a multi-thread runtime can run the retry on a worker
/// where the capture is not installed.
#[tokio::test]
async fn a_retry_says_how_long_it_waited() {
    use tracing_subscriber::layer::SubscriberExt as _;

    let port = vacant_port().await;
    serve_after(port, Duration::from_secs(2));

    let captured = Captured::default();
    let subscriber = tracing_subscriber::registry().with(captured.clone());
    let guard = tracing::subscriber::set_default(subscriber);
    tracing::callsite::rebuild_interest_cache();

    let client = Client::connect(&format!("http://127.0.0.1:{port}"), SecretString::from("t"))
        .await
        .unwrap();
    client
        .call_raw("list_skills", json!({}))
        .await
        .expect("the call succeeds");
    drop(guard);

    let lines = captured.0.lock().unwrap().clone();
    let recovery: Vec<&String> = lines
        .iter()
        .filter(|l| l.contains("gateway came back"))
        .collect();
    let joined = lines.join("\n");

    assert_eq!(
        recovery.len(),
        1,
        "a call that waited must say so exactly once: {joined}"
    );
    assert!(
        recovery[0].contains("tool=\"list_skills\""),
        "it must name the call: {}",
        recovery[0]
    );
    let waited: u64 = recovery[0]
        .split("waited_ms=")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("waited_ms must be readable: {}", recovery[0]));
    assert!(
        (2000..10_000).contains(&waited),
        "waited_ms must be the real wait (~2s here), not a placeholder: {waited}"
    );
}

/// NEGATIVE CONTROL: a call that never waited says nothing.
///
/// Without this, the assertion above would pass just as well against a client
/// that logged "gateway came back" on every single call — which would make the
/// line useless as evidence that a deploy window was crossed.
#[tokio::test]
async fn a_call_that_did_not_wait_stays_quiet() {
    use tracing_subscriber::layer::SubscriberExt as _;

    // Already listening: no gap to cross.
    let port = vacant_port().await;
    serve_after(port, Duration::from_millis(0));
    tokio::time::sleep(Duration::from_millis(300)).await;

    let captured = Captured::default();
    let subscriber = tracing_subscriber::registry().with(captured.clone());
    let guard = tracing::subscriber::set_default(subscriber);
    tracing::callsite::rebuild_interest_cache();

    let client = Client::connect(&format!("http://127.0.0.1:{port}"), SecretString::from("t"))
        .await
        .unwrap();
    client
        .call_raw("list_skills", json!({}))
        .await
        .expect("the call succeeds");
    drop(guard);

    let lines = captured.0.lock().unwrap().clone();
    assert!(
        !lines.iter().any(|l| l.contains("gateway came back")),
        "a call that crossed no gap must not claim it waited: {}",
        lines.join("\n")
    );
}
