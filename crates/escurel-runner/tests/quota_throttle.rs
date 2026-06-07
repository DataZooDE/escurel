//! DoD test (a) for issue #158 — **quota throttling**, with **no mocks**.
//!
//! Per the issue's Scope/DoD: against a real `EscurelProcess` + real runner +
//! real echo-harness, set a LOW `ESCUREL_RUNNER_TENANT_RUNS_PER_MIN`, fire MORE
//! real events than the limit allows in the window, and assert real
//! throttling: the over-limit events are NOT all processed at once (some are
//! held / left in the inbox), the rate is bounded, and throttling is recorded
//! (the runner's `/metrics` `escurel_runner_throttled_total` counter advances).
//!
//! ## Why this is real / no-mock
//!
//! - The events are captured over the real `/mcp` `capture_event` tool and
//!   land in the real gateway inbox.
//! - The runner is the real `escurel-runner` binary with a real echo-harness
//!   subprocess; the quota gate is the real in-process `Governor`.
//! - Throttling is read back two ways from real state: (1) the runner's real
//!   `/metrics` endpoint reports a non-zero `escurel_runner_throttled_total`,
//!   and (2) the real gateway inbox still holds un-processed events while the
//!   per-minute budget is spent (they were held, not processed).
//!
//! The runs/min budget is set well below the burst so the gate must fire; the
//! poll interval is short so the poller drives the budgeted runs. The whole
//! test is deadline-bounded.

use std::net::TcpListener;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const SKILL: &str = "note";
const SKILL_BODY: &str = "---\ntype: skill\nid: note\n---\n# note\n\nFold the note.\n";
const INSTANCE_BODY: &str = "---\ntype: instance\nid: log\nskill: note\n---\n# Log\n\nBASELINE.\n";

/// Runs/min budget — deliberately small so a burst of more than this many
/// events must throttle within the one-minute window.
const RUNS_PER_MIN: usize = 2;
/// Number of real events fired (well over the budget).
const BURST: usize = 8;

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
    body["result"].clone()
}

#[tokio::test]
async fn over_quota_triggers_are_throttled_not_all_processed() {
    // 1. Real gateway with one skill + one instance to fold into.
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

    // 2. Fire a BURST of real events, each pre-flagged to the instance so the
    //    echo-harness has real work for every one that is admitted.
    for i in 0..BURST {
        call_mcp(
            &gateway,
            Role::Agent,
            "capture_event",
            json!({
                "source": "manual",
                "mime": "text/plain",
                "label_skill": SKILL,
                "instance_page_id": instance_page,
                "title": format!("evt-{i}"),
                "body": format!("burst event {i}")
            }),
        )
        .await;
    }

    // 3. Spawn the real runner with a LOW per-tenant runs/min budget so a burst
    //    over the budget must throttle within the one-minute window.
    let token = gateway.mint_token(TENANT, Role::Agent);
    let port = free_port();
    let listen = format!("127.0.0.1:{port}");
    let ledger_dir = tempfile::tempdir().expect("tempdir for ledger");
    let ledger_path = ledger_dir.path().join("ledger.sqlite");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gateway.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_TOKEN", &token)
        .env("ESCUREL_RUNNER_HARNESS", "echo")
        .env("ESCUREL_RUNNER_LEDGER_PATH", ledger_path.to_str().unwrap())
        .env(
            "ESCUREL_RUNNER_TENANT_RUNS_PER_MIN",
            RUNS_PER_MIN.to_string(),
        )
        .env("ESCUREL_RUNNER_TENANT_MAX_CONCURRENT", "8")
        .env("ESCUREL_RUNNER_MAX_ATTEMPTS", "2")
        .env("ESCUREL_RUNNER_RETRY_BACKOFF", "50ms")
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "150ms");
    let _runner = ChildGuard(cmd.spawn().expect("spawn escurel-runner"));

    let http = reqwest::Client::new();
    let metrics_url = format!("http://{listen}/metrics");

    // 4. Wait until the runner has THROTTLED at least one trigger (the real
    //    quota gate fired) — read from the runner's real /metrics endpoint.
    let deadline = Instant::now() + Duration::from_secs(45);
    let mut throttled = 0u64;
    loop {
        if let Ok(resp) = http.get(&metrics_url).send().await
            && resp.status().is_success()
        {
            let body = resp.text().await.unwrap_or_default();
            throttled = body
                .lines()
                .filter(|l| l.starts_with("escurel_runner_throttled_total{"))
                .filter_map(|l| l.rsplit(' ').next())
                .filter_map(|n| n.trim().parse::<u64>().ok())
                .sum();
            if throttled >= 1 {
                break;
            }
        }
        if Instant::now() >= deadline {
            panic!("runner never throttled an over-quota trigger (saw {throttled})");
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    assert!(
        throttled >= 1,
        "the quota gate must record a real throttle: {throttled}"
    );

    // 5. The rate is bounded: with the budget spent for the window, the gateway
    //    inbox must still hold un-processed events (they were held, not all
    //    processed at once). Give the runner a beat to settle, then read the
    //    REAL inbox and assert processed runs are bounded by the budget.
    tokio::time::sleep(Duration::from_millis(800)).await;
    let inbox = call_mcp(&gateway, Role::Agent, "list_inbox", json!({ "limit": 200 })).await;
    let still_in_inbox = inbox["events"].as_array().map(Vec::len).unwrap_or(0);
    assert!(
        still_in_inbox > 0,
        "over-quota events must be held in the inbox, not all processed: inbox empty"
    );

    // The number of *processed* runs in the ledger must be bounded by the
    // per-minute budget — the rate gate genuinely limited throughput.
    let ledger_url = format!("http://{listen}/debug/ledger");
    let body: Value = http
        .get(&ledger_url)
        .send()
        .await
        .expect("GET /debug/ledger")
        .json()
        .await
        .unwrap_or(json!({}));
    let succeeded = body["succeeded"].as_u64().unwrap_or(0);
    assert!(
        succeeded <= RUNS_PER_MIN as u64,
        "processed runs ({succeeded}) must not exceed the runs/min budget ({RUNS_PER_MIN}) within the window: {body}"
    );
}
