//! DoD test for issue #148: the inbox poller + dispatch queue + dedup, end
//! to end with **no mocks** — a real `escurel` gateway process and a real
//! `escurel-runner` process, talking over real HTTP.
//!
//! Flow:
//! 1. Spawn a real gateway (`EscurelProcess`, TestIssuer auth, one tenant
//!    fixture) and seed an inbox event by calling the real `capture_event`
//!    tool over `/mcp` with a minted agent bearer.
//! 2. Spawn the real `escurel-runner` binary pointed at the gateway, with a
//!    minted agent token + a fast (`250ms`) poll interval, on a free port.
//! 3. Poll the runner's `GET /debug/seen` until the seeded `event_id`
//!    appears — proving the poller really pulled it from the real gateway.
//! 4. Convergence/dedup: POST the *same* event to `/trigger` (the webhook
//!    path) and assert `/debug/seen` still lists that `event_id` exactly
//!    once — webhook and poll collapsed onto one seen-set.

use std::net::TcpListener;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

/// The gateway's authoritative single-tenant identity in the test-support
/// harness (`indexer.tenant()`), regardless of the token/fixture tenant.
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

#[tokio::test]
async fn poller_pulls_inbox_event_and_dedups_with_webhook() {
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
            "title": "hello poller",
            "body": "hello poller"
        }),
    )
    .await;
    let event_id = captured["event_id"]
        .as_str()
        .expect("capture_event returns an event_id")
        .to_owned();

    // Sanity: it really landed in the inbox.
    let inbox = call_mcp(&gateway, Role::Agent, "list_inbox", json!({})).await;
    let found = inbox["events"]
        .as_array()
        .map(|es| es.iter().any(|e| e["event_id"] == json!(event_id)))
        .unwrap_or(false);
    assert!(found, "seeded event must be in the gateway inbox: {inbox}");

    // 2. Spawn the real runner, pointed at the gateway, polling fast.
    let token = gateway.mint_token(TENANT, Role::Agent);
    let port = free_port();
    let listen = format!("127.0.0.1:{port}");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gateway.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_TOKEN", &token)
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms");
    let _runner = ChildGuard(cmd.spawn().expect("spawn escurel-runner"));

    let client = reqwest::Client::new();
    let seen_url = format!("http://{listen}/debug/seen");

    // 3. Wait for the poller to pull the event into the seen-set.
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut seen_ids: Vec<String> = Vec::new();
    loop {
        if let Ok(resp) = client.get(&seen_url).send().await
            && resp.status().is_success()
        {
            let body: Value = resp.json().await.unwrap_or(json!({}));
            seen_ids = body["event_ids"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            if seen_ids.iter().any(|id| id == &event_id) {
                break;
            }
        }
        if Instant::now() >= deadline {
            panic!("poller never surfaced {event_id} in /debug/seen within 15s; saw {seen_ids:?}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        seen_ids.iter().filter(|id| *id == &event_id).count() == 1,
        "the polled event must appear exactly once: {seen_ids:?}"
    );

    // 4. Convergence/dedup: POST the SAME event over the webhook path. The
    //    runner has no webhook secret configured (dev), so an unsigned POST
    //    of the serialized Event + tenant_id is accepted.
    let event_body = json!({
        "event_id": event_id,
        "source": "manual",
        "mime": "text/plain",
        "label_skill": "note",
        "title": "hello poller",
        "body": "hello poller",
        "status": "inbox",
        "tenant_id": TENANT
    });
    let resp = client
        .post(format!("http://{listen}/trigger"))
        .json(&event_body)
        .send()
        .await
        .expect("POST /trigger");
    assert_eq!(resp.status().as_u16(), 202, "webhook must accept the event");

    // Give the queue a beat, then assert the event_id is still listed
    // exactly once (webhook collapsed against the poll).
    tokio::time::sleep(Duration::from_millis(500)).await;
    let resp = client.get(&seen_url).send().await.expect("GET /debug/seen");
    let body: Value = resp.json().await.unwrap();
    let ids: Vec<String> = body["event_ids"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        ids.iter().filter(|id| *id == &event_id).count(),
        1,
        "webhook + poll must dedup to a single seen entry: {ids:?}"
    );
}
