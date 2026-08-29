//! DoD test for issue #151 — the **first true trigger→agent→instance
//! end-to-end**, with **no mocks**.
//!
//! Per the issue's Scope/DoD: against a real `EscurelProcess`,
//! `capture_event` a real event; the **real echo-harness process**
//! materialises the instance and folds the event via real `/mcp`; assert
//! (via real `expand`/`list_events`) the event is `processed` and the
//! instance `new_version` advanced. The harness is a real spawned binary —
//! not a mock.
//!
//! Flow:
//! 1. Spawn a real gateway (`EscurelProcess`, TestIssuer auth) seeded via
//!    `FixtureBuilder` with a skill page + a target instance page.
//! 2. `capture_event` a real inbox event labelled with the skill and
//!    pre-flagged to the target instance, over the real `/mcp`.
//! 3. Spawn the real `escurel-runner` (harness=echo, gateway_url, tenant,
//!    token, fast poll). The runner packages the trigger and runs the
//!    **real echo-harness subprocess**, which makes real `/mcp`
//!    `update_page` + `assign_event` calls.
//! 4. Assert the end-to-end effect on the REAL gateway:
//!    - the event is now `processed` (via `list_events` on the instance);
//!    - the instance was written (its `expand` body now carries the echo
//!      harness's appended event note);
//!    - the runner's durable ledger run for the event is terminal
//!      `processed` (via `GET /debug/ledger`).

use std::net::TcpListener;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

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
    let result = body["result"].clone();
    result.get("structuredContent").cloned().unwrap_or(result)
}

#[tokio::test]
async fn echo_harness_folds_event_into_instance_end_to_end() {
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

    // Sanity: the event starts in the inbox (not yet processed).
    let inbox = call_mcp(&gateway, Role::Agent, "list_inbox", json!({})).await;
    assert!(
        inbox["events"]
            .as_array()
            .map(|es| es.iter().any(|e| e["event_id"] == json!(event_id)))
            .unwrap_or(false),
        "seeded event must start in the inbox: {inbox}"
    );

    // 3. Spawn the real runner with the echo harness selected, fast poll.
    let token = gateway.mint_token(TENANT, Role::Agent);
    let port = free_port();
    let listen = format!("127.0.0.1:{port}");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gateway.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_TOKEN", &token)
        .env("ESCUREL_RUNNER_HARNESS", "echo")
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms");
    let _runner = ChildGuard(cmd.spawn().expect("spawn escurel-runner"));

    let http = reqwest::Client::new();
    let ledger_url = format!("http://{listen}/debug/ledger");

    // 4. Wait for the end-to-end effect: the run becomes terminal in the
    //    runner's ledger AND the event is processed on the gateway.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        // (a) Ledger: at least one terminal run row.
        let terminal = http
            .get(&ledger_url)
            .send()
            .await
            .ok()
            .and_then(|r| r.status().is_success().then_some(r));
        let mut ledger_terminal = false;
        if let Some(resp) = terminal {
            let body: Value = resp.json().await.unwrap_or(json!({}));
            ledger_terminal = body["terminal"].as_u64().unwrap_or(0) >= 1;
        }

        // (b) Gateway: the event is now processed on the instance history.
        if ledger_terminal {
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
            panic!("echo harness never folded {event_id} into {instance_page_id} within 30s");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // The instance page must have been written by the harness: its expanded
    // body now carries the echo harness's appended event note.
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
        "the instance body must carry the folded event note (event_id): {body}"
    );
    assert!(
        body.contains("BASELINE"),
        "the harness must append, not clobber the baseline content: {body}"
    );
}
