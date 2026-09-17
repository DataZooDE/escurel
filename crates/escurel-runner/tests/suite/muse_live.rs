//! Live end-to-end DoD test for the Muse Code (`muse`) adapter — **no
//! mocks**, but `#[ignore]` because it drives a real LLM (needs `muse` auth +
//! model quota and is non-deterministic/slow, so it must not run in the
//! default gate).
//!
//! Run it on demand / nightly:
//!
//! ```text
//! cargo test -p escurel-runner --test suite muse_live:: -- --ignored
//! ```
//!
//! Structured exactly like `agy_live.rs`, with the **real `muse` CLI** as the
//! harness: a real gateway, a real inbox event, the real `escurel-runner`
//! with `ESCUREL_RUNNER_HARNESS=muse`, and a real `muse exec` subprocess
//! making real `/mcp` tool calls under the scoped token to fold the event.
//!
//! **Why this test can exist at all.** escurel#451 recorded that a `muse`
//! harness could not be built: Muse Code 1.0.1 was not an MCP client, so
//! every escurel effect would have had to be made by the adapter itself —
//! which the `Harness` contract forbids, and for good reason (the scoped
//! token and the packaged tool surface stop meaning anything). Muse 1.1.1 is
//! an MCP client, so the adapter is a real one. The negative was recorded
//! against a VERSION rather than against the design, which is what made it
//! cheap to revisit.
//!
//! Auth: a live run needs `muse` to be logged in — its credentials live in
//! `~/.config/muse`, and the adapter links them into the per-run private
//! config dir. The escurel `/mcp` bearer is separate: it is the scoped token
//! the runner mints.
//!
//! **The skill declares `autonomy: auto`, and it must.** `muse exec` cannot
//! enforce a narrowed tool surface, so the adapter refuses any task packaged
//! under `REVIEW_TOOLS` — which is what an absent `autonomy:` produces. Drop
//! that line and this test asserts the refusal instead of the fold.

use std::net::TcpListener;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const SKILL: &str = "customer";
const SKILL_BODY: &str = "---\ntype: skill\nid: customer\nautonomy: auto\n---\n# customer\n\n\
Fold the triggering event into the named customer instance. Read the instance \
with `expand`, append a short dated note about the event to its body (do NOT \
delete existing content), write it back with `update_page`, then call \
`assign_event` to mark the event processed and bound to the instance.\n";
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
#[ignore = "live LLM; run with --ignored; needs an authenticated muse + model quota"]
async fn muse_harness_folds_event_into_instance_end_to_end() {
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
            "body": "customer wants to renew their annual contract"
        }),
    )
    .await;
    let event_id = captured["event_id"]
        .as_str()
        .expect("capture_event returns an event_id")
        .to_owned();

    // 3. Spawn the real runner with the MUSE harness selected.
    let token = gateway.mint_token(TENANT, Role::Agent);
    let port = free_port();
    let listen = format!("127.0.0.1:{port}");
    // Its OWN ledger. Without this the runner falls back to
    // `./escurel-runner-ledger.sqlite` in the crate directory — one file
    // shared by every test in the suite AND by every previous run of it. A
    // row another test left behind is a row this one inherits: the content
    // dedup saw its fixture already folded in and correctly declined to run
    // it again, which is right behaviour reading wrong state.
    let ledger_dir = tempfile::tempdir().expect("tempdir for ledger");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gateway.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_TOKEN", &token)
        .env("ESCUREL_RUNNER_HARNESS", "muse")
        .env(
            "ESCUREL_RUNNER_LEDGER_PATH",
            ledger_dir.path().join("ledger.sqlite").to_str().unwrap(),
        )
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "1s");
    // Honour an explicit model override if the operator set one.
    if let Ok(model) = std::env::var("ESCUREL_RUNNER_MUSE_MODEL") {
        cmd.env("ESCUREL_RUNNER_MUSE_MODEL", model);
    }
    let _runner = ChildGuard(cmd.spawn().expect("spawn escurel-runner"));

    let http = reqwest::Client::new();
    let ledger_url = format!("http://{listen}/debug/ledger");

    // 4. Wait for the end-to-end effect: the run becomes terminal in the
    //    runner's ledger AND the event is processed on the gateway. A live
    //    LLM round-trip is slow, so allow a generous deadline.
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
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
            panic!("muse harness never folded {event_id} into {instance_page_id} within 180s");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    // The instance page must have been written by the harness: its expanded
    // body must still carry the baseline (append, not clobber).
    let expanded = call_mcp(
        &gateway,
        Role::Agent,
        "expand",
        json!({ "page_id": instance_page_id }),
    )
    .await;
    let body = expanded["body"].as_str().unwrap_or_default();
    assert!(
        body.contains("BASELINE"),
        "the harness must append, not clobber the baseline content: {body}"
    );
}
