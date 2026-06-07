//! DoD test for issue #149: the **durable run ledger + idempotency gate**,
//! end to end with **no mocks** — a real `escurel` gateway process, a real
//! `escurel-runner` process with a real SQLite ledger file, talking over
//! real HTTP.
//!
//! Per #149 Scope: *"deliver the same real event twice (webhook + poll) and
//! assert exactly one terminal run row in the real ledger."*
//!
//! Flow:
//! 1. Spawn a real gateway (`EscurelProcess`, TestIssuer auth, one tenant
//!    fixture) and seed an inbox event via the real `capture_event` tool.
//! 2. Spawn the real `escurel-runner` binary pointed at the gateway, with a
//!    minted agent token, a fast poll interval, and a real ledger file in a
//!    tempdir (`ESCUREL_RUNNER_LEDGER_PATH`).
//! 3. Let the poller pull the event (delivery #1). POST the SAME event over
//!    `/trigger` (delivery #2, the webhook path).
//! 4. Poll the runner's `GET /debug/ledger` and assert the ledger holds
//!    **exactly one terminal run row** for that event — the two deliveries
//!    collapsed to a single durable run (idempotency).

use std::net::TcpListener;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

/// The gateway's authoritative single-tenant identity in the test-support
/// harness (`indexer.tenant()`).
const TENANT: &str = "acme";

/// Kills the spawned runner on drop so a test failure never orphans it.
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

/// Call an MCP tool over `/mcp` with a freshly minted bearer; return the
/// JSON-RPC `result`.
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
    body["result"].clone()
}

/// Fetch `/debug/ledger` and return `(total_runs, terminal_runs)` for the
/// tenant. Returns `None` if the endpoint is not (yet) reachable.
async fn ledger_counts(client: &reqwest::Client, base: &str) -> Option<(u64, u64)> {
    let resp = client
        .get(format!("{base}/debug/ledger"))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body: Value = resp.json().await.ok()?;
    let total = body["total"].as_u64()?;
    let terminal = body["terminal"].as_u64()?;
    Some((total, terminal))
}

#[tokio::test]
async fn same_event_twice_yields_exactly_one_terminal_run() {
    // 1. Real gateway with a tenant fixture.
    let gateway = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(FixtureBuilder::new().tenant(TENANT).done()),
        ..Default::default()
    })
    .await;

    // Seed an inbox event over the real capture_event tool.
    let captured = call_mcp(
        &gateway,
        Role::Agent,
        "capture_event",
        json!({
            "source": "manual",
            "mime": "text/plain",
            "label_skill": "note",
            "title": "ledger idempotency",
            "body": "ledger idempotency"
        }),
    )
    .await;
    let event_id = captured["event_id"]
        .as_str()
        .expect("capture_event returns an event_id")
        .to_owned();

    // 2. Spawn the real runner with a real ledger file in a tempdir.
    let ledger_dir = tempfile::tempdir().expect("tempdir");
    let ledger_path = ledger_dir.path().join("runner-ledger.sqlite");
    let token = gateway.mint_token(TENANT, Role::Agent);
    let port = free_port();
    let listen = format!("127.0.0.1:{port}");
    let base = format!("http://{listen}");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gateway.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_TOKEN", &token)
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms")
        .env("ESCUREL_RUNNER_LEDGER_PATH", &ledger_path);
    let _runner = ChildGuard(cmd.spawn().expect("spawn escurel-runner"));

    let client = reqwest::Client::new();

    // 3a. Delivery #1 (poll): wait until the runner has created + reconciled
    //     the run (one terminal row in the durable ledger).
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some((total, terminal)) = ledger_counts(&client, &base).await
            && total >= 1
            && terminal >= 1
        {
            break;
        }
        if Instant::now() >= deadline {
            let counts = ledger_counts(&client, &base).await;
            panic!("runner never produced a terminal run row within 20s; counts={counts:?}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // 3b. Delivery #2 (webhook): POST the SAME event. Dev mode (no secret),
    //     so an unsigned POST of the serialized Event + tenant_id is accepted.
    let event_body = json!({
        "event_id": event_id,
        "source": "manual",
        "mime": "text/plain",
        "label_skill": "note",
        "title": "ledger idempotency",
        "body": "ledger idempotency",
        "status": "inbox",
        "tenant_id": TENANT
    });
    let resp = client
        .post(format!("{base}/trigger"))
        .json(&event_body)
        .send()
        .await
        .expect("POST /trigger");
    assert_eq!(resp.status().as_u16(), 202, "webhook must accept the event");

    // Give both paths a beat to settle, then assert idempotency held: the
    // two deliveries collapsed to exactly one terminal run row.
    tokio::time::sleep(Duration::from_millis(800)).await;
    let (total, terminal) = ledger_counts(&client, &base)
        .await
        .expect("ledger counts available");
    assert_eq!(
        total, 1,
        "the same event delivered twice must yield exactly one run row, got total={total}"
    );
    assert_eq!(
        terminal, 1,
        "the single run must be terminal (idempotency authority), got terminal={terminal}"
    );
}
