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
use escurel_runner_harness::{GeminiHarness, Harness};
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
            }] }));
        }
        axum::Json(json!({ "candidates": [{ "content": { "parts": [{ "text": "done" }] } }] }))
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
