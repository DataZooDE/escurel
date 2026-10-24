//! Plan mode (knowledge-workbench backend P2-5b — BRD FR-M-2). A manual
//! start with `mode: plan` runs the harness on a no-write tool surface and
//! asks it to report its plan and stop: the run ends `planned` (terminal,
//! nothing landed, the event still in the inbox) and its `run-finished`
//! carries the plan. A second manual start naming that run as
//! `approved_plan_run_id` gets the plan injected into its input and runs
//! for real.
//!
//! Real gateway, real runner in MINTED mode (the plan is reported through
//! `report_progress`, which needs a run-bound token), real echo harness.

use std::process::{Child, Command};
use std::time::{Duration, Instant};

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role, free_port};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const SKILL_BODY: &str = "---\nkind: skill\nid: renewal\nautonomy: auto\n---\n# renewal\n\nFold the event into the instance.\n";
const INSTANCE_BODY: &str = "---\nkind: instance\nid: c1\nskill: renewal\n---\n# C1\n\nBASELINE.\n";
const PAGE: &str = "markdown/instances/renewal/c1.md";

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
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

async fn manual_start(p: &EscurelProcess, token: &str, manual: Value, title: &str) -> String {
    let r = call(
        p,
        token,
        "capture_event",
        json!({ "source": "workbench", "mime": "text/plain", "label_skill": "renewal",
                "instance_page_id": PAGE, "title": title, "body": "please renew",
                "provenance": { "manual": manual } }),
    )
    .await;
    r["event_id"].as_str().unwrap().to_owned()
}

async fn wait_for_terminal(listen: &str, event_id: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(resp) = reqwest::get(format!(
            "http://{listen}/debug/run?tenant={TENANT}&event_id={event_id}"
        ))
        .await
            && resp.status().is_success()
            && let Ok(run) = resp.json::<Value>().await
            && run["status"] != "pending"
        {
            return run;
        }
        assert!(Instant::now() < deadline, "run never reached a terminal");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The run's event with `title`, waiting briefly: the runner writes its
/// lifecycle events best-effort right after the ledger's terminal, so a
/// read the instant `/debug/run` flips can precede `run-finished`.
async fn run_event(p: &EscurelProcess, token: &str, run_id: &str, title: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let own = call(p, token, "list_events", json!({ "run_id": run_id })).await;
        if let Some(e) = own["events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["title"] == title)
        {
            return e.clone();
        }
        assert!(Instant::now() < deadline, "no {title}: {own}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn body_of(e: &Value) -> Value {
    serde_json::from_str(e["body"].as_str().unwrap()).unwrap()
}

#[tokio::test]
async fn a_plan_mode_run_reports_its_plan_lands_nothing_and_an_approval_runs_it() {
    let gw = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("renewal", SKILL_BODY)
                .instance("renewal", "c1", INSTANCE_BODY)
                .done(),
        ),
        ..Default::default()
    })
    .await;
    let admin = gw.mint_token(TENANT, Role::Admin);
    let alice = gw.mint_token_with_sub(TENANT, Role::Agent, "alice");

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
            ledger_dir.keep().join("ledger.sqlite"),
        )
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms");
    let _runner = ChildGuard(cmd.spawn().expect("spawn runner"));

    // Plan: the harness reports a plan and stops; nothing lands.
    let e1 = manual_start(&gw, &alice, json!({ "mode": "plan" }), "renew (plan first)").await;
    let run = wait_for_terminal(&listen, &e1).await;
    assert_eq!(run["status"], "planned", "{run}");
    let run1 = run["run_id"].as_str().unwrap().to_owned();
    let started = run_event(&gw, &admin, &run1, "run-started").await;
    assert_eq!(
        started["provenance"]["runner"]["manual"]["mode"], "plan",
        "{started}"
    );
    let finished = body_of(&run_event(&gw, &admin, &run1, "run-finished").await);
    assert_eq!(finished["status"], "planned", "{finished}");
    let plan = finished["plan"]
        .as_array()
        .unwrap_or_else(|| panic!("no plan: {finished}"));
    assert!(!plan.is_empty(), "{finished}");
    assert!(
        plan.iter().all(|s| s["status"] == "pending"),
        "a plan, not progress: {finished}"
    );
    assert!(finished["produced_instance"].is_null(), "{finished}");
    let page = call(&gw, &admin, "expand", json!({ "page_id": PAGE })).await;
    assert!(
        !page["body"].as_str().unwrap().contains("folded"),
        "nothing landed: {page}"
    );
    let inbox = call(&gw, &admin, "list_inbox", json!({})).await;
    assert!(
        inbox["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["event_id"] == e1),
        "still inbox: {inbox}"
    );

    // Planned is terminal: a few more polls do not re-run it.
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let ledger: Value = reqwest::get(format!("http://{listen}/debug/ledger"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(ledger["total"], 1, "{ledger}");
    assert_eq!(ledger["planned"], 1, "{ledger}");

    // Approve: a new start naming the plan run gets the plan in its input
    // and runs for real.
    let e2 = manual_start(
        &gw,
        &alice,
        json!({ "approved_plan_run_id": run1 }),
        "renew (approved)",
    )
    .await;
    let run = wait_for_terminal(&listen, &e2).await;
    assert_eq!(run["status"], "processed", "{run}");
    let run2 = run["run_id"].as_str().unwrap().to_owned();
    let started = run_event(&gw, &admin, &run2, "run-started").await;
    assert_eq!(
        started["provenance"]["runner"]["manual"]["approved_plan_run_id"], run1,
        "{started}"
    );
    let page = call(&gw, &admin, "expand", json!({ "page_id": PAGE })).await;
    let body = page["body"].as_str().unwrap();
    assert!(body.contains("folded event"), "landed: {page}");
    let first_step = plan[0]["step"].as_str().unwrap();
    assert!(
        body.contains(first_step),
        "the approved plan reached the harness: {page}"
    );
}

#[tokio::test]
async fn a_failing_plan_run_is_never_recorded_as_planned() {
    let gw = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("renewal", SKILL_BODY)
                .instance("renewal", "c1", INSTANCE_BODY)
                .done(),
        ),
        ..Default::default()
    })
    .await;
    let alice = gw.mint_token_with_sub(TENANT, Role::Agent, "alice");
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
        .env("ESCUREL_ECHO_FAIL_SKILL", "renewal")
        .env(
            "ESCUREL_RUNNER_LEDGER_PATH",
            ledger_dir.keep().join("ledger.sqlite"),
        )
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms");
    let _runner = ChildGuard(cmd.spawn().expect("spawn runner"));
    let event = manual_start(&gw, &alice, json!({ "mode": "plan" }), "failing plan").await;
    let run = wait_for_terminal(&listen, &event).await;
    assert_eq!(run["status"], "dead_letter", "{run}");
    let page = call(&gw, &alice, "expand", json!({ "page_id": PAGE })).await;
    assert!(
        page["body"].as_str().unwrap().contains("BASELINE"),
        "{page}"
    );
}

#[tokio::test]
async fn an_over_budget_gemini_plan_is_dead_lettered_not_planned() {
    use axum::{Router, extract::State};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    async fn answer(State(calls): State<Arc<AtomicUsize>>) -> axum::Json<Value> {
        calls.fetch_add(1, Ordering::SeqCst);
        axum::Json(json!({ "candidates": [{ "content": { "parts": [
            { "text": "Proposed plan" }
        ] } }], "usageMetadata": { "promptTokenCount": 120,
            "candidatesTokenCount": 40, "totalTokenCount": 160 } }))
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let model_base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn({
        let calls = Arc::clone(&calls);
        async move {
            let _ = axum::serve(listener, Router::new().fallback(answer).with_state(calls)).await;
        }
    });

    let gw = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("renewal", SKILL_BODY)
                .instance("renewal", "c1", INSTANCE_BODY)
                .done(),
        ),
        ..Default::default()
    })
    .await;
    let alice = gw.mint_token_with_sub(TENANT, Role::Agent, "alice");
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
        .env("ESCUREL_RUNNER_HARNESS", "gemini")
        .env("ESCUREL_GEMINI_API_KEY", "test-key")
        .env("ESCUREL_RUNNER_GEMINI_BASE_URL", model_base)
        .env("ESCUREL_RUNNER_GEMINI_MAX_TOTAL_TOKENS", "100")
        .env(
            "ESCUREL_RUNNER_LEDGER_PATH",
            ledger_dir.keep().join("ledger.sqlite"),
        )
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms");
    let _runner = ChildGuard(cmd.spawn().expect("spawn runner"));
    let event = manual_start(&gw, &alice, json!({ "mode": "plan" }), "budget plan").await;
    let run = wait_for_terminal(&listen, &event).await;
    assert_eq!(run["status"], "dead_letter", "{run}");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "no extra model turn");
    let page = call(&gw, &alice, "expand", json!({ "page_id": PAGE })).await;
    assert!(
        page["body"].as_str().unwrap().contains("BASELINE"),
        "{page}"
    );
}

#[tokio::test]
async fn a_timed_out_gemini_call_with_unknown_usage_is_not_retried() {
    use axum::{Router, extract::State};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    async fn stalled(State(calls): State<Arc<AtomicUsize>>) -> axum::Json<Value> {
        calls.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_secs(20)).await;
        axum::Json(json!({ "candidates": [{ "content": { "parts": [
            { "text": "Too late" }
        ] } }], "usageMetadata": { "promptTokenCount": 120,
            "candidatesTokenCount": 4, "totalTokenCount": 124 } }))
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let model_base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn({
        let calls = Arc::clone(&calls);
        async move {
            let _ = axum::serve(listener, Router::new().fallback(stalled).with_state(calls)).await;
        }
    });

    let gw = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("renewal", SKILL_BODY)
                .instance("renewal", "c1", INSTANCE_BODY)
                .done(),
        ),
        ..Default::default()
    })
    .await;
    let alice = gw.mint_token_with_sub(TENANT, Role::Agent, "alice");
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
        .env("ESCUREL_RUNNER_HARNESS", "gemini")
        .env("ESCUREL_GEMINI_API_KEY", "test-key")
        .env("ESCUREL_RUNNER_GEMINI_BASE_URL", model_base)
        // Allow gateway packaging to finish under a busy suite; the model
        // itself stalls well past this bound.
        .env("ESCUREL_RUNNER_RUN_TIMEOUT", "5s")
        .env("ESCUREL_RUNNER_MAX_ATTEMPTS", "2")
        .env("ESCUREL_RUNNER_RETRY_BACKOFF", "100ms")
        .env(
            "ESCUREL_RUNNER_LEDGER_PATH",
            ledger_dir.keep().join("ledger.sqlite"),
        )
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms");
    let _runner = ChildGuard(cmd.spawn().expect("spawn runner"));
    let event = manual_start(&gw, &alice, json!({ "mode": "plan" }), "stalled plan").await;
    let run = wait_for_terminal(&listen, &event).await;
    assert_eq!(run["status"], "dead_letter", "{run}");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "must not retry unknown usage"
    );
}

#[tokio::test]
async fn gemini_retry_carries_reported_usage_before_a_second_tool_call() {
    use axum::{Router, extract::State};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    async fn answer(State(calls): State<Arc<AtomicUsize>>) -> axum::Json<Value> {
        let turn = calls.fetch_add(1, Ordering::SeqCst);
        if turn == 0 {
            // A clean answer with no gateway effect makes the reconciler
            // retry. Those 80 tokens still belong to the same run.
            return axum::Json(json!({ "candidates": [{ "content": { "parts": [
                { "text": "No change made" }
            ] } }], "usageMetadata": { "promptTokenCount": 70,
                "candidatesTokenCount": 10, "totalTokenCount": 80 } }));
        }
        axum::Json(json!({ "candidates": [{ "content": { "parts": [{
            "functionCall": { "name": "update_page", "args": {
                "page_id": PAGE,
                "content": "---\nkind: instance\nid: c1\nskill: renewal\n---\nSHOULD_NOT_LAND\n"
            } }
        }] } }], "usageMetadata": { "promptTokenCount": 25,
            "candidatesTokenCount": 5, "totalTokenCount": 30 } }))
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let model_base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn({
        let calls = Arc::clone(&calls);
        async move {
            let _ = axum::serve(listener, Router::new().fallback(answer).with_state(calls)).await;
        }
    });

    let gw = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("renewal", SKILL_BODY)
                .instance("renewal", "c1", INSTANCE_BODY)
                .done(),
        ),
        ..Default::default()
    })
    .await;
    let alice = gw.mint_token_with_sub(TENANT, Role::Agent, "alice");
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
        .env("ESCUREL_RUNNER_HARNESS", "gemini")
        .env("ESCUREL_GEMINI_API_KEY", "test-key")
        .env("ESCUREL_RUNNER_GEMINI_BASE_URL", model_base)
        .env("ESCUREL_RUNNER_GEMINI_MAX_TOTAL_TOKENS", "100")
        .env("ESCUREL_RUNNER_MAX_ATTEMPTS", "2")
        .env("ESCUREL_RUNNER_RETRY_BACKOFF", "100ms")
        .env(
            "ESCUREL_RUNNER_LEDGER_PATH",
            ledger_dir.keep().join("ledger.sqlite"),
        )
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms");
    let _runner = ChildGuard(cmd.spawn().expect("spawn runner"));
    let event = manual_start(&gw, &alice, json!({ "mode": "run" }), "budget retry").await;
    let run = wait_for_terminal(&listen, &event).await;
    assert_eq!(run["status"], "dead_letter", "{run}");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "exactly one model call per attempt"
    );
    let finished = body_of(
        &run_event(
            &gw,
            &gw.mint_token(TENANT, Role::Admin),
            run["run_id"].as_str().unwrap(),
            "run-finished",
        )
        .await,
    );
    assert_eq!(finished["attempts"], 2, "{finished}");
    assert_eq!(finished["usage"]["billed_tokens"], 110, "{finished}");
    let page = call(&gw, &alice, "expand", json!({ "page_id": PAGE })).await;
    assert!(
        page["body"].as_str().unwrap().contains("BASELINE"),
        "{page}"
    );
    assert!(
        !page["body"].as_str().unwrap().contains("SHOULD_NOT_LAND"),
        "{page}"
    );
}
