//! Per-run actor identity (#510): a run writes as its AGENT, not as the runner.
//!
//! `docs/contract/agent-orchestration.md` named this hole in its own header —
//! "every run for every principal currently shares one privilege" — and the
//! audit consequence is what this test pins: two agents driven by one runner
//! used to leave one indistinguishable trail (`last_written_by:
//! escurel-runner`), so "who set this client to cold?" answered "an agent did".
//!
//! One real gateway, one real runner in MINTED mode (no pasted bearer), two
//! skills, two instances, two events. Nothing is stubbed but the model.
//!
//! The assertion is deliberately relational rather than literal: the two pages
//! must name two DIFFERENT writers, and neither may be the runner. A test that
//! only checked one page against one expected string would still pass if every
//! run shared a single hard-coded identity — which is the bug.

use std::process::{Child, Command};
use std::time::{Duration, Instant};

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role, free_port};
use serde_json::{Value, json};

const TENANT: &str = "acme";
/// Two skills, so the two runs differ in exactly the thing that should drive
/// the identity: `label_skill`.
const SKILLS: [&str; 2] = ["inbox-scan", "crm-hygiene"];
const INSTANCE_ID: &str = "globex";
const MARKER: &str = "PER_RUN_ACTOR_FOLD";
/// The runner's own service subject (`ESCUREL_RUNNER_AUTH_SUBJECT`'s default).
const RUNNER_SUBJECT: &str = "escurel-runner";

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn skill_body(skill: &str) -> String {
    format!(
        "---\ntype: skill\nid: {skill}\nautonomy: auto\n---\n# {skill}\n\nFold the event into the instance.\n"
    )
}

fn instance_body(skill: &str) -> String {
    format!("---\ntype: instance\nid: {INSTANCE_ID}\nskill: {skill}\n---\n# Globex\n\nBASELINE.\n")
}

fn page_id(skill: &str) -> String {
    format!("markdown/instances/{skill}/{INSTANCE_ID}.md")
}

async fn call_mcp(p: &EscurelProcess, name: &str, args: Value) -> Value {
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

/// A model that folds whichever event it is handed. It reads the target page
/// out of the packaged input rather than being told, so ONE stub serves both
/// skills and neither run is special-cased.
async fn spawn_stub_model() -> String {
    use axum::Router;

    async fn generate(body: String) -> axum::Json<Value> {
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
        // The packager renders it as `## Target instance (<page id>)`, so the
        // surrounding punctuation comes off before matching.
        let page = input
            .split_whitespace()
            .map(|w| w.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '/' && c != '.'))
            .find(|w| w.starts_with("markdown/instances/") && w.ends_with(".md"))
            .unwrap_or_default()
            .to_owned();
        let skill = page
            .strip_prefix("markdown/instances/")
            .and_then(|r| r.split('/').next())
            .unwrap_or_default()
            .to_owned();
        let parts = if turn == 1 && !page.is_empty() {
            json!([
                { "functionCall": { "name": "update_page", "args": {
                    "page_id": page,
                    "content": format!(
                        "---\ntype: instance\nid: {INSTANCE_ID}\nskill: {skill}\n---\n\
                         # Globex\n\nBASELINE.\n\n{MARKER} {event_id}\n"
                    ),
                } } },
                { "functionCall": { "name": "assign_event", "args": {
                    "event_id": event_id, "instance_page_id": page,
                } } },
            ])
        } else {
            json!([{ "text": "done" }])
        };
        axum::Json(json!({ "candidates": [{ "content": { "parts": parts } }] }))
    }

    let app = Router::new().fallback(generate);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stub model");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn two_skills_driven_by_one_runner_leave_two_distinguishable_writers() {
    let mut fixtures = FixtureBuilder::new().tenant(TENANT);
    for skill in SKILLS {
        fixtures = fixtures.skill(skill, skill_body(skill).as_str()).instance(
            skill,
            INSTANCE_ID,
            instance_body(skill).as_str(),
        );
    }
    let gateway = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(fixtures.done()),
        ..Default::default()
    })
    .await;

    let mut events = Vec::new();
    for skill in SKILLS {
        let captured = call_mcp(
            &gateway,
            "capture_event",
            json!({
                "source": "manual",
                "mime": "text/plain",
                "label_skill": skill,
                "instance_page_id": page_id(skill),
                "title": "renewal",
                "body": format!("work item for {skill}"),
            }),
        )
        .await;
        events.push(captured["event_id"].as_str().expect("event_id").to_owned());
    }

    let model_base = spawn_stub_model().await;
    let (signing_key, kid) = gateway.signing_material();
    let issuer = gateway.issuer_url();
    let listen = format!("127.0.0.1:{}", free_port());
    // Its own ledger: the shared default file carries rows from every other
    // test in the suite, and an inherited row makes this one silently no-op.
    let ledger_dir = tempfile::tempdir().expect("tempdir for ledger");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gateway.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        // MINTED mode: only a runner holding a signing key can scope a run to
        // its agent, so a static bearer would make this test vacuous.
        .env_remove("ESCUREL_RUNNER_TOKEN")
        .env("ESCUREL_RUNNER_AUTH_ISSUER", &issuer)
        .env("ESCUREL_RUNNER_AUTH_KID", kid)
        .env("ESCUREL_RUNNER_AUTH_SIGNING_KEY", &signing_key)
        .env("ESCUREL_RUNNER_HARNESS", "gemini")
        .env(
            "ESCUREL_RUNNER_LEDGER_PATH",
            ledger_dir.path().join("ledger.sqlite").to_str().unwrap(),
        )
        .env("ESCUREL_GEMINI_API_KEY", "test-key-not-a-real-credential")
        .env("ESCUREL_RUNNER_GEMINI_BASE_URL", &model_base)
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms");
    let _runner = ChildGuard(cmd.spawn().expect("spawn escurel-runner"));

    let deadline = Instant::now() + Duration::from_secs(90);
    let mut writers = Vec::new();
    'outer: loop {
        writers.clear();
        for skill in SKILLS {
            let expanded = call_mcp(&gateway, "expand", json!({ "page_id": page_id(skill) })).await;
            let folded = expanded["body"]
                .as_str()
                .unwrap_or_default()
                .contains(MARKER);
            match expanded["page"]["last_written_by"].as_str() {
                Some(w) if folded => writers.push(w.to_owned()),
                _ => {
                    assert!(
                        Instant::now() < deadline,
                        "{skill} never folded its event — writers so far: {writers:?}"
                    );
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    continue 'outer;
                }
            }
        }
        break;
    }

    assert_eq!(writers.len(), 2, "both runs must have written: {writers:?}");
    assert_ne!(
        writers[0], writers[1],
        "two agents under one runner must not share one identity — that is \
         the audit trail saying only \"an agent did it\": {writers:?}"
    );
    for (skill, writer) in SKILLS.iter().zip(&writers) {
        assert_ne!(
            writer, RUNNER_SUBJECT,
            "the run must not write as the runner: {writers:?}"
        );
        assert!(
            writer.contains(skill),
            "{writer} does not name the agent that ran ({skill})"
        );
    }

    // The delegation chain survives: the event each run folded records the
    // runner it was acting for, server-stamped, so "runner acting as
    // inbox-scan" is recoverable rather than lost to the rename.
    for (skill, event_id) in SKILLS.iter().zip(&events) {
        let listed = call_mcp(
            &gateway,
            "list_events",
            json!({ "instance_page_id": page_id(skill) }),
        )
        .await;
        let event = listed["events"]
            .as_array()
            .expect("events")
            .iter()
            .find(|e| e["event_id"] == json!(event_id))
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            event["status"], "processed",
            "{skill}'s event should be folded: {event}"
        );
    }
}
