//! `kind: user | system` and the lineage columns on the wire (knowledge-
//! workbench backend, P1 PR2). Real gateway, real DuckDB, raw JSON-RPC.
//!
//! A `system` event is bookkeeping about a run. It is admin-only to
//! capture (the runner and the gateway write them; a forged one would be
//! a forged run), skips the inbox when it names a page, and is hidden from
//! `list_inbox` / `list_events` unless `include_system` is asked for.
//! `root_event_id` / `run_id` are extracted from `provenance.runner` at
//! capture and answer `list_events{root_event_id}` / `{run_id}` directly.

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "carl";
const PAGE: &str = "markdown/instances/order/4500123.md";

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(FixtureBuilder::new().tenant(TENANT).done()),
        ..Default::default()
    })
    .await
}

async fn call(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    let resp = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": args },
        }))
        .send()
        .await
        .expect("post");
    assert_eq!(resp.status(), 200, "http status");
    resp.json().await.unwrap()
}

fn result(resp: &Value) -> &Value {
    assert!(resp.get("error").is_none(), "unexpected error: {resp}");
    &resp["result"]["structuredContent"]
}

fn ids(page: &Value) -> Vec<String> {
    page["events"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .map(|e| e["event_id"].as_str().unwrap().to_owned())
        .collect()
}

fn run_event(title: &str, root: &str, run: &str, target: Option<&str>) -> Value {
    let mut v = json!({
        "kind": "system",
        "label_skill": "escurel:run",
        "source": "escurel-runner",
        "title": title,
        "body": "{}",
        "provenance": { "runner": { "root_event_id": root, "run_id": run, "depth": 0 } },
    });
    if let Some(t) = target {
        v["instance_page_id"] = json!(t);
    }
    v
}

#[tokio::test]
async fn an_admin_captures_a_system_event_and_it_is_processed_on_the_page_immediately() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);

    let resp = call(
        &p,
        &admin,
        "capture_event",
        run_event("run-started", "R1", "X1", Some(PAGE)),
    )
    .await;
    let stored = result(&resp);
    assert_eq!(stored["kind"], "system");
    assert_eq!(
        stored["status"], "processed",
        "no assign_event round-trip: {stored}"
    );
    assert_eq!(stored["instance_page_id"], PAGE);
    assert_eq!(stored["root_event_id"], "R1");
    assert_eq!(stored["run_id"], "X1");

    // Hidden from the page's history by default, present when asked.
    let hidden = call(
        &p,
        &admin,
        "list_events",
        json!({ "instance_page_id": PAGE }),
    )
    .await;
    assert!(ids(result(&hidden)).is_empty(), "{hidden}");
    let shown = call(
        &p,
        &admin,
        "list_events",
        json!({ "instance_page_id": PAGE, "include_system": true }),
    )
    .await;
    assert_eq!(
        ids(result(&shown)),
        vec![stored["event_id"].as_str().unwrap()]
    );
}

#[tokio::test]
async fn a_non_admin_may_not_capture_a_system_event() {
    let p = start().await;
    let agent = p.mint_token(TENANT, Role::Agent);
    // Even under an ordinary label: `kind` is what makes it bookkeeping.
    let resp = call(
        &p,
        &agent,
        "capture_event",
        json!({ "kind": "system", "label_skill": "email", "title": "forged run" }),
    )
    .await;
    assert_eq!(resp["error"]["code"], -32602, "{resp}");
    // And an unknown kind is a caller mistake, not a silent user event.
    let resp = call(
        &p,
        &agent,
        "capture_event",
        json!({ "kind": "robot", "label_skill": "email", "title": "?" }),
    )
    .await;
    assert_eq!(resp["error"]["code"], -32602, "{resp}");
}

#[tokio::test]
async fn list_inbox_hides_system_events_unless_include_system() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);
    let agent = p.mint_token(TENANT, Role::Agent);

    let user = call(
        &p,
        &agent,
        "capture_event",
        json!({ "label_skill": "email", "title": "a human's event" }),
    )
    .await;
    let user_id = result(&user)["event_id"].as_str().unwrap().to_owned();
    assert_eq!(result(&user)["kind"], "user", "default kind on the wire");
    // No target: stays `inbox`, but is not inbox WORK.
    let sys = call(
        &p,
        &admin,
        "capture_event",
        run_event("runner-status", "R", "X", None),
    )
    .await;
    assert_eq!(result(&sys)["status"], "inbox");
    let sys_id = result(&sys)["event_id"].as_str().unwrap().to_owned();

    let inbox = call(&p, &admin, "list_inbox", json!({})).await;
    assert_eq!(ids(result(&inbox)), vec![user_id.clone()]);
    let all = call(&p, &admin, "list_inbox", json!({ "include_system": true })).await;
    let mut got = ids(result(&all));
    got.sort();
    let mut want = vec![user_id, sys_id];
    want.sort();
    assert_eq!(got, want);
}

/// A root, one cascade hop (a user event whose `provenance.runner` names
/// the root and the emitting run as `parent_run_id` — the runner's own
/// shape, `cascade::build_runner_provenance`), and one run event under it.
async fn seed_lineage(p: &EscurelProcess, admin: &str, agent: &str) -> (String, String, String) {
    let root = call(
        p,
        agent,
        "capture_event",
        json!({ "event_id": "01HROOT", "label_skill": "meeting", "title": "root", "at": "2026-09-22T09:00:00Z" }),
    )
    .await;
    let root_id = result(&root)["event_id"].as_str().unwrap().to_owned();
    let hop = call(
        p,
        agent,
        "capture_event",
        json!({
            "event_id": "01HHOP1", "label_skill": "decision-record", "title": "cascade hop",
            "at": "2026-09-22T09:05:00Z",
            "provenance": { "runner": { "root_event_id": root_id, "parent_event_id": root_id,
                                        "parent_run_id": "X1", "depth": 1,
                                        "lineage_path": [root_id] } },
        }),
    )
    .await;
    let hop_id = result(&hop)["event_id"].as_str().unwrap().to_owned();
    let mut run = run_event("run-started", &root_id, "X1", Some(PAGE));
    run["at"] = json!("2026-09-22T09:01:00Z");
    let run = call(p, admin, "capture_event", run).await;
    let run_id = result(&run)["event_id"].as_str().unwrap().to_owned();
    (root_id, hop_id, run_id)
}

#[tokio::test]
async fn list_events_by_root_event_id_returns_the_root_and_its_lineage_oldest_first() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);
    let agent = p.mint_token(TENANT, Role::Agent);
    let (root_id, hop_id, run_ev) = seed_lineage(&p, &admin, &agent).await;

    // Status-agnostic: the root is still in the inbox, the hop too.
    let tree = call(
        &p,
        &agent,
        "list_events",
        json!({ "root_event_id": root_id }),
    )
    .await;
    assert_eq!(
        ids(result(&tree)),
        vec![root_id.clone(), hop_id.clone()],
        "{tree}"
    );
    let with_runs = call(
        &p,
        &admin,
        "list_events",
        json!({ "root_event_id": root_id, "include_system": true }),
    )
    .await;
    assert_eq!(
        ids(result(&with_runs)),
        vec![root_id.clone(), run_ev, hop_id]
    );
    // Exactly one selector.
    let both = call(
        &p,
        &agent,
        "list_events",
        json!({ "root_event_id": root_id, "instance_page_id": PAGE }),
    )
    .await;
    assert_eq!(both["error"]["code"], -32602, "{both}");
}

#[tokio::test]
async fn list_events_by_run_id_returns_only_that_runs_system_events() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);
    let agent = p.mint_token(TENANT, Role::Agent);
    let (_root, _hop, run_ev) = seed_lineage(&p, &admin, &agent).await;
    // A run has only system events, so `run_id` implies `include_system`.
    let run = call(&p, &admin, "list_events", json!({ "run_id": "X1" })).await;
    assert_eq!(ids(result(&run)), vec![run_ev], "{run}");
    let none = call(&p, &admin, "list_events", json!({ "run_id": "nope" })).await;
    assert!(ids(result(&none)).is_empty());
}

#[tokio::test]
async fn lineage_columns_are_extracted_from_provenance_runner_at_capture() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);
    let agent = p.mint_token(TENANT, Role::Agent);
    let (root_id, hop_id, _run) = seed_lineage(&p, &admin, &agent).await;

    let hop = call(&p, &agent, "list_events", json!({ "event_id": hop_id })).await;
    let hop = &result(&hop)["events"][0];
    assert_eq!(hop["kind"], "user");
    assert_eq!(
        hop["root_event_id"], root_id,
        "from provenance.runner: {hop}"
    );
    // A hop is EMITTED BY run X1 (`parent_run_id`); it is not X1's own
    // record, so it carries no `run_id` — `list_events{run_id}` stays the
    // run's own events.
    assert!(hop["run_id"].is_null(), "{hop}");
    // The root is its own root and belongs to no run.
    let root = call(&p, &agent, "list_events", json!({ "event_id": root_id })).await;
    let root = &result(&root)["events"][0];
    assert_eq!(root["root_event_id"], root_id);
    assert!(root["run_id"].is_null(), "{root}");
}
