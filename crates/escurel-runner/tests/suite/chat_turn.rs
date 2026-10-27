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

use escurel_test_support::{
    AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role, WriteAclMode, free_port,
};
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

const RENEWAL_SKILL: &str =
    "---\nkind: skill\nid: renewal\nautonomy: auto\n---\n# renewal\n\nFold the event in.\n";
const RENEWAL: &str = "---\nkind: instance\nid: c1\nskill: renewal\n---\n# C1\n";
const RENEWAL_PAGE: &str = "markdown/instances/renewal/c1.md";

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
        // Production's write ACL (the test harness defaults to `off`).
        config_overrides: ConfigOverrides {
            write_acl: Some(WriteAclMode::Enforce),
            ..Default::default()
        },
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("chat", CHAT_SKILL)
                .skill("conversation", CONVERSATION_SKILL)
                .skill("note", NOTE_SKILL)
                .instance("conversation", "c-1", CONVERSATION)
                .instance("note", "mine", ALICE_NOTE)
                .instance("note", "theirs", BOB_NOTE)
                .skill("renewal", RENEWAL_SKILL)
                .instance("renewal", "c1", RENEWAL)
                .done(),
        ),
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
    spawn_runner_with(gw, true, &[])
}

/// A runner on the echo harness, minted (`minted`) or on a static admin
/// bearer, with `extra` env.
fn spawn_runner_with(
    gw: &EscurelProcess,
    minted: bool,
    extra: &[(&str, &str)],
) -> (ChildGuard, String) {
    let (signing_key, kid) = gw.signing_material();
    let listen = format!("127.0.0.1:{}", free_port());
    let ledger_dir = tempfile::tempdir().expect("tempdir");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gw.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_HARNESS", "echo")
        .env(
            "ESCUREL_RUNNER_LEDGER_PATH",
            ledger_dir.keep().join("ledger.duckdb"),
        )
        .env("ESCUREL_RUNNER_RETRY_BACKOFF", "50ms")
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms");
    if minted {
        cmd.env_remove("ESCUREL_RUNNER_TOKEN")
            .env("ESCUREL_RUNNER_AUTH_ISSUER", gw.issuer_url())
            .env("ESCUREL_RUNNER_AUTH_KID", kid)
            .env("ESCUREL_RUNNER_AUTH_SIGNING_KEY", &signing_key);
    } else {
        cmd.env("ESCUREL_RUNNER_TOKEN", gw.mint_token(TENANT, Role::Admin));
    }
    for (k, v) in extra {
        cmd.env(k, v);
    }
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
    // An answered turn leaves the inbox: it was handled, and nobody else should pick it up.
    let admin = gw.mint_token(TENANT, Role::Admin);
    let inbox = call(&gw, &admin, "list_inbox", json!({})).await;
    assert!(
        !inbox["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["event_id"] == event_id.as_str()),
        "{inbox}"
    );
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

#[tokio::test]
async fn a_failed_run_posts_a_failed_turn() {
    let gw = gateway().await;
    let alice = gw.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let (_runner, listen) = spawn_runner_with(
        &gw,
        true,
        &[
            ("ESCUREL_ECHO_FAIL_CHAT", "1"),
            ("ESCUREL_RUNNER_CHAT_MAX_ATTEMPTS", "2"),
        ],
    );

    let event_id = ask(&gw, &alice, "u-1", "Which customers renew?", &[]).await;
    assert_eq!(await_terminal(&listen, &event_id).await.1, "dead_letter");
    let msgs = messages(&gw, &alice, 2).await;
    assert_eq!(msgs.len(), 2, "{msgs:#?}");
    assert_eq!(msgs[1]["role"], "assistant");
    assert_eq!(msgs[1]["metadata"]["status"], "failed");
    assert_eq!(msgs[1]["metadata"]["reason"], "harness_error");
    assert!(
        msgs[1]["content"]
            .as_str()
            .unwrap()
            .starts_with("I couldn't finish"),
        "{}",
        msgs[1]["content"]
    );
    // Retries exhausted, one turn — not one per attempt.
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(messages(&gw, &alice, 3).await.len(), 2);
}

#[tokio::test]
async fn a_cancelled_chat_run_posts_a_cancelled_turn() {
    let gw = gateway().await;
    let alice = gw.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let (_runner, listen) = spawn_runner_with(
        &gw,
        true,
        &[
            ("ESCUREL_ECHO_SLEEP_MS", "30000"),
            ("ESCUREL_RUNNER_CANCEL_GRACE", "1s"),
        ],
    );

    let event_id = ask(&gw, &alice, "u-1", "Which customers renew?", &[]).await;
    let url = format!("http://{listen}/debug/run?tenant={TENANT}&event_id={event_id}");
    let deadline = Instant::now() + Duration::from_secs(30);
    while reqwest::get(&url).await.ok().map(|r| r.status()) != Some(reqwest::StatusCode::OK) {
        assert!(Instant::now() < deadline, "the run never started");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    let resp = reqwest::Client::new()
        .post(format!("http://{listen}/debug/cancel"))
        .json(&json!({ "tenant": TENANT, "event_id": event_id, "reason": "never mind" }))
        .send()
        .await
        .expect("cancel");
    assert_eq!(resp.status(), 200);

    assert_eq!(await_terminal(&listen, &event_id).await.1, "cancelled");
    let msgs = messages(&gw, &alice, 2).await;
    assert_eq!(msgs.len(), 2, "{msgs:#?}");
    assert_eq!(msgs[1]["metadata"]["status"], "cancelled");
}

#[tokio::test]
async fn a_static_token_runner_refuses_chat() {
    let gw = gateway().await;
    let alice = gw.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let (_runner, listen) = spawn_runner_with(&gw, false, &[]);

    let event_id = ask(&gw, &alice, "u-1", "Which customers renew?", &[]).await;
    let (run_id, status) = await_terminal(&listen, &event_id).await;
    assert_eq!(status, "dead_letter");
    let finished = run_finished(&gw, &run_id).await;
    assert_eq!(finished["reason"], "chat_requires_run_tokens", "{finished}");
    assert_eq!(finished["attempts"], 1, "no retry: it cannot succeed");
    // The runner's own identity never writes into a person's chat.
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(messages(&gw, &alice, 2).await.len(), 1);
}

#[tokio::test]
async fn a_chat_turn_is_answered_while_a_long_workflow_run_is_in_progress() {
    let gw = gateway().await;
    let alice = gw.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    // Only the renewal fold idles; chat runs answer at once.
    let (_runner, listen) = spawn_runner_with(
        &gw,
        true,
        &[
            ("ESCUREL_ECHO_SLEEP_MS", "20000"),
            ("ESCUREL_ECHO_SLEEP_SKILL", "renewal"),
        ],
    );
    let admin = gw.mint_token(TENANT, Role::Admin);
    let long = call(
        &gw,
        &admin,
        "capture_event",
        json!({ "source": "manual", "label_skill": "renewal", "instance_page_id": RENEWAL_PAGE,
                "title": "renew", "body": "a long job" }),
    )
    .await["event_id"]
        .as_str()
        .unwrap()
        .to_owned();
    // The long run is in flight on the serial lane.
    let url = format!("http://{listen}/debug/run?tenant={TENANT}&event_id={long}");
    let deadline = Instant::now() + Duration::from_secs(30);
    while reqwest::get(&url).await.ok().map(|r| r.status()) != Some(reqwest::StatusCode::OK) {
        assert!(Instant::now() < deadline, "the long run never started");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    tokio::time::sleep(Duration::from_millis(500)).await;

    let started = Instant::now();
    let turn = ask(&gw, &alice, "u-1", "Which customers renew?", &[]).await;
    assert_eq!(await_terminal(&listen, &turn).await.1, "processed");
    let msgs = messages(&gw, &alice, 2).await;
    assert_eq!(msgs.len(), 2, "{msgs:#?}");
    assert!(
        started.elapsed() < Duration::from_secs(12),
        "the chat turn waited for the long run: {:?}",
        started.elapsed()
    );
    let long_run: Value = reqwest::get(&url).await.unwrap().json().await.unwrap();
    assert_eq!(
        long_run["status"], "pending",
        "the long run is still working"
    );
    let metrics = reqwest::get(format!("http://{listen}/metrics"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        metrics.contains("escurel_runner_chat_turns_total{status=\"ok\"} 1"),
        "{metrics}"
    );
    assert!(metrics.contains("escurel_runner_chat_turn_seconds_count{status=\"ok\"} 1"));
}

/// A JSON-RPC call whose whole response (result or error) is returned.
async fn raw(gw: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    reqwest::Client::new()
        .post(gw.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": { "name": name, "arguments": args } }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json")
}

/// A gateway seeded with the shipped `chat_corpus()` and one conversation.
async fn corpus_gateway() -> EscurelProcess {
    let mut fixtures = FixtureBuilder::new().tenant(TENANT);
    for (page_id, body) in escurel_runner_workflow::corpus::chat_corpus() {
        fixtures = fixtures.page(&page_id, body);
    }
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        // Production's write ACL (the test harness defaults to `off`).
        config_overrides: ConfigOverrides {
            write_acl: Some(WriteAclMode::Enforce),
            ..Default::default()
        },
        fixtures: Some(
            fixtures
                .instance("conversation", "c-1", CONVERSATION)
                .done(),
        ),
    })
    .await
}

#[tokio::test]
async fn review_chat_run_creates_a_draft_and_lists_it_in_the_reply() {
    let gw = corpus_gateway().await;
    let alice = gw.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let before = call(
        &gw,
        &alice,
        "expand",
        json!({ "page_id": CONVERSATION_PAGE }),
    )
    .await;
    let (_runner, listen) = spawn_runner_with(&gw, true, &[("ESCUREL_ECHO_CHAT_DRAFT", "1")]);

    let event_id = ask(&gw, &alice, "u-1", "Add that Globex extended to 2028.", &[]).await;
    assert_eq!(await_terminal(&listen, &event_id).await.1, "processed");
    let msgs = messages(&gw, &alice, 2).await;
    let drafts = msgs[1]["metadata"]["drafts"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(drafts.len(), 1, "{msgs:#?}");

    let admin = gw.mint_token(TENANT, Role::Admin);
    let listed = call(&gw, &admin, "list_drafts", json!({})).await;
    let draft = listed["drafts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["draft_id"] == drafts[0])
        .cloned()
        .unwrap_or_else(|| panic!("draft {} not listed: {listed}", drafts[0]));
    assert_eq!(draft["target_page_id"], CONVERSATION_PAGE);
    // Held for review: the page itself is unchanged.
    let after = call(
        &gw,
        &alice,
        "expand",
        json!({ "page_id": CONVERSATION_PAGE }),
    )
    .await;
    assert_eq!(after["content_sha256"], before["content_sha256"]);
}

#[tokio::test]
async fn the_shipped_conversation_is_private_to_its_owner() {
    let gw = corpus_gateway().await;
    let alice = gw.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let bob = gw.mint_token_with_sub(TENANT, Role::Agent, "user:bob");
    let create = |owner: &str, id: &str| {
        json!({ "page_id": format!("markdown/instances/conversation/{id}.md"), "base_sha256": "",
                "content": format!("---\nkind: instance\nskill: conversation\nid: {id}\ntitle: New chat\n\
                    owner: \"{owner}\"\nstarted_at: 2026-10-25T11:00:00Z\n---\n# New chat\n") })
    };

    // The owner starts a conversation.
    let own = raw(&gw, &alice, "update_page", create(ALICE, "c-2")).await;
    assert_eq!(own["result"]["structuredContent"]["ok"], true, "{own}");
    // Someone else cannot start one in her name, read hers, or read its chat.
    let forged = raw(&gw, &bob, "update_page", create(ALICE, "c-3")).await;
    assert_ne!(
        forged["result"]["structuredContent"]["ok"], true,
        "{forged}"
    );
    let read = call(
        &gw,
        &bob,
        "expand",
        json!({ "page_id": "markdown/instances/conversation/c-2.md" }),
    )
    .await;
    assert!(read["page"].is_null(), "{read}");
    call(
        &gw,
        &alice,
        "append_message",
        json!({ "chat_group_id": group(), "role": "user", "content": "private", "embed": false }),
    )
    .await;
    let spy = call(
        &gw,
        &bob,
        "list_messages",
        json!({ "chat_group_id": group() }),
    )
    .await;
    assert_eq!(spy["messages"], json!([]));
}

#[tokio::test]
async fn a_timed_out_chat_run_says_it_took_too_long() {
    let gw = gateway().await;
    let alice = gw.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let (_runner, listen) = spawn_runner_with(
        &gw,
        true,
        &[
            ("ESCUREL_ECHO_SLEEP_MS", "10000"),
            ("ESCUREL_RUNNER_CHAT_TIMEOUT", "1s"),
        ],
    );
    let event_id = ask(&gw, &alice, "u-1", "Which customers renew?", &[]).await;
    let _ = await_terminal(&listen, &event_id).await;
    let msgs = messages(&gw, &alice, 2).await;
    assert_eq!(msgs.len(), 2, "{msgs:#?}");
    assert_eq!(msgs[1]["metadata"]["status"], "failed");
    assert_eq!(msgs[1]["metadata"]["reason"], "timeout", "{}", msgs[1]);
}
