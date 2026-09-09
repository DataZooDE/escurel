//! Live DoD for the Gemini adapter — **no stand-in at all**, and therefore
//! `#[ignore]`: it needs a real API key and burns real quota, and a real
//! model is non-deterministic, so it must not gate every commit.
//!
//! ```text
//! ESCUREL_GEMINI_API_KEY=… cargo test -p escurel-runner --test suite gemini_live:: -- --ignored --nocapture
//! ```
//!
//! Everything is real: the gateway, the event, the runner binary, Gemini
//! itself, and the `/mcp` writes the model chooses to make under the scoped
//! token. The assertion is deliberately about the EFFECT rather than the
//! wording — the event becomes `processed` and the instance keeps its
//! baseline while gaining content — because asserting on a real model's prose
//! is how a live test becomes a flake.

use std::net::TcpListener;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const SKILL: &str = "customer";
const SKILL_BODY: &str = "---\ntype: skill\nid: customer\n---\n# customer\n\n\
Fold the triggering event into the named customer instance. Read the instance \
with `expand`, append a short dated note about the event to its body (do NOT \
delete existing content), write it back with `update_page`, then call \
`assign_event` to mark the event processed and bound to the instance.\n";
const INSTANCE_ID: &str = "globex";
const INSTANCE_BODY: &str =
    "---\ntype: instance\nid: globex\nskill: customer\n---\n# Globex\n\nBASELINE account state.\n";

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

#[tokio::test]
#[ignore = "live: needs ESCUREL_GEMINI_API_KEY and real model quota"]
async fn real_gemini_folds_a_real_event_over_real_mcp() {
    let api_key = std::env::var("ESCUREL_GEMINI_API_KEY")
        .expect("ESCUREL_GEMINI_API_KEY must be set for the live gemini test");

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
    let instance_page_id = format!("markdown/instances/{SKILL}/{INSTANCE_ID}.md");

    let captured = call_mcp(
        &gateway,
        "capture_event",
        json!({
            "source": "manual",
            "mime": "text/plain",
            "label_skill": SKILL,
            "instance_page_id": instance_page_id,
            "title": "renewal request",
            "body": "Globex asked to renew their contract for another year, \
                     starting in March, and wants the same terms.",
        }),
    )
    .await;
    let event_id = captured["event_id"].as_str().expect("event_id").to_owned();

    let token = gateway.mint_token(TENANT, Role::Agent);
    let listen = format!("127.0.0.1:{}", free_port());
    // Its OWN ledger. Without this the runner falls back to
    // `./escurel-runner-ledger.sqlite` in the crate directory — one file
    // shared by every test in the suite AND by every previous run of it. A
    // row another test left behind is a row this one inherits: the content
    // dedup saw its fixture already folded in and correctly declined to run
    // it again, which is right behaviour reading wrong state.
    let ledger_dir = tempfile::tempdir().expect("tempdir for ledger");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gateway.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_TOKEN", &token)
        .env("ESCUREL_RUNNER_HARNESS", "gemini")
        .env(
            "ESCUREL_RUNNER_LEDGER_PATH",
            ledger_dir.path().join("ledger.sqlite").to_str().unwrap(),
        )
        .env("ESCUREL_GEMINI_API_KEY", api_key)
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms");
    if let Ok(model) = std::env::var("ESCUREL_RUNNER_GEMINI_MODEL") {
        cmd.env("ESCUREL_RUNNER_GEMINI_MODEL", model);
    }
    let _runner = ChildGuard(cmd.spawn().expect("spawn escurel-runner"));

    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        let events = call_mcp(
            &gateway,
            "list_events",
            json!({ "instance_page_id": instance_page_id }),
        )
        .await;
        if events["events"].as_array().is_some_and(|es| {
            es.iter()
                .any(|e| e["event_id"] == json!(event_id) && e["status"] == json!("processed"))
        }) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "real gemini never processed {event_id} within 180s — check /dlq on the runner"
        );
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    let expanded = call_mcp(&gateway, "expand", json!({ "page_id": instance_page_id })).await;
    let body = expanded["body"].as_str().unwrap_or_default();
    assert!(
        body.contains("BASELINE account state"),
        "the fold must preserve what was already there: {body}"
    );
    assert!(
        body.len() > INSTANCE_BODY.len(),
        "the fold must have added something: {body}"
    );
    println!("live gemini folded the event; instance body is now:\n{body}");
}
