//! A skill page's `model:` picks the model for runs of that skill — within
//! `ESCUREL_RUNNER_MODEL_ALLOW`, and FAILING CLOSED outside it (the same
//! posture a skill `harness:` outside `ESCUREL_RUNNER_HARNESS_ALLOW` takes).
//!
//! Real gateway, real runner binary, real `/mcp` calls; the model is a real
//! local HTTP server speaking Gemini's `generateContent`, so the URL the
//! adapter calls — which names the model — is observable.

use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role, free_port};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const SKILL: &str = "customer";
const SKILL_MODEL: &str = "gemini-3.5-flash-lite";
const RUNNER_MODEL: &str = "gemini-3.8-flash";
const SKILL_BODY: &str = "---\nkind: skill\nid: customer\nautonomy: auto\n\
model: gemini-3.5-flash-lite\n---\n# customer\n\nFold the event into the instance.\n";
const INSTANCE_PAGE: &str = "markdown/instances/customer/globex.md";
const INSTANCE_BODY: &str =
    "---\nkind: instance\nid: globex\nskill: customer\n---\n# Globex\n\nBASELINE.\n";

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn call(p: &EscurelProcess, name: &str, args: Value) -> Value {
    let token = p.mint_token(TENANT, Role::Admin);
    let body: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": args },
        }))
        .send()
        .await
        .expect("post /mcp")
        .json()
        .await
        .expect("json");
    assert!(body.get("error").is_none(), "tool {name} error: {body}");
    body["result"]
        .get("structuredContent")
        .cloned()
        .unwrap_or_else(|| body["result"].clone())
}

/// A stub `generateContent`: records each request PATH, folds the event on
/// the first turn (update_page + assign_event), answers text on the second.
async fn spawn_stub_model(paths: Arc<Mutex<Vec<String>>>) -> String {
    use axum::{Router, extract::State, http::Uri};

    async fn generate(
        State(paths): State<Arc<Mutex<Vec<String>>>>,
        uri: Uri,
        body: String,
    ) -> axum::Json<Value> {
        paths.lock().expect("lock").push(uri.path().to_owned());
        let req: Value = serde_json::from_str(&body).expect("adapter sent JSON");
        let parts = if req["contents"].as_array().map_or(0, Vec::len) == 1 {
            let input = req["contents"][0]["parts"][0]["text"]
                .as_str()
                .unwrap_or_default();
            let event_id = input
                .split_whitespace()
                .find(|w| w.starts_with("01") && w.len() == 26)
                .unwrap_or_default();
            json!([
                { "functionCall": { "name": "update_page", "args": {
                    "page_id": INSTANCE_PAGE,
                    "content": format!("{INSTANCE_BODY}\nFOLDED {event_id}\n"),
                } } },
                { "functionCall": { "name": "assign_event", "args": {
                    "event_id": event_id, "instance_page_id": INSTANCE_PAGE,
                } } },
            ])
        } else {
            json!([{ "text": "Folded." }])
        };
        axum::Json(json!({ "candidates": [{ "content": { "parts": parts } }],
            "usageMetadata": { "promptTokenCount": 10, "candidatesTokenCount": 5,
                "totalTokenCount": 15 } }))
    }

    let app = Router::new().fallback(generate).with_state(paths);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stub model");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

/// Run one event of the `model:`-declaring skill through a gemini runner
/// whose own model is [`RUNNER_MODEL`]. Returns the terminal ledger row,
/// the `run-finished` / `run-started` events and the paths the model saw.
async fn run_skill(model_allow: Option<&str>) -> (Value, Value, Value, Vec<String>) {
    let gateway = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill(SKILL, SKILL_BODY)
                .instance(SKILL, "globex", INSTANCE_BODY)
                .done(),
        ),
        ..Default::default()
    })
    .await;
    let captured = call(
        &gateway,
        "capture_event",
        json!({ "source": "manual", "mime": "text/plain", "label_skill": SKILL,
                "instance_page_id": INSTANCE_PAGE, "title": "renewal", "body": "renew" }),
    )
    .await;
    let event_id = captured["event_id"].as_str().expect("event_id").to_owned();

    let paths = Arc::new(Mutex::new(Vec::new()));
    let model_base = spawn_stub_model(Arc::clone(&paths)).await;
    let listen = format!("127.0.0.1:{}", free_port());
    let ledger_dir = tempfile::tempdir().expect("tempdir for ledger");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gateway.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env(
            "ESCUREL_RUNNER_TOKEN",
            gateway.mint_token(TENANT, Role::Admin),
        )
        .env("ESCUREL_RUNNER_HARNESS", "gemini")
        .env("ESCUREL_RUNNER_GEMINI_MODEL", RUNNER_MODEL)
        .env("ESCUREL_RUNNER_GEMINI_BASE_URL", &model_base)
        .env("ESCUREL_GEMINI_API_KEY", "test-key-not-a-real-credential")
        .env(
            "ESCUREL_RUNNER_LEDGER_PATH",
            ledger_dir.path().join("ledger.sqlite"),
        )
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms")
        .env_remove("ESCUREL_RUNNER_MODEL_ALLOW");
    if let Some(allow) = model_allow {
        cmd.env("ESCUREL_RUNNER_MODEL_ALLOW", allow);
    }
    let _runner = ChildGuard(cmd.spawn().expect("spawn escurel-runner"));

    let deadline = Instant::now() + Duration::from_secs(60);
    let run = loop {
        if let Ok(resp) = reqwest::get(format!(
            "http://{listen}/debug/run?tenant={TENANT}&event_id={event_id}"
        ))
        .await
            && resp.status().is_success()
            && let Ok(run) = resp.json::<Value>().await
            && run["status"] != "pending"
        {
            break run;
        }
        assert!(Instant::now() < deadline, "run never reached a terminal");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let run_id = run["run_id"].as_str().expect("run_id").to_owned();
    let mut found = (None, None);
    let deadline = Instant::now() + Duration::from_secs(10);
    while found.0.is_none() || found.1.is_none() {
        let own = call(&gateway, "list_events", json!({ "run_id": run_id })).await;
        for e in own["events"].as_array().expect("events") {
            match e["title"].as_str() {
                Some("run-finished") => found.0 = Some(e.clone()),
                Some("run-started") => found.1 = Some(e.clone()),
                _ => {}
            }
        }
        assert!(Instant::now() < deadline, "no run events: {own}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let seen = paths.lock().expect("lock").clone();
    (run, found.0.unwrap(), found.1.unwrap(), seen)
}

#[tokio::test]
async fn a_skill_model_within_the_allow_list_is_the_model_the_run_calls() {
    let (run, _finished, started, paths) = run_skill(Some(SKILL_MODEL)).await;
    assert_eq!(run["status"], "processed", "{run}");
    let expected = format!("/models/{SKILL_MODEL}:generateContent");
    assert!(
        !paths.is_empty() && paths.iter().all(|p| *p == expected),
        "every turn must call the skill's model, not {RUNNER_MODEL}: {paths:?}"
    );
    assert_eq!(
        started["provenance"]["runner"]["model"], SKILL_MODEL,
        "the run's events record the effective model: {started}"
    );
}

#[tokio::test]
async fn a_skill_model_outside_the_allow_list_fails_the_run_closed() {
    let (run, finished, _started, paths) = run_skill(None).await;
    assert_eq!(run["status"], "dead_letter", "{run}");
    let body: Value = serde_json::from_str(finished["body"].as_str().unwrap()).unwrap();
    let error = body["error"].as_str().unwrap_or("");
    assert!(
        error.contains(SKILL_MODEL) && error.contains("ESCUREL_RUNNER_MODEL_ALLOW"),
        "the refusal names the model and the allow-list: {body}"
    );
    assert!(paths.is_empty(), "no model may be called: {paths:?}");
}
