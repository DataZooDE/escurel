//! The gateway decides whether a run worked — not the harness's account of it.
//!
//! The reconciler's whole rule is "don't trust the harness: read back over
//! `/mcp` to confirm". That rule was applied to a self-reported SUCCESS and
//! skipped for a self-reported FAILURE: `!outcome.ok` returned before the
//! read-back ever happened.
//!
//! Measured in the cluster on 2026-09-06. A Gemini run read a live Gmail
//! thread, called `create_draft` (the gateway answered `status: ok`), kept
//! talking, and hit the model-turn cap. The run was recorded
//! `failed (retriable re-drive)` for work that had landed — and a re-drive
//! would have produced a SECOND draft for a page whose stated rule is one
//! draft per page.
//!
//! Two cases, because the fix must not become "ignore failures":
//!
//!   * a model that DRAFTS and then runs out of turns → the run succeeded;
//!   * a model that drafts NOTHING and runs out of turns → still a failure.
//!
//! The second is the control. Without it, a runner that recorded every run as
//! success would pass the first assertion.

use std::net::TcpListener;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const SKILL: &str = "customer";
const INSTANCE_ID: &str = "globex";

/// `review`, so the run's confirmable effect is a DRAFT.
const SKILL_BODY: &str = "---\ntype: skill\nid: customer\nautonomy: review\n---\n# customer\n\n\
Fold the triggering event into the named customer instance.\n";
const INSTANCE_BODY: &str =
    "---\ntype: instance\nid: globex\nskill: customer\n---\n# Globex\n\nBASELINE.\n";

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

/// A model that never gives a final answer, so the turn cap always fires.
///
/// When `draft` is set it creates one on its first turn and then talks
/// forever; otherwise it only ever searches. Neither ever returns text, which
/// is what "stopped after N model turns without a final answer" means.
async fn spawn_stub_model(page_id: String, draft: bool) -> String {
    use axum::{Router, extract::State};

    async fn generate(
        State((page_id, draft)): State<(String, bool)>,
        body: String,
    ) -> axum::Json<Value> {
        let req: Value = serde_json::from_str(&body).expect("json");
        let turn = req["contents"].as_array().map_or(0, Vec::len).div_ceil(2);
        let input = req["contents"][0]["parts"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let event_id = input
            .split_whitespace()
            .find(|w| w.starts_with("01") && w.len() == 26)
            .unwrap_or_default()
            .to_owned();

        let parts = if turn == 1 && draft {
            let content = format!(
                "---\ntype: instance\nid: {INSTANCE_ID}\nskill: {SKILL}\n---\n\
                 # Globex\n\nBASELINE.\n\nDRAFTED_THEN_RAN_OUT {event_id}\n"
            );
            json!([{ "functionCall": { "name": "create_draft", "args": {
                "target_page_id": page_id,
                "content": content,
                "event_id": event_id,
            } } }])
        } else {
            // Busywork, forever. Never a final answer.
            json!([{ "functionCall": { "name": "search", "args": { "q": "globex" } } }])
        };
        axum::Json(json!({ "candidates": [{ "content": { "parts": parts } }] }))
    }

    let app = Router::new()
        .fallback(generate)
        .with_state((page_id, draft));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stub model");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

/// Run one event to a terminal ledger state and return the runner's counts.
async fn run_until_terminal(draft: bool) -> (Value, EscurelProcess, String) {
    let gateway = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill(SKILL, SKILL_BODY)
                .instance(SKILL, INSTANCE_ID, INSTANCE_BODY)
                .done(),
        ),
        ..Default::default()
    })
    .await;
    let page_id = format!("markdown/instances/{SKILL}/{INSTANCE_ID}.md");

    call_mcp(
        &gateway,
        "capture_event",
        json!({
            "source": "manual",
            "mime": "text/plain",
            "label_skill": SKILL,
            "instance_page_id": page_id,
            "title": "renewal",
            "body": "they want to renew",
        }),
    )
    .await;

    let model_base = spawn_stub_model(page_id.clone(), draft).await;
    let token = gateway.mint_token(TENANT, Role::Agent);
    let port = free_port();
    // A ledger of this test's own. The default path is a file in the working
    // directory that every runner test shares, so global counts there say
    // nothing about this run.
    let ledger = std::env::temp_dir().join(format!(
        "escurel-runner-hfvg-{}-{port}.sqlite",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&ledger);
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", format!("127.0.0.1:{port}"))
        .env("ESCUREL_RUNNER_GATEWAY_URL", gateway.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_LEDGER_PATH", &ledger)
        .env("ESCUREL_RUNNER_TOKEN", &token)
        .env("ESCUREL_RUNNER_HARNESS", "gemini")
        .env("ESCUREL_GEMINI_API_KEY", "test-key-not-a-real-credential")
        .env("ESCUREL_RUNNER_GEMINI_BASE_URL", &model_base)
        // Reach the cap in seconds rather than twelve round trips.
        .env("ESCUREL_RUNNER_MAX_TURNS", "2")
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms");
    let mut runner = ChildGuard(cmd.spawn().expect("spawn escurel-runner"));

    // Wait for the runner to actually be up, and say so loudly if it is not.
    //
    // `free_port` picks a port by binding and closing, so under a parallel
    // suite another process can take it first. That used to surface as this
    // test waiting out its whole deadline on a ledger nobody was writing —
    // a mystery timeout instead of a diagnosis.
    let health = format!("http://127.0.0.1:{port}/healthz");
    let up_by = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(status) = runner.0.try_wait().expect("try_wait") {
            panic!("the runner exited before serving /healthz: {status}");
        }
        if reqwest::get(&health)
            .await
            .is_ok_and(|r| r.status().is_success())
        {
            break;
        }
        assert!(Instant::now() < up_by, "the runner never served /healthz");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let ledger_url = format!("http://127.0.0.1:{port}/debug/ledger");
    // Generous on purpose. This spawns a gateway, a runner and a model, and
    // the workspace suite runs many such tests at once — the contention is a
    // known property of this suite, not of the code under test. Only the
    // PATIENCE is generous: the assertions below are unchanged, so a real
    // regression still fails, it just fails after waiting.
    let deadline = Instant::now() + Duration::from_secs(240);
    loop {
        if let Ok(resp) = reqwest::get(&ledger_url).await
            && let Ok(v) = resp.json::<Value>().await
            && v["terminal"].as_i64().unwrap_or(0) > 0
        {
            return (v, gateway, page_id);
        }
        assert!(
            Instant::now() < deadline,
            "the run never reached a terminal ledger state"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

#[tokio::test]
async fn a_draft_that_landed_outlives_the_harness_saying_it_failed() {
    let (ledger, gateway, page_id) = run_until_terminal(true).await;

    assert_eq!(
        ledger["failed"].as_i64().unwrap_or(-1),
        0,
        "a run whose draft the gateway can confirm must not be recorded failed \
         — a retriable re-drive would draft the same page twice: {ledger}"
    );
    assert!(
        ledger["succeeded"].as_i64().unwrap_or(0) > 0,
        "the confirmed draft is the run's effect: {ledger}"
    );

    // And the draft is really there — so the assertion above is about the
    // verdict, not about a run that quietly did nothing.
    let drafts = call_mcp(&gateway, "list_drafts", json!({})).await;
    let found = drafts["drafts"]
        .as_array()
        .map(|ds| {
            ds.iter().any(|d| {
                d["target_page_id"].as_str() == Some(page_id.as_str())
                    && d["content"]
                        .as_str()
                        .is_some_and(|c| c.contains("DRAFTED_THEN_RAN_OUT"))
            })
        })
        .unwrap_or(false);
    assert!(
        found,
        "the draft the run is credited with must exist: {drafts}"
    );
}

#[tokio::test]
async fn running_out_of_turns_with_nothing_drafted_is_still_a_failure() {
    // The control. Same model, same cap, same everything — it just never
    // drafts. If this passed as success too, the fix would be "believe
    // nothing" rather than "ask the gateway".
    let (ledger, gateway, _page_id) = run_until_terminal(false).await;

    assert_eq!(
        ledger["succeeded"].as_i64().unwrap_or(-1),
        0,
        "a run that produced no confirmable effect must not be credited: {ledger}"
    );

    let drafts = call_mcp(&gateway, "list_drafts", json!({})).await;
    assert_eq!(
        drafts["drafts"].as_array().map_or(0, Vec::len),
        0,
        "control: this model drafts nothing, so there is nothing to confirm: {drafts}"
    );
}
