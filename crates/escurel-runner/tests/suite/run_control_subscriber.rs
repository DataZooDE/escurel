//! The runner acts on `escurel:run-control` events (knowledge-workbench
//! backend P2-3b — BRD FR-C). A human's request, authorised and stamped by
//! the gateway (P2-2), is tailed by the runner, acted on — cancel, retry,
//! pause, resume, requeue — and answered under `escurel:run-control-result`,
//! one result per request, in the run's own record.
//!
//! Real gateway, real runner binary, real echo harness idling long enough
//! for a cancel to land mid-run.

use std::time::Duration;

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::json;

use crate::support::{
    RunnerProcess, RunningRunner, body_of, call, control, wait_for_result, wait_for_run_started,
};

const TENANT: &str = "acme";
const SKILL_BODY: &str = "---\nkind: skill\nid: renewal\nautonomy: auto\n---\n# renewal\n\nFold the event into the instance.\n";
const INSTANCE_BODY: &str = "---\nkind: instance\nid: c1\nskill: renewal\n---\n# C1\n\nBASELINE.\n";
const PAGE: &str = "markdown/instances/renewal/c1.md";
const ECHO_SLEEP_MS: &str = "4000";

fn spawn_runner(gw: &EscurelProcess, admin: &str) -> RunningRunner {
    RunnerProcess::echo(gw, TENANT)
        .static_bearer(admin)
        .env("ESCUREL_RUNNER_CANCEL_GRACE", "1s")
        .env("ESCUREL_ECHO_SLEEP_MS", ECHO_SLEEP_MS)
        .spawn()
}

async fn capture(p: &EscurelProcess, token: &str, title: &str) -> String {
    let r = call(
        p,
        token,
        "capture_event",
        json!({ "source": "manual", "mime": "text/plain", "label_skill": "renewal",
                "instance_page_id": PAGE, "title": title, "body": "please renew" }),
    )
    .await;
    r["event_id"].as_str().unwrap().to_owned()
}

/// A `retry` that lands while the tenant is paused is throttled at
/// admission; the row must go back to a retriable terminal for the poller
/// to re-drive after `resume`, not sit `pending` with nothing queued
/// (codex second-opinion review of P2, P1).
#[tokio::test]
async fn a_throttled_retry_is_re_driven_by_the_poller_not_wedged() {
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
    let admin = gw.mint_token_with_sub(TENANT, Role::Admin, "ops:jo");
    let runner = spawn_runner(&gw, &admin);

    // Run 1, cancelled → a retriable terminal.
    let e1 = capture(&gw, &admin, "renew one").await;
    let run1 = runner
        .wait_for_status(&e1, &["pending"], Duration::from_secs(30))
        .await["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    wait_for_run_started(&gw, &admin, &run1).await;
    control(&gw, &admin, json!({ "action": "cancel", "run_id": run1 })).await;
    runner
        .wait_for_status(&e1, &["cancelled"], Duration::from_secs(10))
        .await;

    // Paused, the retry is admitted by nobody: answered `requeued`, and the
    // row must not be left `pending` with nothing queued.
    let req = control(&gw, &admin, json!({ "action": "pause" })).await;
    wait_for_result(&gw, &admin, &req).await;
    let req = control(&gw, &admin, json!({ "action": "retry", "run_id": run1 })).await;
    let result = wait_for_result(&gw, &admin, &req).await;
    assert_eq!(body_of(&result)["outcome"], "requeued", "{result}");
    tokio::time::sleep(Duration::from_millis(800)).await;
    let run = runner
        .wait_for_status(
            &e1,
            &["failed", "dead_letter", "processed"],
            Duration::from_secs(5),
        )
        .await;
    assert_ne!(run["status"], "pending", "{run}");

    // Resumed, the poller re-drives it to completion.
    let req = control(&gw, &admin, json!({ "action": "resume" })).await;
    wait_for_result(&gw, &admin, &req).await;
    let run = runner
        .wait_for_status(&e1, &["processed"], Duration::from_secs(40))
        .await;
    assert_eq!(run["status"], "processed", "{run}");
}

#[tokio::test]
async fn the_runner_acts_on_control_events_and_answers_each_one() {
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
    let admin = gw.mint_token_with_sub(TENANT, Role::Admin, "ops:jo");

    let runner = spawn_runner(&gw, &admin);

    // --- cancel: a live run is stopped, and the answer names it.
    let e1 = capture(&gw, &admin, "renew one").await;
    let run = runner
        .wait_for_status(&e1, &["pending"], Duration::from_secs(30))
        .await;
    let run1 = run["run_id"].as_str().unwrap().to_owned();
    wait_for_run_started(&gw, &admin, &run1).await;
    let req = control(
        &gw,
        &admin,
        json!({ "action": "cancel", "run_id": run1, "reason": "wrong document" }),
    )
    .await;
    let run = runner
        .wait_for_status(&e1, &["cancelled"], Duration::from_secs(10))
        .await;
    assert_eq!(run["run_id"], run1);
    let result = wait_for_result(&gw, &admin, &req).await;
    let body = body_of(&result);
    assert_eq!(body["action"], "cancel", "{result}");
    assert_eq!(body["outcome"], "cancelled", "{result}");
    assert_eq!(body["run_id"], run1, "{result}");
    assert_eq!(result["kind"], "system");
    assert_eq!(
        result["instance_page_id"], PAGE,
        "answered where the request was filed"
    );
    assert_eq!(result["run_id"], run1, "in the run's own record");
    assert_eq!(
        result["provenance"]["control"]["requested_by"], "ops:jo",
        "{result}"
    );
    // The cancel reason reached the run's terminal.
    let own = call(&gw, &admin, "list_events", json!({ "run_id": run1 })).await;
    let finished = own["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["title"] == "run-finished")
        .expect("run-finished");
    assert_eq!(body_of(finished)["reason"], "wrong document");

    // --- cancel of a run that is not live: answered, not acted on.
    wait_for_run_started(&gw, &admin, &run1).await;
    let req = control(&gw, &admin, json!({ "action": "cancel", "run_id": run1 })).await;
    let result = wait_for_result(&gw, &admin, &req).await;
    assert_eq!(body_of(&result)["outcome"], "not_live", "{result}");

    // --- retry: the cancelled run is re-driven as a fresh run, which lands.
    let req = control(&gw, &admin, json!({ "action": "retry", "run_id": run1 })).await;
    let result = wait_for_result(&gw, &admin, &req).await;
    let body = body_of(&result);
    assert_eq!(body["outcome"], "requeued", "{result}");
    let run2 = body["new_run_id"].as_str().expect("new run id").to_owned();
    assert_ne!(run2, run1);
    let run = runner
        .wait_for_status(&e1, &["processed"], Duration::from_secs(30))
        .await;
    assert_eq!(run["run_id"], run2, "{run}");

    // --- pause: a new event is not admitted while the tenant is paused…
    let req = control(&gw, &admin, json!({ "action": "pause" })).await;
    let result = wait_for_result(&gw, &admin, &req).await;
    assert_eq!(body_of(&result)["outcome"], "paused", "{result}");
    let e2 = capture(&gw, &admin, "renew two").await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(
        runner.debug_run(&e2).await.is_none(),
        "paused: no run claimed"
    );
    // …and runs once resumed.
    let req = control(&gw, &admin, json!({ "action": "resume" })).await;
    let result = wait_for_result(&gw, &admin, &req).await;
    assert_eq!(body_of(&result)["outcome"], "resumed", "{result}");
    runner
        .wait_for_status(&e2, &["pending", "processed"], Duration::from_secs(30))
        .await;

    // --- requeue of an event that is not dead-lettered is refused, with why.
    let req = control(&gw, &admin, json!({ "action": "requeue", "event_id": e1 })).await;
    let result = wait_for_result(&gw, &admin, &req).await;
    let body = body_of(&result);
    assert_eq!(body["outcome"], "refused", "{result}");
    assert!(
        body["detail"].as_str().unwrap_or("").contains("dead"),
        "{result}"
    );

    // Every request got exactly one answer.
    let results = call(
        &gw,
        &admin,
        "list_events",
        json!({ "label_skill": "escurel:run-control-result" }),
    )
    .await;
    assert_eq!(results["events"].as_array().unwrap().len(), 6, "{results}");
    let inbox = call(&gw, &admin, "list_inbox", json!({})).await;
    assert!(
        inbox["events"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| !e["label_skill"].as_str().unwrap().starts_with("escurel:")),
        "no control traffic in the inbox: {inbox}"
    );
}
