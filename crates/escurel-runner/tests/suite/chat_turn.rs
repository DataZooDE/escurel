//! Chat runs (governed chat agent, BRD FR-T/FR-R, AC1/AC2): a user turn in a
//! page-bound conversation, plus a `chat` event naming it, is answered by
//! exactly one assistant turn that the RUNNER appends with the run's own
//! token.
//!
//! Real gateway, real runner binary in minted mode (only a runner holding a
//! signing key can mint the run token the reply rule #785 requires), real echo
//! harness. The echo answers `echo: <the question>` and lists the focus pages
//! it was shown, so the test can see what the run was allowed to read.

use std::process::{Child, Command};
use std::time::{Duration, Instant};

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role, free_port};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const TENANT: &str = "acme";
const ALICE: &str = "user:alice";
const CHAT_SKILL: &str = "---\nkind: skill\nid: chat\ndescription: Answer questions.\n\
    autonomy: review\n---\n# chat\n\nAnswer the person's question from the knowledge base.\n";
const CONVERSATION_SKILL: &str = "---\nkind: skill\nid: conversation\ndescription: A chat.\n\
    owner_field: owner\nacl:\n  read: [owner]\n  create: [owner]\n  update: [owner]\n  delete: [owner]\n\
    chat:\n  page_bound: true\n---\n# conversation\n";
const CONVERSATION: &str = "---\nkind: instance\nskill: conversation\nid: c-1\n\
    title: Renewals\nowner: \"user:alice\"\nstarted_at: 2026-10-25T10:00:00Z\n---\n# Renewals\n";
const CONVERSATION_PAGE: &str = "markdown/instances/conversation/c-1.md";
const NOTE_SKILL: &str = "---\nkind: skill\nid: note\ndescription: Note.\nvisibility: owner\n\
    owner_field: owner\n---\n# note\n";
const ALICE_NOTE: &str = "---\nkind: instance\nskill: note\nid: mine\nowner: \"user:alice\"\n---\n# Mine\n\nGlobex renews.\n";
const BOB_NOTE: &str =
    "---\nkind: instance\nskill: note\nid: theirs\nowner: \"user:bob\"\n---\n# Theirs\n\nSECRET.\n";

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
                .skill("note", NOTE_SKILL)
                .instance("conversation", "c-1", CONVERSATION)
                .instance("note", "mine", ALICE_NOTE)
                .instance("note", "theirs", BOB_NOTE)
                .done(),
        ),
        ..Default::default()
    })
    .await
}

async fn call(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    let body: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": { "name": name, "arguments": args } }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json");
    assert!(body.get("error").is_none(), "{name}: {body}");
    body["result"]["structuredContent"].clone()
}

fn group() -> String {
    format!(
        "page-chat:v1:{:x}:{CONVERSATION_PAGE}",
        Sha256::digest(ALICE.as_bytes())
    )
}

/// A minted-mode runner on the echo harness (no static bearer).
fn spawn_runner(gw: &EscurelProcess) -> (ChildGuard, String) {
    let (signing_key, kid) = gw.signing_material();
    let listen = format!("127.0.0.1:{}", free_port());
    let ledger_dir = tempfile::tempdir().expect("tempdir");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gw.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env_remove("ESCUREL_RUNNER_TOKEN")
        .env("ESCUREL_RUNNER_AUTH_ISSUER", gw.issuer_url())
        .env("ESCUREL_RUNNER_AUTH_KID", kid)
        .env("ESCUREL_RUNNER_AUTH_SIGNING_KEY", &signing_key)
        .env("ESCUREL_RUNNER_HARNESS", "echo")
        .env(
            "ESCUREL_RUNNER_LEDGER_PATH",
            ledger_dir.keep().join("ledger.duckdb"),
        )
        .env("ESCUREL_RUNNER_RETRY_BACKOFF", "50ms")
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms");
    (ChildGuard(cmd.spawn().expect("spawn runner")), listen)
}

/// The person's turn: append it, then capture the `chat` event naming it.
async fn ask(gw: &EscurelProcess, alice: &str, msg_id: &str, text: &str, focus: &[&str]) -> String {
    call(
        gw,
        alice,
        "append_message",
        json!({ "chat_group_id": group(), "role": "user", "content": text,
                "msg_id": msg_id, "embed": false }),
    )
    .await;
    let captured = call(
        gw,
        alice,
        "capture_event",
        json!({ "source": "chat", "label_skill": "chat", "instance_page_id": CONVERSATION_PAGE,
                "title": "chat turn", "body": "",
                "provenance": { "chat_group_id": group(), "user_msg_id": msg_id, "focus": focus } }),
    )
    .await;
    captured["event_id"].as_str().expect("event_id").to_owned()
}

async fn await_terminal(listen: &str, event_id: &str) -> (String, String) {
    let url = format!("http://{listen}/debug/run?tenant={TENANT}&event_id={event_id}");
    let deadline = Instant::now() + Duration::from_secs(40);
    loop {
        if let Ok(resp) = reqwest::get(&url).await
            && let Ok(v) = resp.json::<Value>().await
            && let Some(status) = v["status"].as_str()
            && status != "pending"
        {
            return (v["run_id"].as_str().unwrap().to_owned(), status.to_owned());
        }
        assert!(Instant::now() < deadline, "run never reached a terminal");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The group's messages, oldest first, once `n` are there (the reply is
/// appended after the ledger terminal).
async fn messages(gw: &EscurelProcess, alice: &str, n: usize) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let r = call(
            gw,
            alice,
            "list_messages",
            json!({ "chat_group_id": group(), "direction": "asc" }),
        )
        .await;
        let msgs = r["messages"].as_array().cloned().unwrap_or_default();
        if msgs.len() >= n || Instant::now() >= deadline {
            return msgs;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn run_finished(gw: &EscurelProcess, run_id: &str) -> Value {
    let admin = gw.mint_token(TENANT, Role::Admin);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let r = call(gw, &admin, "list_events", json!({ "run_id": run_id })).await;
        if let Some(e) = r["events"]
            .as_array()
            .and_then(|es| es.iter().find(|e| e["title"] == "run-finished"))
        {
            return serde_json::from_str(e["body"].as_str().unwrap_or("{}")).unwrap();
        }
        assert!(Instant::now() < deadline, "no run-finished for {run_id}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn a_user_turn_gets_exactly_one_assistant_reply_from_the_run() {
    let gw = gateway().await;
    let alice = gw.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let (_runner, listen) = spawn_runner(&gw);

    let event_id = ask(&gw, &alice, "u-1", "Which customers renew?", &[]).await;
    let (run_id, status) = await_terminal(&listen, &event_id).await;
    assert_eq!(status, "processed");

    let msgs = messages(&gw, &alice, 2).await;
    assert_eq!(
        msgs.len(),
        2,
        "one user turn, one assistant turn: {msgs:#?}"
    );
    let reply = &msgs[1];
    assert_eq!(reply["role"], "assistant");
    assert_eq!(reply["content"], "echo: Which customers renew?");
    assert_eq!(reply["msg_id"], format!("run:{run_id}:reply"));
    // Server-stamped from the run token: the run, its root event, the asker.
    assert_eq!(reply["metadata"]["run_id"], run_id.as_str());
    assert_eq!(reply["metadata"]["event_id"], event_id.as_str());
    assert_eq!(reply["author"], ALICE, "the run acts as the asker");
    // Runner-set.
    assert_eq!(reply["metadata"]["status"], "ok");
    assert_eq!(reply["metadata"]["user_msg_id"], "u-1");
    assert_eq!(reply["metadata"]["drafts"], json!([]));

    let finished = run_finished(&gw, &run_id).await;
    assert_eq!(finished["status"], "processed");
    // `report_progress` works under the asker-scoped run token (FR-A4).
    assert!(
        finished["plan"].is_object() || finished["plan"].is_array(),
        "the run's progress was recorded: {finished}"
    );
    // Nothing else arrives later.
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(messages(&gw, &alice, 3).await.len(), 2);
}

#[tokio::test]
async fn identical_turns_both_answered() {
    let gw = gateway().await;
    let alice = gw.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let (_runner, listen) = spawn_runner(&gw);

    let first = ask(&gw, &alice, "u-1", "yes", &[]).await;
    assert_eq!(await_terminal(&listen, &first).await.1, "processed");
    let second = ask(&gw, &alice, "u-2", "yes", &[]).await;
    assert_eq!(await_terminal(&listen, &second).await.1, "processed");

    let msgs = messages(&gw, &alice, 4).await;
    let replies: Vec<&Value> = msgs.iter().filter(|m| m["role"] == "assistant").collect();
    assert_eq!(replies.len(), 2, "{msgs:#?}");
    assert_eq!(replies[0]["metadata"]["user_msg_id"], "u-1");
    assert_eq!(replies[1]["metadata"]["user_msg_id"], "u-2");
}

#[tokio::test]
async fn a_second_event_for_the_same_user_turn_is_not_answered_twice() {
    let gw = gateway().await;
    let alice = gw.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let (_runner, listen) = spawn_runner(&gw);

    let first = ask(&gw, &alice, "u-1", "Which customers renew?", &[]).await;
    assert_eq!(await_terminal(&listen, &first).await.1, "processed");
    // The client re-captures for the same turn (a retry after a timeout).
    let again = call(
        &gw,
        &alice,
        "capture_event",
        json!({ "source": "chat", "label_skill": "chat", "instance_page_id": CONVERSATION_PAGE,
                "title": "chat turn", "body": "",
                "provenance": { "chat_group_id": group(), "user_msg_id": "u-1" } }),
    )
    .await["event_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(await_terminal(&listen, &again).await.1, "dead_letter");
    tokio::time::sleep(Duration::from_secs(1)).await;
    let msgs = messages(&gw, &alice, 3).await;
    assert_eq!(msgs.len(), 2, "one reply per user turn: {msgs:#?}");
}

#[tokio::test]
async fn a_chat_run_reads_with_the_askers_rights() {
    let gw = gateway().await;
    let alice = gw.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let (_runner, listen) = spawn_runner(&gw);

    let event_id = ask(
        &gw,
        &alice,
        "u-1",
        "compare these",
        &[
            "markdown/instances/note/mine.md",
            "markdown/instances/note/theirs.md",
        ],
    )
    .await;
    assert_eq!(await_terminal(&listen, &event_id).await.1, "processed");
    let msgs = messages(&gw, &alice, 2).await;
    let content = msgs[1]["content"].as_str().unwrap();
    assert!(
        content.contains("markdown/instances/note/mine.md"),
        "{content}"
    );
    assert!(
        !content.contains("theirs"),
        "a page the asker cannot read never reaches the run: {content}"
    );
}
