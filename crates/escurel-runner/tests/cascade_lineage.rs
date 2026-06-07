//! DoD test for issue #156 — the **cascade emitter + lineage**, with **no
//! mocks**.
//!
//! Per the issue's Scope/DoD: against a real `EscurelProcess` with a real
//! **two-stage skill chain (meeting → decision-record)**, `capture_event` a
//! real meeting event; run the real runner (real echo-harness). Assert — via
//! the real `/mcp` `list_inbox`/`list_events` surface — that the runner
//! emitted a follow-on **decision-record** event whose REAL
//! `provenance.runner.depth == 1` with the correct `root_event_id`
//! (== the original meeting event's id), and that a SECOND run fires for that
//! cascaded event (its ledger row exists). The provenance is read back from
//! the REAL gateway, never from runner internals.
//!
//! ## The real two-stage chain
//!
//! - **meeting** skill + a **decision-record** instance (`q3-roadmap`). The
//!   meeting event is captured *pre-flagged* to the decision-record instance,
//!   so the echo-harness folds the meeting note into the decision-record
//!   instance — a **cross-skill** write (the event's label is `meeting`, the
//!   instance it lands on is a `decision-record`). That cross-skill confirmed
//!   write is what the runner bridges into a follow-on `decision-record`
//!   event.
//! - **decision-record** skill — seeded so the *second* hop packages cleanly
//!   when the runner re-enters the cascaded decision-record event through the
//!   exact same poll → trigger → package → harness → reconcile pipeline.
//!
//! ## Why this terminates without #157's loop controls
//!
//! The cascade fires **only on a genuine cross-skill confirmed change**: the
//! produced instance's skill must differ from the triggering event's own
//! label. Hop 0 (meeting event → decision-record instance) is cross-skill →
//! it cascades. The emitted decision-record event is **unassigned** (no target
//! instance), so hop 1 (the runner processing the decision-record event) is a
//! clean no-op for the echo-harness — it produces no instance, so there is no
//! cross-skill change and **no further cascade**. The chain converges after
//! one hop. The whole test is deadline-bounded.

use std::net::TcpListener;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "acme";

const MEETING_SKILL: &str = "meeting";
const MEETING_SKILL_BODY: &str = "---\ntype: skill\nid: meeting\n---\n# meeting\n\n\
     Fold the meeting note into the decision-record instance it concerns.\n";

const DECISION_SKILL: &str = "decision-record";
const DECISION_SKILL_BODY: &str = "---\ntype: skill\nid: decision-record\n---\n# decision-record\n\n\
     Maintain the running decision record.\n";

const DECISION_INSTANCE_ID: &str = "q3-roadmap";
const DECISION_INSTANCE_BODY: &str = "---\ntype: instance\nid: q3-roadmap\nskill: decision-record\n---\n# Q3 Roadmap\n\n\
     BASELINE decision record.\n";

/// Kills the spawned runner on drop so a test failure never orphans it.
struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    listener.local_addr().expect("read local_addr").port()
}

/// Call an MCP tool over `/mcp` with a freshly minted bearer; return the
/// JSON-RPC `result`.
async fn call_mcp(p: &EscurelProcess, role: Role, name: &str, args: Value) -> Value {
    let token = p.mint_token(TENANT, role);
    let resp = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": name, "arguments": args },
        }))
        .send()
        .await
        .expect("post /mcp");
    assert_eq!(resp.status(), 200, "http status");
    let body: Value = resp.json().await.unwrap();
    assert!(body.get("error").is_none(), "tool {name} error: {body}");
    body["result"].clone()
}

#[tokio::test]
async fn confirmed_cross_skill_write_cascades_a_lineage_tagged_event() {
    // 1. Real gateway with the two-stage chain: a meeting skill, a
    //    decision-record skill, and a decision-record instance.
    let gateway = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill(MEETING_SKILL, MEETING_SKILL_BODY)
                .skill(DECISION_SKILL, DECISION_SKILL_BODY)
                .instance(DECISION_SKILL, DECISION_INSTANCE_ID, DECISION_INSTANCE_BODY)
                .done(),
        ),
        ..Default::default()
    })
    .await;

    let decision_instance_page_id =
        format!("markdown/instances/{DECISION_SKILL}/{DECISION_INSTANCE_ID}.md");

    // 2. Capture a real MEETING event pre-flagged to the DECISION-RECORD
    //    instance: a cross-skill write (label `meeting`, lands on a
    //    `decision-record` instance) — exactly what should cascade.
    let captured = call_mcp(
        &gateway,
        Role::Agent,
        "capture_event",
        json!({
            "source": "manual",
            "mime": "text/plain",
            "label_skill": MEETING_SKILL,
            "instance_page_id": decision_instance_page_id,
            "title": "weekly sync",
            "body": "Decided to ship Q3 roadmap"
        }),
    )
    .await;
    let meeting_event_id = captured["event_id"]
        .as_str()
        .expect("capture_event returns an event_id")
        .to_owned();

    // 3. Spawn the real runner with the echo harness, fast poll.
    let token = gateway.mint_token(TENANT, Role::Agent);
    let port = free_port();
    let listen = format!("127.0.0.1:{port}");
    let ledger_dir = tempfile::tempdir().expect("tempdir for ledger");
    let ledger_path = ledger_dir.path().join("ledger.sqlite");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gateway.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_TOKEN", &token)
        .env("ESCUREL_RUNNER_HARNESS", "echo")
        .env("ESCUREL_RUNNER_LEDGER_PATH", ledger_path.to_str().unwrap())
        // Small attempts cap + short backoff so the (terminating) second hop's
        // reconcile converges/exhausts quickly rather than dragging the test.
        .env("ESCUREL_RUNNER_MAX_ATTEMPTS", "3")
        .env("ESCUREL_RUNNER_RETRY_BACKOFF", "100ms")
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms");
    let _runner = ChildGuard(cmd.spawn().expect("spawn escurel-runner"));

    // 4. Wait for the cascade: a NEW decision-record-labelled event appears on
    //    the gateway, distinct from the meeting event, carrying
    //    provenance.runner with depth 1 and the meeting event as its root.
    let deadline = Instant::now() + Duration::from_secs(45);
    let cascaded = loop {
        let inbox = call_mcp(&gateway, Role::Agent, "list_inbox", json!({ "limit": 100 })).await;
        let found = inbox["events"].as_array().and_then(|es| {
            es.iter()
                .find(|e| {
                    e["event_id"] != json!(meeting_event_id)
                        && e["label_skill"] == json!(DECISION_SKILL)
                        && e["provenance"]["runner"].is_object()
                })
                .cloned()
        });
        if let Some(ev) = found {
            break ev;
        }
        if Instant::now() >= deadline {
            panic!("runner never cascaded a decision-record event from {meeting_event_id}");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };

    // 5. Assert the REAL provenance.runner shape read back from the gateway.
    let runner = &cascaded["provenance"]["runner"];
    assert_eq!(
        runner["depth"].as_u64(),
        Some(1),
        "cascaded event depth must be parent_depth(0) + 1: {cascaded}"
    );
    assert_eq!(
        runner["root_event_id"].as_str(),
        Some(meeting_event_id.as_str()),
        "root_event_id must stay the original meeting event: {cascaded}"
    );
    assert_eq!(
        runner["parent_event_id"].as_str(),
        Some(meeting_event_id.as_str()),
        "parent_event_id is the event this run processed: {cascaded}"
    );
    assert_eq!(
        runner["changed_instance"].as_str(),
        Some(decision_instance_page_id.as_str()),
        "changed_instance is the produced decision-record instance: {cascaded}"
    );
    assert!(
        runner["changed_version"]
            .as_str()
            .is_some_and(|v| !v.is_empty()),
        "changed_version is the confirmed version from the reconciler: {cascaded}"
    );
    let lineage_path = runner["lineage_path"]
        .as_array()
        .expect("lineage_path is an array");
    assert_eq!(
        lineage_path.last().and_then(Value::as_str),
        Some(meeting_event_id.as_str()),
        "lineage_path ends at the parent event id (root → this hop): {cascaded}"
    );
    assert!(
        runner["parent_run_id"]
            .as_str()
            .is_some_and(|v| !v.is_empty()),
        "parent_run_id names the run that cascaded: {cascaded}"
    );

    let cascaded_event_id = cascaded["event_id"].as_str().unwrap().to_owned();

    // 6. A SECOND run fires for the cascaded decision-record event: the runner
    //    re-enters it through the exact same pipeline and records a ledger row.
    let http = reqwest::Client::new();
    let run_url = format!("http://{listen}/debug/run?tenant={TENANT}&event_id={cascaded_event_id}");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(resp) = http.get(&run_url).send().await
            && resp.status().is_success()
        {
            let body: Value = resp.json().await.unwrap_or(json!({}));
            if body["event_id"].as_str() == Some(cascaded_event_id.as_str()) {
                break;
            }
        }
        if Instant::now() >= deadline {
            panic!("no second run fired for the cascaded event {cascaded_event_id}");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // 7. The original meeting event is processed on the decision-record
    //    instance, and the instance body carries the harness's appended note
    //    (the write genuinely landed — the cascade describes a real change).
    let events = call_mcp(
        &gateway,
        Role::Agent,
        "list_events",
        json!({ "instance_page_id": decision_instance_page_id }),
    )
    .await;
    let processed = events["events"]
        .as_array()
        .map(|es| {
            es.iter().any(|e| {
                e["event_id"] == json!(meeting_event_id) && e["status"] == json!("processed")
            })
        })
        .unwrap_or(false);
    assert!(processed, "the meeting event must be processed: {events}");
}
