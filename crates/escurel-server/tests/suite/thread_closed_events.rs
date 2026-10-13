//! Closing a thread (VS Code workbench PR-9). Closing is a DURABLE event in
//! the gateway, not per-window state: a human files an
//! `escurel:thread-closed` event naming the thread's root, and the
//! extension's Threads outline folds those events into a "Closed" group.
//!
//! The closest precedent is `escurel:run-control` (`run_control_events.rs`):
//!
//! - `close` / `reopen` name a root event in the body. The caller must be
//!   allowed to READ the root (the same visibility rule as the thread
//!   itself), and must not be a run-bound machine token. An unknown root
//!   and an unreadable one read the same: `event_not_found` (no existence
//!   oracle).
//! - `provenance.thread.requested_by` is stamped server-side from the
//!   token; the request is stored as bookkeeping (`kind: system`) on the
//!   root's page, so it never shows as inbox work and turns up both under
//!   the label (`list_events{label_skill}` — the outline's read path, no
//!   new tool) and in the thread's own record (`list_events{root_event_id}`).
//! - Closing a closed thread (reopening an open one) is a no-op answer,
//!   not an error. Nothing is written to the thread's instance pages.
//!
//! Real gateway, real DuckDB, raw JSON-RPC; `WriteAclMode::Enforce`.

use escurel_server::WriteAclMode;
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "stuttgart-ai";
const ALICE: &str = "whatsapp:111";
const BOB: &str = "whatsapp:222";
const MEMBER_SKILL: &str = "---\nkind: skill\nid: community_member\n\
    description: A member.\nvisibility: owner\nowner_field: credential\n---\n# community_member\n";
const ALICE_MEMBER: &str = "---\nkind: instance\nskill: community_member\nid: alice\n\
    credential: \"whatsapp:111\"\n---\n# Alice\n";
const ALICE_PAGE: &str = "markdown/instances/community_member/alice.md";
const ROOT: &str = "01HTHREADROOT00000000000000";
const RUN: &str = "01HTHREADRUN000000000000000";

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            write_acl: Some(WriteAclMode::Enforce),
            ..Default::default()
        },
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("community_member", MEMBER_SKILL)
                .instance("community_member", "alice", ALICE_MEMBER)
                .done(),
        ),
    })
    .await
}

async fn rpc(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    reqwest::Client::new()
        .post(p.mcp_url())
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

async fn call(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    let body = rpc(p, token, name, args).await;
    assert!(body.get("error").is_none(), "{name}: {body}");
    body["result"]["structuredContent"].clone()
}

async fn call_err(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    let body = rpc(p, token, name, args).await;
    assert!(body.get("error").is_some(), "{name} should fail: {body}");
    body["error"].clone()
}

/// The thread to close: a user event on Alice's page, filed by an admin.
async fn seed_root(p: &EscurelProcess, admin: &str) {
    call(
        p,
        admin,
        "capture_event",
        json!({
            "event_id": ROOT,
            "source": "workbench", "mime": "application/json",
            "label_skill": "community_member", "title": "a question",
            "instance_page_id": ALICE_PAGE, "body": "{}",
        }),
    )
    .await;
}

fn close(action: &str, root: Option<&str>) -> Value {
    let mut body = json!({ "action": action, "reason": "done with this" });
    if let Some(r) = root {
        body["root_event_id"] = json!(r);
    }
    json!({
        "source": "workbench", "mime": "application/json",
        "label_skill": "escurel:thread-closed", "title": action,
        "body": body.to_string(),
        // A caller-supplied requester is overwritten, never trusted.
        "provenance": { "thread": { "requested_by": "mallory" } },
    })
}

async fn label_events(p: &EscurelProcess, admin: &str) -> Vec<Value> {
    call(
        p,
        admin,
        "list_events",
        json!({ "label_skill": "escurel:thread-closed" }),
    )
    .await["events"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

#[tokio::test]
async fn a_reader_may_close_a_thread_and_the_request_is_stamped() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    seed_root(&p, &admin).await;

    let r = call(&p, &alice, "capture_event", close("close", Some(ROOT))).await;
    let id = r["event_id"].as_str().expect("event_id").to_owned();
    assert_eq!(r["kind"], "system", "a close request is bookkeeping: {r}");
    assert_eq!(r["status"], "processed", "never inbox work: {r}");
    assert_eq!(
        r["instance_page_id"], ALICE_PAGE,
        "filed on the root's page, into the thread's own record: {r}"
    );
    assert_eq!(r["root_event_id"], ROOT, "{r}");
    assert!(r["run_id"].is_null(), "a close names no run: {r}");
    let thread = &r["provenance"]["thread"];
    assert_eq!(thread["requested_by"], ALICE, "server-stamped: {r}");
    assert_eq!(thread["action"], "close");
    assert_eq!(thread["root_event_id"], ROOT);
    assert_eq!(thread["reason"], "done with this");

    // Read back where the outline looks: under the label, and in the
    // thread's own record — but never as inbox work.
    let by_label = label_events(&p, &admin).await;
    assert!(by_label.iter().any(|e| e["event_id"] == id), "{by_label:?}");
    // (The root branch hides system rows unless asked: the outline reads
    // via the label, which implies `include_system` — see `label_events`.)
    let by_root = call(
        &p,
        &admin,
        "list_events",
        json!({ "root_event_id": ROOT, "include_system": true }),
    )
    .await;
    assert!(
        by_root["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["event_id"] == id),
        "{by_root}"
    );
    let inbox = call(&p, &admin, "list_inbox", json!({})).await;
    assert!(
        inbox["events"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["event_id"] != id),
        "{inbox}"
    );
}

#[tokio::test]
async fn closing_a_closed_thread_is_a_no_op_not_an_error() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    seed_root(&p, &admin).await;

    call(&p, &alice, "capture_event", close("close", Some(ROOT))).await;
    let again = call(&p, &alice, "capture_event", close("close", Some(ROOT))).await;
    assert_eq!(again["provenance"]["thread"]["action"], "close", "{again}");
    assert_eq!(again["root_event_id"], ROOT, "{again}");
    let by_label = label_events(&p, &admin).await;
    assert_eq!(by_label.len(), 1, "no second row was filed: {by_label:?}");

    // Reopening an open thread is the same no-op the other way.
    let open = call(&p, &alice, "capture_event", close("reopen", Some(ROOT))).await;
    assert_eq!(open["provenance"]["thread"]["action"], "reopen", "{open}");
    let open_again = call(&p, &alice, "capture_event", close("reopen", Some(ROOT))).await;
    assert_eq!(
        open_again["provenance"]["thread"]["action"], "reopen",
        "{open_again}"
    );
    let by_label = label_events(&p, &admin).await;
    assert_eq!(
        by_label.len(),
        2,
        "close + reopen, nothing more: {by_label:?}"
    );
}

#[tokio::test]
async fn a_stranger_and_an_unknown_root_read_the_same_event_not_found() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let bob = p.mint_token_with_sub(TENANT, Role::Agent, BOB);
    seed_root(&p, &admin).await;

    let denied = call_err(&p, &bob, "capture_event", close("close", Some(ROOT))).await;
    let unknown = call_err(
        &p,
        &alice,
        "capture_event",
        close("close", Some("01HNO_SUCH_ROOT000000000000")),
    )
    .await;
    for (name, e) in [("stranger", &denied), ("unknown root", &unknown)] {
        assert_eq!(e["code"], -32602, "{name}: {e}");
        assert_eq!(e["data"]["code"], "event_not_found", "{name}: {e}");
        assert_eq!(e["data"]["retryable"], false, "{name}: {e}");
    }
    assert_eq!(denied["message"], unknown["message"], "no existence oracle");
    assert_eq!(
        label_events(&p, &admin).await.len(),
        0,
        "nothing was written"
    );
}

#[tokio::test]
async fn a_run_bound_machine_token_cannot_close_a_thread_it_can_read() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);
    seed_root(&p, &admin).await;

    // Alice's own subject, so the root IS readable: only the run binding refuses.
    let machine = p.mint_token_for_run(TENANT, Role::Agent, ALICE, RUN, ROOT);
    let e = call_err(&p, &machine, "capture_event", close("close", Some(ROOT))).await;
    assert_eq!(e["code"], -32602, "{e}");
    assert_eq!(e["data"]["code"], "event_not_found", "{e}");
    assert_eq!(
        label_events(&p, &admin).await.len(),
        0,
        "nothing was written"
    );
}

#[tokio::test]
async fn a_malformed_close_request_is_a_caller_mistake() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);
    seed_root(&p, &admin).await;

    let bad_action = call_err(&p, &admin, "capture_event", close("hide", Some(ROOT))).await;
    let no_root = call_err(&p, &admin, "capture_event", close("close", None)).await;
    let mut not_json = close("close", Some(ROOT));
    not_json["body"] = json!("close it");
    let not_json = call_err(&p, &admin, "capture_event", not_json).await;
    for (name, e) in [
        ("action", &bad_action),
        ("no root", &no_root),
        ("not json", &not_json),
    ] {
        assert_eq!(e["code"], -32602, "{name}: {e}");
        assert_ne!(
            e["data"]["code"], "event_not_found",
            "{name} is a mistake, not a denial: {e}"
        );
        assert!(
            e["message"].as_str().unwrap().contains("thread-closed"),
            "{name}: {e}"
        );
    }
    assert_eq!(
        label_events(&p, &admin).await.len(),
        0,
        "nothing was written"
    );
}

// ------------------------------------------------------------ review hardening (Claude) ---

/// Everything a caller sends in `provenance` except the server's own `thread` block is dropped:
/// a close request must not smuggle a runner lineage, a review block or another requester into
/// the thread's record (downstream readers interpret those keys).
#[tokio::test]
async fn a_close_request_keeps_nothing_of_the_callers_provenance_but_the_thread_block() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    seed_root(&p, &admin).await;

    let mut req = close("close", Some(ROOT));
    req["provenance"] = json!({
        "thread": { "requested_by": "mallory", "action": "reopen", "root_event_id": "01HOTHERROOT0000000000000" },
        "runner": { "root_event_id": "01HOTHERROOT0000000000000", "parent_run_id": RUN, "cause": "forged" },
        "review": { "draft_id": "d1", "decision": "approved" },
        "control": { "action": "cancel" },
    });
    let r = call(&p, &alice, "capture_event", req).await;
    let prov = r["provenance"].as_object().expect("provenance object");
    for forbidden in ["runner", "review", "control", "workflow"] {
        assert!(
            !prov.contains_key(forbidden),
            "caller-supplied `{forbidden}` must not survive: {r}"
        );
    }
    assert_eq!(r["provenance"]["thread"]["requested_by"], ALICE, "{r}");
    assert_eq!(r["provenance"]["thread"]["action"], "close", "{r}");
    assert_eq!(r["provenance"]["thread"]["root_event_id"], ROOT, "{r}");
    assert_eq!(
        r["root_event_id"], ROOT,
        "lineage comes from the root, not provenance: {r}"
    );
}

/// The reason is a human's free text: bounded at the gateway. The root's title is the server's
/// own data but may be long: it is stamped truncated, never refused.
#[tokio::test]
async fn a_long_reason_is_refused_and_a_long_root_title_is_stamped_truncated() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    call(
        &p,
        &admin,
        "capture_event",
        json!({
            "event_id": ROOT,
            "source": "workbench", "mime": "application/json",
            "label_skill": "community_member", "title": "T".repeat(600),
            "instance_page_id": ALICE_PAGE, "body": "{}",
        }),
    )
    .await;

    let mut long = close("close", Some(ROOT));
    let mut body: Value = serde_json::from_str(long["body"].as_str().unwrap()).unwrap();
    body["reason"] = json!("r".repeat(201));
    long["body"] = json!(body.to_string());
    let e = call_err(&p, &alice, "capture_event", long).await;
    assert_eq!(e["code"], -32602, "{e}");
    assert_ne!(
        e["data"]["code"], "event_not_found",
        "a mistake, not a denial: {e}"
    );
    assert_eq!(
        label_events(&p, &admin).await.len(),
        0,
        "nothing was written"
    );

    let mut ok = close("close", Some(ROOT));
    let mut body: Value = serde_json::from_str(ok["body"].as_str().unwrap()).unwrap();
    body["reason"] = json!("r".repeat(200));
    ok["body"] = json!(body.to_string());
    let r = call(&p, &alice, "capture_event", ok).await;
    let title = r["provenance"]["thread"]["root_title"]
        .as_str()
        .expect("root_title");
    assert!(
        title.chars().count() <= 200,
        "root_title is capped: {}",
        title.chars().count()
    );
    assert_eq!(
        r["provenance"]["thread"]["reason"]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        200,
        "{r}"
    );
}

/// The carve-out admits `escurel:thread-closed` and nothing else: a human without admin still
/// cannot file the runner's or the server's bookkeeping labels, nor a `kind: system` event under
/// an ordinary label (these refusals pre-date this change: pinned here so the carve-out cannot
/// widen them).
#[tokio::test]
async fn the_carve_out_does_not_widen_the_reserved_namespace_for_a_human() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    seed_root(&p, &admin).await;

    for label in [
        "escurel:run-status",
        "escurel:runner-status",
        "escurel:thread-closed-extra",
    ] {
        let e = call_err(
            &p,
            &alice,
            "capture_event",
            json!({
                "source": "workbench", "mime": "application/json",
                "label_skill": label, "title": "x",
                "instance_page_id": ALICE_PAGE,
                "body": json!({ "action": "close", "root_event_id": ROOT }).to_string(),
            }),
        )
        .await;
        assert_eq!(e["code"], -32602, "{label}: {e}");
    }
    let e = call_err(
        &p,
        &alice,
        "capture_event",
        json!({
            "source": "workbench", "mime": "application/json",
            "label_skill": "community_member", "title": "x", "kind": "system",
            "instance_page_id": ALICE_PAGE, "body": "{}",
        }),
    )
    .await;
    assert_eq!(
        e["code"], -32602,
        "kind: system under an ordinary label: {e}"
    );
    assert_eq!(
        label_events(&p, &admin).await.len(),
        0,
        "nothing was written"
    );
}

/// Two humans (or one human twice) closing at once: both calls succeed, every stored row says
/// `close`, and a later reopen is the newest row, so the outline's fold ends open.
#[tokio::test]
async fn concurrent_closes_leave_a_state_the_fold_can_read() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    seed_root(&p, &admin).await;

    let (a, b, c, d) = tokio::join!(
        rpc(&p, &alice, "capture_event", close("close", Some(ROOT))),
        rpc(&p, &alice, "capture_event", close("close", Some(ROOT))),
        rpc(&p, &alice, "capture_event", close("close", Some(ROOT))),
        rpc(&p, &alice, "capture_event", close("close", Some(ROOT))),
    );
    for r in [&a, &b, &c, &d] {
        assert!(
            r.get("error").is_none(),
            "a concurrent close never errors: {r}"
        );
    }
    let rows = label_events(&p, &admin).await;
    assert!(!rows.is_empty() && rows.len() <= 4, "{rows:?}");
    assert!(
        rows.iter()
            .all(|e| e["provenance"]["thread"]["action"] == "close"),
        "{rows:?}"
    );

    call(&p, &alice, "capture_event", close("reopen", Some(ROOT))).await;
    let newest_first = call(
        &p,
        &admin,
        "list_events",
        json!({ "label_skill": "escurel:thread-closed", "newest_first": true }),
    )
    .await;
    assert_eq!(
        newest_first["events"][0]["provenance"]["thread"]["action"], "reopen",
        "the reopen is the newest row: {newest_first}"
    );
}
