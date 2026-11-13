//! A run can be cancelled while its harness is running (knowledge-workbench
//! backend P2-3a — BRD FR-C cancel mechanics).
//!
//! Real gateway, real runner binary, real echo harness told to sleep before
//! it acts. A cancel request (here through the runner's `POST /debug/cancel`,
//! the seam the `escurel:run-control` subscriber uses next) terminates the
//! harness subprocess (SIGTERM, a grace, then SIGKILL), the ledger records
//! the run `cancelled` — terminal, never retried by the poller — the run's
//! `run-finished` says so, nothing landed on the page, the trigger event
//! stays `inbox`, and no cascade is emitted.

use std::time::{Duration, Instant};

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

use crate::support::{RunnerProcess, call};

const TENANT: &str = "acme";
const SKILL_BODY: &str = "---\nkind: skill\nid: renewal\nautonomy: auto\n---\n# renewal\n\nFold the event into the instance.\n";
const INSTANCE_BODY: &str = "---\nkind: instance\nid: c1\nskill: renewal\n---\n# C1\n\nBASELINE.\n";
const PAGE: &str = "markdown/instances/renewal/c1.md";

#[tokio::test]
async fn a_cancelled_run_stops_its_harness_and_lands_nothing() {
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

    let runner = RunnerProcess::echo(&gw, TENANT)
        .static_bearer(&admin)
        .env("ESCUREL_RUNNER_CANCEL_GRACE", "1s")
        // The echo harness idles long enough for the cancel to land mid-run.
        .env("ESCUREL_ECHO_SLEEP_MS", "30000")
        .spawn();
    let listen = runner.listen();

    let r = call(
        &gw,
        &admin,
        "capture_event",
        json!({ "source": "manual", "mime": "text/plain", "label_skill": "renewal",
                "instance_page_id": PAGE, "title": "renew", "body": "please renew" }),
    )
    .await;
    let event_id = r["event_id"].as_str().unwrap().to_owned();

    // The run is in flight: the ledger row is pending and the harness is asleep.
    let run = runner
        .wait_for_status(&event_id, &["pending"], Duration::from_secs(30))
        .await;
    let run_id = run["run_id"].as_str().unwrap().to_owned();
    tokio::time::sleep(Duration::from_millis(500)).await;

    let started = Instant::now();
    let resp = reqwest::Client::new()
        .post(format!("http://{listen}/debug/cancel"))
        .json(&json!({ "tenant": TENANT, "event_id": event_id, "reason": "wrong document" }))
        .send()
        .await
        .expect("cancel");
    assert_eq!(resp.status(), 200, "cancel is acknowledged for a live run");
    let ack: Value = resp.json().await.unwrap();
    assert_eq!(ack["run_id"], run_id, "{ack}");

    // Terminal `cancelled` well inside the harness's 30 s sleep: the
    // subprocess was stopped, not waited out.
    let run = runner
        .wait_for_status(&event_id, &["cancelled"], Duration::from_secs(10))
        .await;
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "cancel took {:?}",
        started.elapsed()
    );
    assert_eq!(run["run_id"], run_id);

    // Nothing landed: the page is untouched and the event is still inbox.
    let page = call(&gw, &admin, "expand", json!({ "page_id": PAGE })).await;
    assert!(
        page["body"].as_str().unwrap().contains("BASELINE."),
        "{page}"
    );
    assert!(!page["body"].as_str().unwrap().contains("renew"), "{page}");
    let inbox = call(&gw, &admin, "list_inbox", json!({})).await;
    let trigger = inbox["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["event_id"] == event_id);
    assert_eq!(
        trigger.map(|e| e["status"].clone()),
        Some(json!("inbox")),
        "{inbox}"
    );

    // The run's record says cancelled, and why.
    let own = call(&gw, &admin, "list_events", json!({ "run_id": run_id })).await;
    let finished = own["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["title"] == "run-finished")
        .unwrap_or_else(|| panic!("no run-finished: {own}"));
    let body: Value = serde_json::from_str(finished["body"].as_str().unwrap()).unwrap();
    assert_eq!(body["status"], "cancelled", "{body}");
    assert_eq!(body["reason"], "wrong document", "{body}");

    // Cancelled is terminal for the poller: the event stays inbox but is not
    // re-run, so the ledger holds exactly one run after a few more polls.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let ledger: Value = reqwest::get(format!("http://{listen}/debug/ledger"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(ledger["total"], 1, "{ledger}");
    assert_eq!(ledger["cancelled"], 1, "{ledger}");

    // A cancel for a run that is not live is a 404, not an error.
    let resp = reqwest::Client::new()
        .post(format!("http://{listen}/debug/cancel"))
        .json(&json!({ "tenant": TENANT, "event_id": event_id }))
        .send()
        .await
        .expect("cancel again");
    assert_eq!(resp.status(), 404);
}
