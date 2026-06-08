//! DoD test for issue #155 — the **outcome reconciler + read-back +
//! retry**, with **no mocks**.
//!
//! Per the issue's Scope/DoD: against a real `EscurelProcess` + the real
//! echo-harness, inject a **real transient `/mcp` failure** and assert the
//! runner *retries*, then *succeeds* once the endpoint is reachable, with the
//! outcome recorded `succeeded` in the **real** sqlite ledger — carrying the
//! produced instance + its confirmed version.
//!
//! ## Why the transient failure is REAL (not a mock)
//!
//! The runner is pointed at a **real TCP reverse proxy** this test owns,
//! listening on a real OS-assigned port. While the proxy is in its initial
//! *refuse* phase it accepts each inbound connection and **immediately drops
//! it** — the runner's `reqwest` client sees a genuine connection reset /
//! truncated response (`Error::Transport`), exactly the wire failure a
//! gateway that is up-but-not-ready, or a flapping load-balancer, produces.
//! No transport is stubbed: real sockets, a real `connect()`, real bytes (or
//! the real absence of them). After the test flips the proxy to its *forward*
//! phase it pipes bytes straight through to the real `EscurelProcess`, so a
//! later retry connects and the whole package→harness→read-back path runs for
//! real.
//!
//! ## Determinism
//!
//! The proxy counts the connections it refuses (`refused`) and only flips to
//! forward mode when the test flips a shared `AtomicBool` — the test flips it
//! *after* observing that the runner has already burned at least one real
//! refused connection (so "the runner retried" is asserted, not assumed) and
//! while attempts remain on the cap. There is no fixed `sleep` gating
//! correctness: the flip is driven by an observed counter, the success wait is
//! a deadline-bounded poll of the real ledger + the real gateway.

use std::net::TcpListener as StdTcpListener;
use std::process::{Child, Command};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};

const TENANT: &str = "acme";
const SKILL: &str = "customer";
const SKILL_BODY: &str =
    "---\ntype: skill\nid: customer\n---\n# customer\n\nFold the event into a customer instance.\n";
const INSTANCE_ID: &str = "globex";
const INSTANCE_BODY: &str =
    "---\ntype: instance\nid: globex\nskill: customer\n---\n# Globex\n\nBASELINE account state.\n";

/// Kills the spawned runner on drop so a test failure never orphans it.
struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Shared state of the real TCP proxy the runner dials.
struct ProxyState {
    /// `false` → refuse (accept + immediately close); `true` → forward.
    forward: AtomicBool,
    /// How many connections were refused in the refuse phase.
    refused: AtomicU64,
}

/// Reserve a real OS-assigned loopback port and hand it back free for the
/// proxy to bind. (Binding then dropping leaves the port unbound; the proxy
/// rebinds it a moment later — acceptable here because only this test process
/// races for it.)
fn free_port() -> u16 {
    let l = StdTcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    l.local_addr().expect("local_addr").port()
}

/// Run the real TCP proxy: bind `proxy_port`; for each inbound connection,
/// refuse (accept + drop) while `forward` is false, else splice it to
/// `upstream` (`host:port` of the real gateway). Real sockets throughout.
async fn run_proxy(proxy_port: u16, upstream: String, state: Arc<ProxyState>) {
    let listener = TcpListener::bind(("127.0.0.1", proxy_port))
        .await
        .expect("proxy bind");
    loop {
        let (inbound, _) = match listener.accept().await {
            Ok(pair) => pair,
            Err(_) => continue,
        };
        if !state.forward.load(Ordering::SeqCst) {
            // Refuse: drop the socket immediately. The peer's in-flight
            // request fails with a real transport error.
            state.refused.fetch_add(1, Ordering::SeqCst);
            drop(inbound);
            continue;
        }
        let upstream = upstream.clone();
        tokio::spawn(async move {
            if let Ok(outbound) = TcpStream::connect(&upstream).await {
                splice(inbound, outbound).await;
            }
        });
    }
}

/// Bidirectional byte copy between the runner-facing and gateway-facing
/// sockets — a real, dumb TCP splice.
async fn splice(mut a: TcpStream, mut b: TcpStream) {
    let (mut ar, mut aw) = a.split();
    let (mut br, mut bw) = b.split();
    let c2s = async {
        let _ = tokio::io::copy(&mut ar, &mut bw).await;
        let _ = bw.shutdown().await;
    };
    let s2c = async {
        let _ = tokio::io::copy(&mut br, &mut aw).await;
        let _ = aw.shutdown().await;
    };
    tokio::join!(c2s, s2c);
}

/// Strip the scheme + return `host:port` of the gateway for the proxy to dial.
fn upstream_authority(base_url: &str) -> String {
    base_url
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .trim_end_matches('/')
        .to_owned()
}

async fn call_mcp(p: &EscurelProcess, role: Role, name: &str, args: Value) -> Value {
    let token = p.mint_token(TENANT, role);
    let resp = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": name, "arguments": args },
        }))
        .send()
        .await
        .expect("post /mcp");
    assert_eq!(resp.status(), 200, "http status");
    let body: Value = resp.json().await.unwrap();
    assert!(body.get("error").is_none(), "tool {name} error: {body}");
    let result = body["result"].clone();
    result.get("structuredContent").cloned().unwrap_or(result)
}

#[tokio::test]
async fn reconciler_retries_a_real_transient_mcp_failure_then_succeeds() {
    // 1. Real gateway with a skill + target instance.
    let gateway = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill(SKILL, SKILL_BODY)
                .instance(SKILL, INSTANCE_ID, INSTANCE_BODY)
                .done(),
        ),
        ..Default::default()
    })
    .await;

    let instance_page_id = format!("markdown/instances/{SKILL}/{INSTANCE_ID}.md");

    // 2. Capture a real inbox event pre-flagged to the target instance.
    let captured = call_mcp(
        &gateway,
        Role::Agent,
        "capture_event",
        json!({
            "source": "manual",
            "mime": "text/plain",
            "label_skill": SKILL,
            "instance_page_id": instance_page_id,
            "title": "renewal request",
            "body": "ECHO_FOLD_MARKER customer wants to renew"
        }),
    )
    .await;
    let event_id = captured["event_id"]
        .as_str()
        .expect("capture_event returns an event_id")
        .to_owned();

    // 3. Stand up the real TCP proxy in front of the gateway, initially in
    //    REFUSE mode so the runner's first `/mcp` attempts hit a real
    //    transport failure.
    let proxy_port = free_port();
    let state = Arc::new(ProxyState {
        forward: AtomicBool::new(false),
        refused: AtomicU64::new(0),
    });
    let upstream = upstream_authority(gateway.base_url());
    {
        let state = Arc::clone(&state);
        tokio::spawn(run_proxy(proxy_port, upstream, state));
    }
    let proxy_url = format!("http://127.0.0.1:{proxy_port}");

    // 4. Spawn the real runner pointed at the PROXY (not the gateway), with a
    //    generous attempts cap + short backoff so retries span the refuse
    //    phase. The runner-local ledger lives in a real sqlite file.
    let token = gateway.mint_token(TENANT, Role::Agent);
    let listen_port = free_port();
    let listen = format!("127.0.0.1:{listen_port}");
    let ledger_dir = tempfile::tempdir().expect("tempdir for ledger");
    let ledger_path = ledger_dir.path().join("ledger.sqlite");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", &proxy_url)
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_TOKEN", &token)
        .env("ESCUREL_RUNNER_HARNESS", "echo")
        .env("ESCUREL_RUNNER_LEDGER_PATH", ledger_path.to_str().unwrap())
        .env("ESCUREL_RUNNER_MAX_ATTEMPTS", "8")
        .env("ESCUREL_RUNNER_RETRY_BACKOFF", "150ms")
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms");
    let _runner = ChildGuard(cmd.spawn().expect("spawn escurel-runner"));

    // 5. Observe REAL refused connections (the runner is retrying), THEN flip
    //    the proxy to forward mode so a later attempt converges. This is the
    //    determinism anchor: we flip only after the failure has actually been
    //    exercised against a real socket.
    let refuse_deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if state.refused.load(Ordering::SeqCst) >= 1 {
            break;
        }
        if Instant::now() >= refuse_deadline {
            panic!("runner never attempted a connection through the proxy (no retry observed)");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let refused_before_flip = state.refused.load(Ordering::SeqCst);
    assert!(
        refused_before_flip >= 1,
        "the runner must hit at least one real refused connection before success"
    );
    state.forward.store(true, Ordering::SeqCst);

    // 6. The run must now converge: the event ends `processed` on the real
    //    gateway AND the run is recorded `succeeded` in the real ledger,
    //    carrying the produced instance + its confirmed version.
    let http = reqwest::Client::new();
    let ledger_url = format!("http://{listen}/debug/ledger");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let mut succeeded = false;
        if let Ok(resp) = http.get(&ledger_url).send().await
            && resp.status().is_success()
        {
            let body: Value = resp.json().await.unwrap_or(json!({}));
            succeeded = body["succeeded"].as_u64().unwrap_or(0) >= 1;
        }
        if succeeded {
            let events = call_mcp(
                &gateway,
                Role::Agent,
                "list_events",
                json!({ "instance_page_id": instance_page_id }),
            )
            .await;
            let processed = events["events"]
                .as_array()
                .map(|es| {
                    es.iter().any(|e| {
                        e["event_id"] == json!(event_id) && e["status"] == json!("processed")
                    })
                })
                .unwrap_or(false);
            if processed {
                break;
            }
        }
        if Instant::now() >= deadline {
            panic!(
                "runner never converged after the proxy came up (refused={}, succeeded={succeeded})",
                state.refused.load(Ordering::SeqCst)
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // 7. Read the REAL sqlite ledger row back and assert it records
    //    `succeeded` WITH the produced instance + a non-empty confirmed
    //    version (the read-back result, not a guess).
    let row = call_debug_run(&http, &listen, &event_id).await;
    assert_eq!(
        row["status"].as_str(),
        Some("processed"),
        "the ledger run must be terminal `processed` (succeeded): {row}"
    );
    assert_eq!(
        row["instance_page_id"].as_str(),
        Some(instance_page_id.as_str()),
        "the ledger must record the produced instance: {row}"
    );
    let version = row["produced_version"].as_str().unwrap_or_default();
    assert!(
        !version.is_empty(),
        "the ledger must record the produced instance's confirmed version: {row}"
    );

    // The instance page must carry the harness's appended note: the write
    // genuinely landed despite the early transient failures.
    let expanded = call_mcp(
        &gateway,
        Role::Agent,
        "expand",
        json!({ "page_id": instance_page_id }),
    )
    .await;
    let body = expanded["body"].as_str().unwrap_or_default();
    assert!(
        body.contains(&event_id),
        "the instance body must carry the folded event note: {body}"
    );
    assert!(
        body.contains("BASELINE"),
        "the harness must append, not clobber the baseline: {body}"
    );
}

/// Fetch a single ledger run row via the runner's `/debug/run` introspection
/// surface (reads the real sqlite file the runner is writing).
async fn call_debug_run(http: &reqwest::Client, listen: &str, event_id: &str) -> Value {
    let url = format!("http://{listen}/debug/run?tenant={TENANT}&event_id={event_id}");
    let resp = http.get(&url).send().await.expect("get /debug/run");
    assert!(resp.status().is_success(), "debug/run status");
    resp.json().await.expect("debug/run json")
}
