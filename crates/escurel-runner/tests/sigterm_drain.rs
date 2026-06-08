//! DoD test (c) for issue #158 — **SIGTERM drain + restart consistency**,
//! with **no mocks**.
//!
//! Per the issue's Scope/DoD: start the real runner, trigger a real run, send a
//! REAL `SIGTERM` (libc `kill`) to the child mid-run, assert it drains (the
//! in-flight run completes or is left consistently recoverable, exit code 0),
//! then RESTART a fresh runner against the SAME ledger file + gateway and
//! assert ledger consistency — no orphaned `pending`; the event is either
//! `processed` or cleanly re-driven.
//!
//! ## Why this is real / no-mock
//!
//! - Real gateway (`EscurelProcess`), real `/mcp`, real `FixtureBuilder` data.
//! - Real `escurel-runner` subprocess; a REAL `SIGTERM` is delivered to its PID
//!   via `libc::kill` (not a tokio in-process signal, not a mock).
//! - A REAL second `escurel-runner` process is started against the SAME real
//!   sqlite ledger file + the SAME gateway; crash recovery + the poller run for
//!   real.
//! - Consistency is read back from the REAL ledger (`/debug/ledger`,
//!   `/debug/run`) and the REAL gateway inbox.

use std::net::TcpListener;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const SKILL: &str = "note";
const SKILL_BODY: &str = "---\ntype: skill\nid: note\n---\n# note\n\nFold the note.\n";
const INSTANCE_BODY: &str = "---\ntype: instance\nid: log\nskill: note\n---\n# Log\n\nBASELINE.\n";

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    listener.local_addr().expect("read local_addr").port()
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

/// Spawn a runner against the given ledger path + gateway.
fn spawn_runner(listen: &str, gateway: &EscurelProcess, token: &str, ledger_path: &str) -> Child {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gateway.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_TOKEN", token)
        .env("ESCUREL_RUNNER_HARNESS", "echo")
        .env("ESCUREL_RUNNER_LEDGER_PATH", ledger_path)
        .env("ESCUREL_RUNNER_MAX_ATTEMPTS", "3")
        .env("ESCUREL_RUNNER_RETRY_BACKOFF", "100ms")
        .env("ESCUREL_RUNNER_DRAIN_TIMEOUT", "15s")
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "150ms");
    cmd.spawn().expect("spawn escurel-runner")
}

/// Wait until the runner's HTTP listener answers `/healthz`.
async fn wait_healthy(http: &reqwest::Client, listen: &str, within: Duration) {
    let deadline = Instant::now() + within;
    let url = format!("http://{listen}/healthz");
    loop {
        if let Ok(resp) = http.get(&url).send().await
            && resp.status().is_success()
        {
            return;
        }
        if Instant::now() >= deadline {
            panic!("runner never became healthy at {listen}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn sigterm_drains_and_restart_is_consistent() {
    // 1. Real gateway with a skill + instance to fold into.
    let gateway = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill(SKILL, SKILL_BODY)
                .instance(SKILL, "log", INSTANCE_BODY)
                .done(),
        ),
        ..Default::default()
    })
    .await;

    let instance_page = format!("markdown/instances/{SKILL}/log.md");

    // 2. Capture a real event pre-flagged to the instance.
    let captured = call_mcp(
        &gateway,
        Role::Agent,
        "capture_event",
        json!({
            "source": "manual",
            "mime": "text/plain",
            "label_skill": SKILL,
            "instance_page_id": instance_page,
            "title": "drain me",
            "body": "process across a restart"
        }),
    )
    .await;
    let event_id = captured["event_id"].as_str().unwrap().to_owned();

    let token = gateway.mint_token(TENANT, Role::Agent);
    let http = reqwest::Client::new();

    // The SAME ledger file across both runner lifetimes — the durability under
    // test. Held in a tempdir that outlives both processes.
    let ledger_dir = tempfile::tempdir().expect("tempdir for ledger");
    let ledger_path = ledger_dir.path().join("ledger.sqlite");
    let ledger_path = ledger_path.to_str().unwrap().to_owned();

    // 3. Start the first runner; wait until it is healthy + has begun the run
    //    (a ledger row for the event exists).
    let port1 = free_port();
    let listen1 = format!("127.0.0.1:{port1}");
    let mut child1 = spawn_runner(&listen1, &gateway, &token, &ledger_path);
    let pid1 = child1.id();
    wait_healthy(&http, &listen1, Duration::from_secs(15)).await;

    let run_url1 = format!("http://{listen1}/debug/run?tenant={TENANT}&event_id={event_id}");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Ok(resp) = http.get(&run_url1).send().await
            && resp.status().is_success()
        {
            let body: Value = resp.json().await.unwrap_or(json!({}));
            if body["event_id"].as_str() == Some(event_id.as_str()) {
                break; // a run row exists (pending or already terminal)
            }
        }
        if Instant::now() >= deadline {
            panic!("first runner never began a run for {event_id}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // 4. Send a REAL SIGTERM to the running child and wait for it to exit. The
    //    runner must drain (let any in-flight run finish, bounded by the drain
    //    timeout) and exit 0 — not be killed by the signal. We deliver the
    //    signal with the real `kill(1)` utility (a genuine SIGTERM to the pid;
    //    the workspace forbids `unsafe`, so no in-process libc call).
    let kill = Command::new("kill")
        .arg("-TERM")
        .arg(pid1.to_string())
        .status()
        .expect("run kill -TERM");
    assert!(kill.success(), "kill -TERM must succeed");

    // Wait for exit, bounded. `wait` blocks; poll `try_wait` with a deadline.
    let exit_deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        match child1.try_wait().expect("try_wait") {
            Some(status) => break status,
            None => {
                if Instant::now() >= exit_deadline {
                    let _ = child1.kill();
                    panic!("runner did not exit within the drain deadline after SIGTERM");
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    };
    assert!(
        status.success(),
        "runner must drain and exit 0 on SIGTERM, got {status:?}"
    );

    // 5. RESTART a fresh runner against the SAME ledger + gateway. Crash
    //    recovery sweeps any orphaned pending row; the poller backstops the
    //    still-inbox event. Either way the event converges to processed.
    let port2 = free_port();
    let listen2 = format!("127.0.0.1:{port2}");
    let _child2 = ChildGuard(spawn_runner(&listen2, &gateway, &token, &ledger_path));
    wait_healthy(&http, &listen2, Duration::from_secs(15)).await;

    // 6. Ledger consistency: within the deadline the event becomes `processed`
    //    (recovered-as-confirmed or re-driven by the poller), with NO orphaned
    //    `pending` row left behind.
    let run_url2 = format!("http://{listen2}/debug/run?tenant={TENANT}&event_id={event_id}");
    let ledger_url2 = format!("http://{listen2}/debug/ledger");
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        let mut processed = false;
        if let Ok(resp) = http.get(&run_url2).send().await
            && resp.status().is_success()
        {
            let body: Value = resp.json().await.unwrap_or(json!({}));
            processed = body["status"].as_str() == Some("processed");
        }
        // The run is processed AND no row is left dangling pending.
        if processed
            && let Ok(resp) = http.get(&ledger_url2).send().await
            && resp.status().is_success()
        {
            let body: Value = resp.json().await.unwrap_or(json!({}));
            let total = body["total"].as_u64().unwrap_or(0);
            let succeeded = body["succeeded"].as_u64().unwrap_or(0);
            // Exactly one run row for the single event, and it is terminal
            // (succeeded), so nothing is orphaned pending.
            assert_eq!(total, 1, "exactly one run row for the single event: {body}");
            assert_eq!(succeeded, 1, "the event's run is processed: {body}");
            break;
        }
        if Instant::now() >= deadline {
            panic!("event {event_id} never reached a consistent processed state after restart");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // 7. The effect genuinely landed: the event is processed on the instance in
    //    the REAL gateway.
    let events = call_mcp(
        &gateway,
        Role::Agent,
        "list_events",
        json!({ "instance_page_id": instance_page }),
    )
    .await;
    let processed = events["events"]
        .as_array()
        .map(|es| {
            es.iter()
                .any(|e| e["event_id"] == json!(event_id) && e["status"] == json!("processed"))
        })
        .unwrap_or(false);
    assert!(
        processed,
        "the event must be processed on the instance: {events}"
    );
}
