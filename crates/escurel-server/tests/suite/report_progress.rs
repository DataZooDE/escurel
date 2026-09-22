//! `report_progress` (knowledge-workbench backend, P1 PR6 — BRD FR-P-1/3).
//! Real gateway, real DuckDB, raw JSON-RPC plus the typed client.
//!
//! An agent reports its whole plan as a snapshot; the gateway writes it as
//! a `run-progress` system event under `escurel:run` for the run the
//! caller's TOKEN names — the only way to say which run, so a token that
//! belongs to no run is refused, admin or not. The same plan reported
//! twice is one event; a run keeps its last fifty snapshots.

use escurel_client::SecretString;
use escurel_client::{Client, PlanStep, ReportProgressRequest};
use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "carl";
const PAGE: &str = "markdown/instances/order/4500123.md";
const RUN: &str = "01HRUNXXXXXXXXXXXXXXXXXXXX";
const ROOT: &str = "01HROOTXXXXXXXXXXXXXXXXXXX";

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

fn plan(steps: &[(&str, &str)]) -> Value {
    json!(
        steps
            .iter()
            .map(|(s, st)| json!({ "step": s, "status": st }))
            .collect::<Vec<_>>()
    )
}

/// A run's own events, as the admin sees them.
async fn run_events(p: &EscurelProcess, run: &str) -> Vec<Value> {
    let admin = p.mint_token(TENANT, Role::Admin);
    let r = call(p, &admin, "list_events", json!({ "run_id": run })).await;
    result(&r)["events"].as_array().cloned().unwrap_or_default()
}

#[tokio::test]
async fn report_progress_is_refused_without_a_run_claim_even_for_admin() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);
    let r = call(
        &p,
        &admin,
        "report_progress",
        json!({ "plan": plan(&[("a", "pending")]) }),
    )
    .await;
    assert_eq!(r["error"]["code"], -32602, "{r}");
    assert!(
        r["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("run"),
        "{r}"
    );
    // A run-bound token with a status outside the enum is a caller mistake.
    let agent = p.mint_token_for_run(TENANT, Role::Agent, "agent:note", RUN, ROOT);
    let r = call(
        &p,
        &agent,
        "report_progress",
        json!({ "plan": plan(&[("a", "flying")]) }),
    )
    .await;
    assert_eq!(r["error"]["code"], -32602, "{r}");
    assert!(run_events(&p, RUN).await.is_empty());
}

#[tokio::test]
async fn report_progress_writes_a_run_progress_system_event_stamped_with_the_token_run() {
    let p = start().await;
    // A NON-admin run-bound token: the tool must not lean on the agent
    // token's admin role to write under the reserved namespace.
    let agent = p.mint_token_for_run(TENANT, Role::Agent, "agent:note", RUN, ROOT);
    let r = call(
        &p,
        &agent,
        "report_progress",
        json!({
            "plan": plan(&[("read the order", "completed"), ("assess risk", "in_progress"), ("write summary", "pending")]),
            "current": "assess risk",
            "note": "supplier GH-4711 flagged",
        }),
    )
    .await;
    let out = result(&r);
    assert_eq!(out["ok"], true, "{out}");
    assert_eq!(out["run_id"], RUN);
    assert_eq!(out["steps"], 3);

    let events = run_events(&p, RUN).await;
    assert_eq!(events.len(), 1, "{events:?}");
    let e = &events[0];
    assert_eq!(e["event_id"], out["event_id"]);
    assert_eq!(e["kind"], "system");
    assert_eq!(e["label_skill"], "escurel:run");
    assert_eq!(e["title"], "run-progress");
    assert_eq!(e["root_event_id"], ROOT);
    assert_eq!(e["run_id"], RUN);
    assert_eq!(e["provenance"]["runner"]["run_id"], RUN, "{e}");
    assert_eq!(e["provenance"]["runner"]["root_event_id"], ROOT);
    assert_eq!(e["provenance"]["runner"]["reported_by"], "agent:note");
    let body: Value = serde_json::from_str(e["body"].as_str().unwrap()).unwrap();
    assert_eq!(body["current"], "assess risk");
    assert_eq!(body["note"], "supplier GH-4711 flagged");
    assert_eq!(body["plan"][1]["status"], "in_progress");
    // The agent's own view: it is hidden from the inbox, as bookkeeping is.
    let inbox = call(&p, &agent, "list_inbox", json!({})).await;
    assert!(result(&inbox)["events"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn the_same_plan_reported_twice_is_one_event() {
    let p = start().await;
    let agent = p.mint_token_for_run(TENANT, Role::Agent, "agent:note", RUN, ROOT);
    let snapshot = json!({ "plan": plan(&[("a", "completed"), ("b", "pending")]), "current": "b" });
    let first = call(&p, &agent, "report_progress", snapshot.clone()).await;
    let again = call(&p, &agent, "report_progress", snapshot).await;
    assert_eq!(result(&first)["event_id"], result(&again)["event_id"]);
    assert_eq!(run_events(&p, RUN).await.len(), 1);
    // A changed plan is a new snapshot.
    call(
        &p,
        &agent,
        "report_progress",
        json!({ "plan": plan(&[("a", "completed"), ("b", "completed")]) }),
    )
    .await;
    assert_eq!(run_events(&p, RUN).await.len(), 2);
}

#[tokio::test]
async fn run_progress_is_pruned_to_the_last_fifty_per_run() {
    let p = start().await;
    let agent = p.mint_token_for_run(TENANT, Role::Agent, "agent:note", RUN, ROOT);
    for i in 0..55 {
        let r = call(
            &p,
            &agent,
            "report_progress",
            json!({ "plan": plan(&[(&format!("s{i}"), "in_progress")]) }),
        )
        .await;
        assert_eq!(result(&r)["ok"], true, "{r}");
    }
    let events = run_events(&p, RUN).await;
    assert_eq!(events.len(), 50, "bounded per run");
    let bodies: Vec<String> = events
        .iter()
        .map(|e| e["body"].as_str().unwrap().to_owned())
        .collect();
    assert!(
        bodies.iter().any(|b| b.contains("\"s54\"")),
        "the latest survives"
    );
    assert!(
        !bodies.iter().any(|b| b.contains("\"s0\"")),
        "the oldest is pruned"
    );
    // Another run is untouched by this run's pruning.
    let other = p.mint_token_for_run(TENANT, Role::Agent, "agent:note", "01HOTHERRUN", ROOT);
    call(
        &p,
        &other,
        "report_progress",
        json!({ "plan": plan(&[("x", "pending")]) }),
    )
    .await;
    assert_eq!(run_events(&p, "01HOTHERRUN").await.len(), 1);
    assert_eq!(run_events(&p, RUN).await.len(), 50);
}

#[tokio::test]
async fn run_progress_is_attached_to_the_runs_target_page_when_run_started_exists() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);
    let agent = p.mint_token_for_run(TENANT, Role::Agent, "agent:note", RUN, ROOT);
    // Before the runner has announced the run: nowhere to attach.
    let r = call(
        &p,
        &agent,
        "report_progress",
        json!({ "plan": plan(&[("a", "pending")]) }),
    )
    .await;
    let orphan = result(&r)["event_id"].as_str().unwrap().to_owned();
    // The runner's `run-started` names the target page (as the run-event
    // writer will capture it).
    call(
        &p,
        &admin,
        "capture_event",
        json!({
            "kind": "system", "label_skill": "escurel:run", "title": "run-started",
            "instance_page_id": PAGE, "source": "escurel-runner",
            "provenance": { "runner": { "run_id": RUN, "root_event_id": ROOT, "target_page_id": PAGE } },
        }),
    )
    .await;
    let r = call(
        &p,
        &agent,
        "report_progress",
        json!({ "plan": plan(&[("a", "completed")]) }),
    )
    .await;
    let attached = result(&r)["event_id"].as_str().unwrap().to_owned();

    let by_id = |id: String| {
        let admin = admin.clone();
        let p = &p;
        async move {
            let r = call(p, &admin, "list_events", json!({ "event_id": id })).await;
            result(&r)["events"][0].clone()
        }
    };
    let o = by_id(orphan).await;
    assert!(o["instance_page_id"].is_null(), "{o}");
    assert_eq!(o["status"], "inbox");
    let a = by_id(attached).await;
    assert_eq!(a["instance_page_id"], PAGE, "{a}");
    assert_eq!(a["status"], "processed");
    // And the page's history shows it, when asked for system rows.
    let hist = call(
        &p,
        &admin,
        "list_events",
        json!({ "instance_page_id": PAGE, "include_system": true }),
    )
    .await;
    assert_eq!(
        result(&hist)["events"].as_array().unwrap().len(),
        2,
        "{hist}"
    );
}

#[tokio::test]
async fn the_typed_client_reports_progress() {
    let p = start().await;
    let token = p.mint_token_for_run(TENANT, Role::Agent, "agent:note", RUN, ROOT);
    let client = Client::connect(p.base_url(), SecretString::from(token))
        .await
        .expect("connect");
    let resp = client
        .report_progress(ReportProgressRequest {
            plan: vec![
                PlanStep {
                    step: "a".into(),
                    status: "completed".into(),
                },
                PlanStep {
                    step: "b".into(),
                    status: "blocked".into(),
                },
            ],
            current: "b".into(),
            note: String::new(),
        })
        .await
        .expect("report_progress");
    assert!(resp.ok);
    assert_eq!(resp.run_id, RUN);
    assert_eq!(resp.steps, 2);
    assert!(!resp.event_id.is_empty());
}
