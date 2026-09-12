//! A review run that has nothing to do CONVERGES — it is not retried until
//! the agent invents something.
//!
//! Measured on lab, 2026-09-12. A run under `autonomy: review` reached an
//! agent that read the target page and answered "Event already covered by
//! existing note; no page modification needed", naming the note it had read.
//! The reconciler asked the gateway for a draft, was told there is none,
//! classified that as a TRANSIENT failure, and retried. On the second attempt
//! the agent did the only thing that would satisfy the read-back: it created
//! a draft of the page it had just said needed no change. A human found that
//! card in their review queue, indistinguishable from real work until they
//! opened it and found nothing in it.
//!
//! `main.rs` already had the conversion for this — `ReconcileError::Converged`
//! exists precisely for a clean no-op — but its guard required
//! `harness_produced.is_none()`, and the agent HAD named a page. Under
//! `review` that name is not evidence of an effect: the only effect that
//! counts is a draft, and naming the page it examined is how an agent says
//! what the event was about.
//!
//! **The load-bearing assertion is the call COUNT.** "No draft appeared" is
//! satisfied by a run that never happened, which is the shape of a test that
//! proves nothing; "the model was asked exactly once" is what says the run
//! ran and was not pressed a second time. Asking twice and taking the second
//! answer is not a retry — it is pressure, and an agent holding a tool will
//! use it.
//!
//! Real gateway, real runner binary, real `/mcp`. The model is a local HTTP
//! server speaking Gemini's wire shape that calls NO tools and reports the
//! page it read, which is exactly what the lab agent did.

use std::net::TcpListener;
use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const INSTANCE_ID: &str = "globex";

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind")
        .local_addr()
        .expect("addr")
        .port()
}

async fn call_mcp(p: &EscurelProcess, name: &str, args: Value) -> Value {
    let token = p.mint_token(TENANT, Role::Agent);
    let body = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": name, "arguments": args },
    });
    let res = reqwest::Client::new()
        .post(p.mcp_url())
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .expect("mcp call");
    let out: Value = res.json().await.expect("mcp json");
    serde_json::from_str(
        out["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or("null"),
    )
    .unwrap_or(Value::Null)
}

/// A model that does its job and finds nothing to do.
///
/// It calls no tools at all and answers in words, naming the page it read —
/// the shape that made the runner retry. Every request is counted, because
/// the count is the assertion.
async fn spawn_idle_model(page_id: String, calls: Arc<Mutex<usize>>) -> String {
    use axum::{Router, extract::State};

    #[derive(Clone)]
    struct St {
        page_id: String,
        calls: Arc<Mutex<usize>>,
    }

    async fn generate(State(st): State<St>, _body: String) -> axum::Json<Value> {
        *st.calls.lock().expect("lock") += 1;
        axum::Json(json!({ "candidates": [{ "content": { "parts": [
            { "text": format!(
                "Event already covered by existing note; no page modification \
                 needed. {}", st.page_id) },
        ] } }] }))
    }

    let app = Router::new()
        .fallback(generate)
        .with_state(St { page_id, calls });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind idle model");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn a_review_run_with_nothing_to_do_is_not_retried_into_drafting() {
    let skill = "customer_review";
    let skill_body = format!(
        "---\ntype: skill\nid: {skill}\nautonomy: review\n---\n# {skill}\n\n\
         Fold the event in.\n"
    );
    let instance_body = format!(
        "---\ntype: instance\nid: {INSTANCE_ID}\nskill: {skill}\n---\n\
         # Globex\n\nBASELINE.\n"
    );
    let gateway = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill(skill, skill_body.as_str())
                .instance(skill, INSTANCE_ID, instance_body.as_str())
                .done(),
        ),
        ..Default::default()
    })
    .await;
    let page_id = format!("markdown/instances/{skill}/{INSTANCE_ID}.md");

    let captured = call_mcp(
        &gateway,
        "capture_event",
        json!({
            "source": "manual",
            "mime": "text/plain",
            "label_skill": skill,
            "instance_page_id": page_id,
            "title": "renewal",
            "body": "they want to renew",
        }),
    )
    .await;
    let event_id = captured["event_id"].as_str().expect("event_id").to_owned();

    let calls = Arc::new(Mutex::new(0usize));
    let model_base = spawn_idle_model(page_id.clone(), Arc::clone(&calls)).await;

    let token = gateway.mint_token(TENANT, Role::Agent);
    let ledger_dir = tempfile::tempdir().expect("tempdir for ledger");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env(
        "ESCUREL_RUNNER_LISTEN",
        format!("127.0.0.1:{}", free_port()),
    )
    .env("ESCUREL_RUNNER_GATEWAY_URL", gateway.base_url())
    .env("ESCUREL_RUNNER_TENANT", TENANT)
    .env("ESCUREL_RUNNER_TOKEN", &token)
    .env("ESCUREL_RUNNER_HARNESS", "gemini")
    .env(
        "ESCUREL_RUNNER_LEDGER_PATH",
        ledger_dir.path().join("ledger.sqlite").to_str().unwrap(),
    )
    .env("ESCUREL_GEMINI_API_KEY", "test-key-not-a-real-credential")
    .env("ESCUREL_RUNNER_GEMINI_BASE_URL", &model_base)
    .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms");
    let _runner = ChildGuard(cmd.spawn().expect("spawn escurel-runner"));

    // The run must actually HAPPEN. Without this the assertions below are
    // satisfied by a runner that never picked the event up, which is the
    // exact shape of a test that proves nothing.
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        if *calls.lock().expect("lock") > 0 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the runner must dispatch the event to the model within 60s"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    // Now sit through several retry windows. The backoff that produced the
    // second attempt on lab was 500ms; ten seconds is twenty of those.
    tokio::time::sleep(Duration::from_secs(10)).await;

    assert_eq!(
        *calls.lock().expect("lock"),
        1,
        "a review run whose agent found nothing to do must be asked ONCE. A \
         second ask is pressure, not a retry, and the agent answers it by \
         creating a draft of a page it just said needed no change."
    );

    let drafts = call_mcp(&gateway, "list_drafts", json!({})).await;
    let held: Vec<&Value> = drafts["drafts"]
        .as_array()
        .expect("drafts")
        .iter()
        .filter(|d| d["event_id"] == json!(event_id))
        .collect();
    assert!(
        held.is_empty(),
        "nothing to do must leave the review queue empty — a card a human \
         opens to find no change in is worse than no card: {held:?}"
    );

    // …and the page is untouched, so "no draft" is not hiding a write that
    // went round the gate.
    let body = call_mcp(&gateway, "expand", json!({ "page_id": &page_id })).await["body"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(
        body.contains("BASELINE."),
        "the target must be exactly as it was: {body}"
    );
}
