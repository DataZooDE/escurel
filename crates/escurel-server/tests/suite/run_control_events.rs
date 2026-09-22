//! Controls as events (knowledge-workbench backend P2-2). A human cancels,
//! retries, pauses, resumes or requeues by capturing an
//! `escurel:run-control` event; the runner's subscriber acts on it. The
//! gateway stays automation-free — it only AUTHORISES the request and
//! stamps who made it:
//!
//! - `cancel` / `retry` name a run; the caller must be allowed to WRITE the
//!   run's target page (the page its `run-started` names), under the same
//!   `ESCUREL_WRITE_ACL` gate as `update_page`. An unknown run and a
//!   forbidden one read the same: `event_not_found` (no existence oracle).
//! - `pause` / `resume` / `requeue` are admin-only; a non-admin gets the
//!   same `event_not_found`.
//! - `provenance.control.requested_by` is stamped server-side from the
//!   token; the request is stored as bookkeeping (`kind: system`) on the
//!   run's target page, so it never shows as inbox work and turns up in
//!   the run's own record (`list_events{run_id}`) and under the label.
//!
//! Real gateway, real DuckDB, raw JSON-RPC; `WriteAclMode::Enforce`.

use escurel_server::WriteAclMode;
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "stuttgart-ai";
const ALICE: &str = "whatsapp:111";
const BOB: &str = "whatsapp:222";
const MEMBER_SKILL: &str = "---\ntype: skill\nid: community_member\n\
    description: A member.\nvisibility: owner\nowner_field: credential\n---\n# community_member\n";
const ALICE_MEMBER: &str = "---\ntype: instance\nskill: community_member\nid: alice\n\
    credential: \"whatsapp:111\"\n---\n# Alice\n";
const ALICE_PAGE: &str = "markdown/instances/community_member/alice.md";
const RUN: &str = "01HRUNALICE0000000000000000";
const ROOT: &str = "01HROOTALICE000000000000000";

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

/// The run the human acts on: the runner announced it on Alice's page.
async fn seed_run(p: &EscurelProcess, admin: &str) {
    call(
        p,
        admin,
        "capture_event",
        json!({
            "event_id": format!("run:{RUN}:started"),
            "kind": "system", "source": "escurel-runner", "mime": "application/json",
            "label_skill": "escurel:run", "title": "run-started",
            "instance_page_id": ALICE_PAGE, "body": "{}",
            "provenance": { "runner": { "run_id": RUN, "root_event_id": ROOT } },
        }),
    )
    .await;
}

fn control(action: &str, run_id: Option<&str>) -> Value {
    let mut body = json!({ "action": action, "reason": "wrong document" });
    if let Some(r) = run_id {
        body["run_id"] = json!(r);
    }
    json!({
        "source": "workbench", "mime": "application/json",
        "label_skill": "escurel:run-control", "title": action,
        "body": body.to_string(),
        // A caller-supplied requester is overwritten, never trusted.
        "provenance": { "control": { "requested_by": "mallory" } },
    })
}

#[tokio::test]
async fn the_pages_owner_may_cancel_a_run_on_it_and_the_request_is_stamped() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    seed_run(&p, &admin).await;

    let r = call(&p, &alice, "capture_event", control("cancel", Some(RUN))).await;
    let id = r["event_id"].as_str().expect("event_id").to_owned();
    assert_eq!(r["kind"], "system", "a control request is bookkeeping: {r}");
    assert_eq!(r["status"], "processed", "never inbox work: {r}");
    assert_eq!(
        r["instance_page_id"], ALICE_PAGE,
        "attached to the run's target page: {r}"
    );
    assert_eq!(r["run_id"], RUN, "{r}");
    assert_eq!(r["root_event_id"], ROOT, "{r}");
    let control = &r["provenance"]["control"];
    assert_eq!(control["requested_by"], ALICE, "server-stamped: {r}");
    assert_eq!(control["action"], "cancel");
    assert_eq!(control["run_id"], RUN);

    // Read back where the runner's subscriber and the workbench look.
    let by_label = call(
        &p,
        &admin,
        "list_events",
        json!({ "label_skill": "escurel:run-control" }),
    )
    .await;
    assert!(
        by_label["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["event_id"] == id),
        "{by_label}"
    );
    let by_run = call(&p, &admin, "list_events", json!({ "run_id": RUN })).await;
    assert!(
        by_run["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["event_id"] == id),
        "{by_run}"
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
async fn a_stranger_and_an_unknown_run_read_the_same_event_not_found() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let bob = p.mint_token_with_sub(TENANT, Role::Agent, BOB);
    seed_run(&p, &admin).await;

    let denied = call_err(&p, &bob, "capture_event", control("cancel", Some(RUN))).await;
    let unknown = call_err(
        &p,
        &alice,
        "capture_event",
        control("retry", Some("01HNOSUCHRUN000000000000000")),
    )
    .await;
    for (name, e) in [("stranger", &denied), ("unknown run", &unknown)] {
        assert_eq!(e["code"], -32602, "{name}: {e}");
        assert_eq!(e["data"]["code"], "event_not_found", "{name}: {e}");
        assert_eq!(e["data"]["retryable"], false, "{name}: {e}");
    }
    assert_eq!(denied["message"], unknown["message"], "no existence oracle");
    // Nothing was written either way.
    let by_label = call(
        &p,
        &admin,
        "list_events",
        json!({ "label_skill": "escurel:run-control" }),
    )
    .await;
    assert_eq!(
        by_label["events"].as_array().unwrap().len(),
        0,
        "{by_label}"
    );
}

/// The runner tails the label by `(at, event_id)`; a request backdated by
/// its caller would sort before the tail's cursor and never be acted on
/// (codex second-opinion review of P2). Control requests get the server's
/// clock, whatever the caller sent.
#[tokio::test]
async fn a_control_request_is_stamped_with_the_servers_clock() {
    let p = start().await;
    let ops = p.mint_token_with_sub(TENANT, Role::Admin, "ops:jo");
    let mut req = control("pause", None);
    req["at"] = json!("2000-01-01T00:00:00Z");
    let r = call(&p, &ops, "capture_event", req).await;
    let at = r["at"].as_str().unwrap_or("");
    assert!(
        at.starts_with("20") && !at.starts_with("2000-01-01"),
        "server time, not the caller's: {r}"
    );
}

#[tokio::test]
async fn pause_resume_and_requeue_are_admin_only() {
    let p = start().await;
    let ops = p.mint_token_with_sub(TENANT, Role::Admin, "ops:jo");
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);

    for action in ["pause", "resume", "requeue"] {
        let e = call_err(&p, &alice, "capture_event", control(action, None)).await;
        assert_eq!(e["data"]["code"], "event_not_found", "{action}: {e}");
    }
    let r = call(&p, &ops, "capture_event", control("pause", None)).await;
    assert_eq!(r["kind"], "system", "{r}");
    assert!(
        r["instance_page_id"].as_str().unwrap_or("").is_empty(),
        "tenant-wide, no page: {r}"
    );
    assert_eq!(r["provenance"]["control"]["requested_by"], "ops:jo", "{r}");
    assert_eq!(r["provenance"]["control"]["action"], "pause");
    let by_label = call(
        &p,
        &ops,
        "list_events",
        json!({ "label_skill": "escurel:run-control" }),
    )
    .await;
    assert_eq!(
        by_label["events"].as_array().unwrap().len(),
        1,
        "{by_label}"
    );
}

#[tokio::test]
async fn a_malformed_control_request_is_a_caller_mistake() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);
    seed_run(&p, &admin).await;

    // Unknown action, cancel without a run, and a body that is not JSON.
    let bad_action = call_err(&p, &admin, "capture_event", control("explode", Some(RUN))).await;
    let no_run = call_err(&p, &admin, "capture_event", control("cancel", None)).await;
    let mut not_json = control("cancel", Some(RUN));
    not_json["body"] = json!("cancel it");
    let not_json = call_err(&p, &admin, "capture_event", not_json).await;
    for (name, e) in [
        ("action", &bad_action),
        ("no run", &no_run),
        ("not json", &not_json),
    ] {
        assert_eq!(e["code"], -32602, "{name}: {e}");
        assert_ne!(
            e["data"]["code"], "event_not_found",
            "{name} is a mistake, not a denial: {e}"
        );
        assert!(
            e["message"].as_str().unwrap().contains("run-control"),
            "{name}: {e}"
        );
    }
}
