//! The Gemini adapter survives a MALFORMED function call (knowledge-workbench
//! backend P1, found by PR6b's live barrier run).
//!
//! `gemini-2.5-flash` answered the new "report your plan" paragraph by
//! emitting Python-style pseudo-code for the nested `plan` array instead of
//! a function call; the API returned `finishReason: MALFORMED_FUNCTION_CALL`
//! with no parts, and the adapter reported an upstream error — a whole
//! attempt lost to one bad turn. A malformed call is the model's mistake to
//! correct, not the run's to fail: the adapter now tells the model what went
//! wrong and lets it try again, within the same turn budget.
//!
//! Real gateway (the adapter reads `tools/list` from it), real adapter, a
//! stub model that scripts the two answers.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use escurel_runner_core::{SecretString, TaskContext};
use escurel_runner_harness::{GeminiHarness, Harness, HarnessError};
use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "acme";

/// Every request body the stub model received.
type Seen = Arc<Mutex<Vec<Value>>>;

async fn spawn_stub_model(seen: Seen, always_malformed: bool) -> String {
    use axum::Router;
    use axum::extract::State;

    #[derive(Clone)]
    struct St {
        seen: Seen,
        always_malformed: bool,
    }

    async fn generate(State(st): State<St>, body: String) -> axum::Json<Value> {
        let req: Value = serde_json::from_str(&body).expect("json");
        let n = {
            let mut s = st.seen.lock().unwrap();
            s.push(req);
            s.len()
        };
        if n == 1 || st.always_malformed {
            // Verbatim shape of the live failure: no `content`, a finish reason.
            return axum::Json(json!({ "candidates": [{
                "finishReason": "MALFORMED_FUNCTION_CALL", "index": 0,
                "finishMessage": "Malformed function call: print(default_api.report_progress(plan=[...]))"
            }], "usageMetadata": { "promptTokenCount": 20, "candidatesTokenCount": 2,
                "totalTokenCount": 22 } }));
        }
        axum::Json(
            json!({ "candidates": [{ "content": { "parts": [{ "text": "done" }] } }],
            "usageMetadata": { "promptTokenCount": 25, "candidatesTokenCount": 1,
                "totalTokenCount": 26 } }),
        )
    }

    let app = Router::new().fallback(generate).with_state(St {
        seen,
        always_malformed,
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

async fn spawn_budget_model(seen: Seen, page_id: String) -> String {
    use axum::Router;
    use axum::extract::State;

    #[derive(Clone)]
    struct St {
        seen: Seen,
        page_id: String,
    }
    async fn generate(State(st): State<St>, body: String) -> axum::Json<Value> {
        let request: Value = serde_json::from_str(&body).expect("Gemini request JSON");
        let turn = {
            let mut seen = st.seen.lock().unwrap();
            seen.push(request);
            seen.len()
        };
        let parts = if turn == 1 {
            json!([{ "functionCall": { "name": "update_page", "args": {
                "page_id": st.page_id,
                "content": "---\nkind: instance\nid: item\nskill: customer\n---\nOVER_BUDGET_WRITE\n"
            } } }])
        } else {
            json!([{ "text": "done" }])
        };
        axum::Json(json!({ "candidates": [{ "content": { "parts": parts } }],
            "usageMetadata": { "promptTokenCount": 120, "candidatesTokenCount": 40,
                "totalTokenCount": 160 } }))
    }

    let app = Router::new()
        .fallback(generate)
        .with_state(St { seen, page_id });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

async fn spawn_missing_usage_model(seen: Seen) -> String {
    use axum::Router;
    use axum::extract::State;
    async fn generate(State(seen): State<Seen>, body: String) -> axum::Json<Value> {
        seen.lock()
            .unwrap()
            .push(serde_json::from_str(&body).unwrap());
        axum::Json(json!({ "candidates": [{
            "finishReason": "MALFORMED_FUNCTION_CALL", "index": 0
        }] }))
    }
    let app = Router::new().fallback(generate).with_state(seen);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

fn task(gateway: &EscurelProcess) -> TaskContext {
    TaskContext::for_test(
        "fold the event".to_owned(),
        "## Triggering event\n\nx".to_owned(),
        gateway.mcp_url().to_owned(),
        vec!["expand".to_owned(), "report_progress".to_owned()],
        SecretString::from(gateway.mint_token(TENANT, Role::Agent)),
    )
}

#[tokio::test]
async fn a_malformed_function_call_is_corrected_not_fatal() {
    let gateway = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(FixtureBuilder::new().tenant(TENANT).done()),
        ..Default::default()
    })
    .await;
    let seen: Seen = Arc::default();
    let base = spawn_stub_model(Arc::clone(&seen), false).await;
    let harness = GeminiHarness::new("test-key")
        .with_base_url(Some(base))
        .with_max_turns(4)
        .with_timeout(Duration::from_secs(20));

    let outcome = harness
        .run(&task(&gateway))
        .await
        .expect("a malformed call is not an upstream error");
    assert!(outcome.ok, "{outcome:?}");
    assert_eq!(outcome.summary, "done");

    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2, "one correction turn, then the answer");
    // The correction reaches the model as the LAST user text, naming what
    // went wrong and asking for plain JSON arguments.
    let last_user = seen[1]["contents"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["role"] == "user")
        .next_back()
        .cloned()
        .unwrap_or_default();
    let text: String = last_user["parts"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|p| p["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("malformed"), "{last_user}");
    assert!(
        text.contains("report_progress(plan"),
        "names the bad call: {last_user}"
    );
    assert!(text.contains("JSON"), "{last_user}");
}

#[tokio::test]
async fn a_model_that_stays_malformed_runs_out_of_turns_cleanly() {
    let gateway = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(FixtureBuilder::new().tenant(TENANT).done()),
        ..Default::default()
    })
    .await;
    let seen: Seen = Arc::default();
    let base = spawn_stub_model(Arc::clone(&seen), true).await;
    let harness = GeminiHarness::new("test-key")
        .with_base_url(Some(base))
        .with_max_turns(3)
        .with_timeout(Duration::from_secs(20));
    // Bounded: a FAILED outcome the reconciler can retry, never a hang and
    // never an adapter error.
    let outcome = harness.run(&task(&gateway)).await.expect("bounded");
    assert!(!outcome.ok, "{outcome:?}");
    assert_eq!(seen.lock().unwrap().len(), 3, "exactly max_turns requests");
}

#[tokio::test]
async fn a_model_turn_over_the_token_budget_cannot_write_through_real_mcp() {
    token_budget_case(Some(100)).await;
}

#[tokio::test]
async fn a_model_turn_within_the_token_budget_writes_through_real_mcp() {
    token_budget_case(None).await;
}

#[tokio::test]
async fn a_retry_cannot_restart_an_exhausted_run_budget() {
    let gateway = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(FixtureBuilder::new().tenant(TENANT).done()),
        ..Default::default()
    })
    .await;
    let seen: Seen = Arc::default();
    let base = spawn_stub_model(Arc::clone(&seen), false).await;
    let harness = GeminiHarness::new("test-key")
        .with_base_url(Some(base))
        .with_max_total_tokens(100);
    let mut work = task(&gateway);
    work.prior_model_tokens = 100;
    let outcome = harness.run(&work).await.expect("budget outcome");
    assert!(!outcome.ok, "{outcome:?}");
    assert!(outcome.summary.contains("token budget"), "{outcome:?}");
    assert!(
        seen.lock().unwrap().is_empty(),
        "no paid model call remains"
    );
}

#[tokio::test]
async fn a_retry_limits_requested_output_to_its_remaining_token_allowance() {
    let gateway = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(FixtureBuilder::new().tenant(TENANT).done()),
        ..Default::default()
    })
    .await;
    let seen: Seen = Arc::default();
    let base = spawn_stub_model(Arc::clone(&seen), false).await;
    let harness = GeminiHarness::new("test-key")
        .with_base_url(Some(base))
        .with_max_total_tokens(100);
    let mut work = task(&gateway);
    work.prior_model_tokens = 80;
    let _ = harness.run(&work).await.expect("budget outcome");
    let requests = seen.lock().unwrap();
    assert_eq!(
        requests.len(),
        1,
        "the first response consumes the allowance"
    );
    assert_eq!(requests[0]["generationConfig"]["maxOutputTokens"], 20);
}

#[tokio::test]
async fn malformed_response_without_usage_does_not_trigger_unaccounted_retry() {
    let gateway = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(FixtureBuilder::new().tenant(TENANT).done()),
        ..Default::default()
    })
    .await;
    let seen: Seen = Arc::default();
    let base = spawn_missing_usage_model(Arc::clone(&seen)).await;
    let harness = GeminiHarness::new("test-key")
        .with_base_url(Some(base))
        .with_max_turns(4);
    let outcome = harness
        .run(&task(&gateway))
        .await
        .expect("accounting failure");
    assert!(!outcome.ok, "{outcome:?}");
    assert!(outcome.summary.contains("usage metadata"), "{outcome:?}");
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_gateway_preflight_error_has_no_model_usage() {
    let seen: Seen = Arc::default();
    let base = spawn_stub_model(Arc::clone(&seen), false).await;
    let harness = GeminiHarness::new("test-key").with_base_url(Some(base));
    let work = TaskContext::for_test(
        "plan".to_owned(),
        "request".to_owned(),
        "http://127.0.0.1:1/mcp".to_owned(),
        vec!["expand".to_owned()],
        SecretString::from("test-token".to_owned()),
    );
    let error = harness
        .run(&work)
        .await
        .expect_err("gateway is unavailable");
    assert!(matches!(error, HarnessError::Preflight { .. }), "{error:?}");
    assert!(seen.lock().unwrap().is_empty(), "model was not called");
}

async fn token_budget_case(limit: Option<u64>) {
    let page_id = "markdown/instances/customer/item.md";
    let gateway = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill(
                    "customer",
                    "---\nkind: skill\nid: customer\nautonomy: auto\n---\n",
                )
                .instance(
                    "customer",
                    "item",
                    "---\nkind: instance\nid: item\nskill: customer\n---\nBASELINE\n",
                )
                .done(),
        ),
        ..Default::default()
    })
    .await;
    let seen: Seen = Arc::default();
    let base = spawn_budget_model(Arc::clone(&seen), page_id.to_owned()).await;
    let mut harness = GeminiHarness::new("test-key").with_base_url(Some(base));
    if let Some(tokens) = limit {
        harness = harness.with_max_total_tokens(tokens);
    }
    let work = TaskContext::for_test(
        "update the record".to_owned(),
        "use the declared tool".to_owned(),
        gateway.mcp_url().to_owned(),
        vec!["update_page".to_owned()],
        SecretString::from(gateway.mint_token(TENANT, Role::Agent)),
    );
    let outcome = harness.run(&work).await.expect("budget outcome");
    if limit.is_some() {
        assert!(!outcome.ok, "over-budget turn must fail: {outcome:?}");
        assert!(outcome.summary.contains("token budget"), "{outcome:?}");
        assert_eq!(outcome.usage.unwrap().input_tokens, 120);
        assert_eq!(
            seen.lock().unwrap().len(),
            1,
            "must not request another model turn"
        );
    } else {
        assert!(outcome.ok, "within-budget turn should finish: {outcome:?}");
        assert_eq!(
            seen.lock().unwrap().len(),
            2,
            "tool result should reach the model"
        );
    }
    let token = gateway.mint_token(TENANT, Role::Agent);
    let response: Value = reqwest::Client::new()
        .post(gateway.mcp_url())
        .bearer_auth(token)
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "expand", "arguments": { "page_id": page_id } } }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let content = response["result"]["structuredContent"]["body"]
        .as_str()
        .unwrap_or_default();
    if limit.is_some() {
        assert!(
            content.contains("BASELINE") && !content.contains("OVER_BUDGET_WRITE"),
            "{response}"
        );
    } else {
        assert!(content.contains("OVER_BUDGET_WRITE"), "{response}");
    }
}
