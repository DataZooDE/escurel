//! Chat turns (governed chat agent, BRD FR-T2/FR-T3): an event whose
//! provenance names a page-chat turn dedups on `(chat_group_id, user_msg_id)`,
//! not on what it says. The same words asked twice are two runs; a second
//! event for the SAME turn — a client re-capturing after a timeout — is not.
//!
//! Real gateway, real runner binary, real echo harness.

use std::process::{Child, Command};
use std::time::{Duration, Instant};

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role, free_port};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const GROUP: &str = "page-chat:v1:0000:markdown/instances/conversation/c-1.md";
const CHAT_SKILL: &str = "---\nkind: skill\nid: chat\ndescription: Answer questions.\n\
    autonomy: auto\n---\n# chat\n\nAnswer the person's question.\n";
const CONVERSATION_SKILL: &str =
    "---\nkind: skill\nid: conversation\ndescription: A chat.\n---\n# conversation\n";
const CONVERSATION: &str = "---\nkind: instance\nskill: conversation\nid: c-1\n---\n# Renewals\n";
const CONVERSATION_PAGE: &str = "markdown/instances/conversation/c-1.md";

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn gateway() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("chat", CHAT_SKILL)
                .skill("conversation", CONVERSATION_SKILL)
                .instance("conversation", "c-1", CONVERSATION)
                .done(),
        ),
        ..Default::default()
    })
    .await
}

fn spawn_runner(gw: &EscurelProcess) -> (ChildGuard, String) {
    let listen = format!("127.0.0.1:{}", free_port());
    let ledger_dir = tempfile::tempdir().expect("tempdir");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gw.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_TOKEN", gw.mint_token(TENANT, Role::Admin))
        .env("ESCUREL_RUNNER_HARNESS", "echo")
        .env(
            "ESCUREL_RUNNER_LEDGER_PATH",
            ledger_dir.keep().join("ledger.duckdb"),
        )
        .env("ESCUREL_RUNNER_RETRY_BACKOFF", "50ms")
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms");
    (ChildGuard(cmd.spawn().expect("spawn runner")), listen)
}

/// Capture a `chat` event for turn `user_msg_id` saying `text`.
async fn capture_turn(gw: &EscurelProcess, user_msg_id: &str, text: &str) -> String {
    let token = gw.mint_token(TENANT, Role::Agent);
    let body: Value = reqwest::Client::new()
        .post(gw.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "capture_event", "arguments": {
                "source": "chat", "label_skill": "chat", "instance_page_id": CONVERSATION_PAGE,
                "title": "chat turn", "body": text,
                "provenance": { "chat_group_id": GROUP, "user_msg_id": user_msg_id } } } }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json");
    body["result"]["structuredContent"]["event_id"]
        .as_str()
        .unwrap_or_else(|| panic!("capture: {body}"))
        .to_owned()
}

async fn await_terminal(listen: &str, event_id: &str) -> String {
    let url = format!("http://{listen}/debug/run?tenant={TENANT}&event_id={event_id}");
    let deadline = Instant::now() + Duration::from_secs(40);
    loop {
        if let Ok(resp) = reqwest::get(&url).await
            && let Ok(v) = resp.json::<Value>().await
            && let Some(status) = v["status"].as_str()
            && status != "pending"
        {
            return status.to_owned();
        }
        assert!(Instant::now() < deadline, "run never reached a terminal");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn identical_turns_are_two_runs_and_one_turn_is_one_run() {
    let gw = gateway().await;
    let (_runner, listen) = spawn_runner(&gw);

    let first = capture_turn(&gw, "u-1", "yes").await;
    assert_eq!(await_terminal(&listen, &first).await, "processed");
    // Same words, same page, a new turn: content dedup must not swallow it.
    let second = capture_turn(&gw, "u-2", "yes").await;
    assert_eq!(await_terminal(&listen, &second).await, "processed");
    // The same turn again (a re-capture): one run per turn.
    let again = capture_turn(&gw, "u-1", "yes").await;
    assert_eq!(await_terminal(&listen, &again).await, "dead_letter");
}
