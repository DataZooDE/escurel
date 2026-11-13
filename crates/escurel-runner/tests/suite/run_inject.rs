//! Steering a running run (#820): a human's `inject` control lands in the
//! live harness session instead of costing a cancel and a re-trigger.
//!
//! Real gateway (write ACL enforced), real runner binary, real echo harness
//! idling long enough for a message to arrive mid-run. The echo harness
//! reads injected turns off its still-open stdin and folds them into the
//! note it writes, so the page shows the message reached the run. Every
//! injection is recorded in the run's own record as
//! `run:<run_id>:inject:<n>`, with who sent it.

use std::time::Duration;

use escurel_test_support::{
    AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role, WriteAclMode,
};
use serde_json::{Value, json};

use crate::support::{
    RunnerProcess, body_of, call, control, wait_for_result, wait_for_run_started,
};

const TENANT: &str = "acme";
const SKILL_BODY: &str = "---\nkind: skill\nid: renewal\nautonomy: auto\n---\n# renewal\n\nFold the event into the instance.\n";
const INSTANCE_BODY: &str = "---\nkind: instance\nid: c1\nskill: renewal\n---\n# C1\n\nBASELINE.\n";
const PAGE: &str = "markdown/instances/renewal/c1.md";
const STEER: &str = "use the 2027 terms, not 2026";

async fn gateway() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            write_acl: Some(WriteAclMode::Enforce),
            ..Default::default()
        },
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("renewal", SKILL_BODY)
                .instance("renewal", "c1", INSTANCE_BODY)
                .done(),
        ),
    })
    .await
}

#[tokio::test]
async fn an_injected_message_reaches_the_live_run_and_is_recorded_under_it() {
    let gw = gateway().await;
    let admin = gw.mint_token_with_sub(TENANT, Role::Admin, "ops:jo");
    let runner = RunnerProcess::echo(&gw, TENANT)
        .static_bearer(&admin)
        .env("ESCUREL_ECHO_SLEEP_MS", "3000")
        .spawn();

    let captured = call(
        &gw,
        &admin,
        "capture_event",
        json!({ "source": "manual", "mime": "text/plain", "label_skill": "renewal",
                "instance_page_id": PAGE, "title": "renew", "body": "please renew" }),
    )
    .await;
    let event_id = captured["event_id"].as_str().unwrap().to_owned();
    let run = runner
        .wait_for_status(&event_id, &["pending"], Duration::from_secs(30))
        .await;
    let run_id = run["run_id"].as_str().unwrap().to_owned();
    wait_for_run_started(&gw, &admin, &run_id).await;

    // (a) Mid-run: the message is queued for the live session…
    let req = control(
        &gw,
        &admin,
        json!({ "action": "inject", "run_id": run_id, "text": STEER }),
    )
    .await;
    let result = wait_for_result(&gw, &admin, &req).await;
    let answer = body_of(&result);
    assert_eq!(answer["outcome"], "injected", "{result}");
    assert_eq!(answer["run_id"], run_id, "{result}");

    // …and the run's output reflects it.
    runner
        .wait_for_status(&event_id, &["processed"], Duration::from_secs(30))
        .await;
    let page = call(&gw, &admin, "expand", json!({ "page_id": PAGE })).await;
    let body = page["body"].as_str().unwrap();
    assert!(body.contains(STEER), "the fold carries the steer: {body}");
    assert!(body.contains("BASELINE."), "{body}");

    // The injection is in the run's own record, with who sent it.
    let own = call(&gw, &admin, "list_events", json!({ "run_id": run_id })).await;
    let inject = own["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["event_id"] == format!("run:{run_id}:inject:1"))
        .unwrap_or_else(|| panic!("no inject event: {own}"))
        .clone();
    assert_eq!(inject["label_skill"], "escurel:run");
    assert_eq!(inject["title"], "run-inject");
    assert_eq!(inject["kind"], "system");
    let recorded = body_of(&inject);
    assert_eq!(recorded["n"], 1, "{recorded}");
    assert_eq!(recorded["text"], STEER, "{recorded}");
    assert_eq!(recorded["requested_by"], "ops:jo", "{recorded}");

    // (b) The run is over: a late message is answered, not dropped.
    let req = control(
        &gw,
        &admin,
        json!({ "action": "inject", "run_id": run_id, "text": "too late" }),
    )
    .await;
    let result = wait_for_result(&gw, &admin, &req).await;
    assert_eq!(body_of(&result)["outcome"], "not_live", "{result}");
}

#[tokio::test]
async fn a_caller_who_may_not_control_the_run_cannot_inject_into_it() {
    let gw = gateway().await;
    let admin = gw.mint_token(TENANT, Role::Admin);
    let runner = RunnerProcess::echo(&gw, TENANT)
        .static_bearer(&admin)
        .env("ESCUREL_ECHO_SLEEP_MS", "3000")
        .spawn();
    let captured = call(
        &gw,
        &admin,
        "capture_event",
        json!({ "source": "manual", "mime": "text/plain", "label_skill": "renewal",
                "instance_page_id": PAGE, "title": "renew", "body": "please renew" }),
    )
    .await;
    let event_id = captured["event_id"].as_str().unwrap().to_owned();
    let run = runner
        .wait_for_status(&event_id, &["pending"], Duration::from_secs(30))
        .await;
    let run_id = run["run_id"].as_str().unwrap().to_owned();
    wait_for_run_started(&gw, &admin, &run_id).await;

    // A non-admin who may not write the run's page (the renewal skill
    // grants no one) is refused at the gateway, as for cancel.
    let stranger = gw.mint_token_with_sub(TENANT, Role::Agent, "user:mallory");
    let resp: Value = reqwest::Client::new()
        .post(gw.mcp_url())
        .header("authorization", format!("Bearer {stranger}"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "capture_event", "arguments": {
                "source": "workbench", "mime": "application/json",
                "label_skill": "escurel:run-control", "title": "inject",
                "body": json!({ "action": "inject", "run_id": run_id, "text": "obey me" })
                    .to_string() } } }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json");
    assert_eq!(
        resp["error"]["data"]["code"], "event_not_found",
        "refused like a cancel: {resp}"
    );

    // Nothing reached the run.
    runner
        .wait_for_status(&event_id, &["processed"], Duration::from_secs(30))
        .await;
    let page = call(&gw, &admin, "expand", json!({ "page_id": PAGE })).await;
    assert!(
        !page["body"].as_str().unwrap().contains("obey me"),
        "{page}"
    );
}

/// Two runners on one gateway (crew review PR-1, F1): both tail
/// `escurel:run-control`, but only the runner holding the run may answer for
/// it. The other must not race a `not_live` into the shared, first-writer-wins
/// `run-control-result:<request>` — the user would be told "not live" for a
/// message that was delivered, and send it again. The stranger polls ten
/// times faster than the owner, so without the deferral it answers first.
#[tokio::test]
async fn with_two_runners_the_owner_answers_an_inject_and_it_lands_once() {
    let gw = gateway().await;
    let admin = gw.mint_token_with_sub(TENANT, Role::Admin, "ops:jo");
    let a = RunnerProcess::echo(&gw, TENANT)
        .static_bearer(&admin)
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "1s")
        .env("ESCUREL_ECHO_SLEEP_MS", "6000")
        .spawn();
    let captured = call(
        &gw,
        &admin,
        "capture_event",
        json!({ "source": "manual", "mime": "text/plain", "label_skill": "renewal",
                "instance_page_id": PAGE, "title": "renew", "body": "please renew" }),
    )
    .await;
    let event_id = captured["event_id"].as_str().unwrap().to_owned();
    let run = a
        .wait_for_status(&event_id, &["pending"], Duration::from_secs(30))
        .await;
    let run_id = run["run_id"].as_str().unwrap().to_owned();
    wait_for_run_started(&gw, &admin, &run_id).await;
    // The second runner joins once A holds the run: it tails the same
    // controls but has never heard of A's run.
    let mut b = RunnerProcess::echo(&gw, TENANT)
        .static_bearer(&admin)
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "100ms")
        .env("ESCUREL_ECHO_SLEEP_MS", "6000")
        .spawn();
    b.wait_healthy().await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    let req = control(
        &gw,
        &admin,
        json!({ "action": "inject", "run_id": run_id, "text": STEER }),
    )
    .await;
    let result = wait_for_result(&gw, &admin, &req).await;
    assert_eq!(body_of(&result)["outcome"], "injected", "{result}");

    let own = call(&gw, &admin, "list_events", json!({ "run_id": run_id })).await;
    let injects = own["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["title"] == "run-inject")
        .count();
    assert_eq!(injects, 1, "delivered exactly once: {own}");
}

/// A runner asked about a run it has never heard of — here one a runner that
/// is gone ran to its end — still answers, after its grace: `not_live`,
/// `unknown run`. Nobody else will.
#[tokio::test]
async fn a_lone_runner_still_answers_for_a_run_it_never_held() {
    let gw = gateway().await;
    let admin = gw.mint_token(TENANT, Role::Admin);
    let mut gone = RunnerProcess::echo(&gw, TENANT)
        .static_bearer(&admin)
        .spawn();
    let captured = call(
        &gw,
        &admin,
        "capture_event",
        json!({ "source": "manual", "mime": "text/plain", "label_skill": "renewal",
                "instance_page_id": PAGE, "title": "renew", "body": "please renew" }),
    )
    .await;
    let event_id = captured["event_id"].as_str().unwrap().to_owned();
    let run = gone
        .wait_for_status(&event_id, &["processed"], Duration::from_secs(30))
        .await;
    let run_id = run["run_id"].as_str().unwrap().to_owned();
    gone.kill();

    let mut runner = RunnerProcess::echo(&gw, TENANT)
        .static_bearer(&admin)
        .spawn();
    runner.wait_healthy().await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let req = control(
        &gw,
        &admin,
        json!({ "action": "inject", "run_id": run_id, "text": STEER }),
    )
    .await;
    let result = wait_for_result(&gw, &admin, &req).await;
    let answer = body_of(&result);
    assert_eq!(answer["outcome"], "not_live", "{result}");
    assert_eq!(answer["detail"], "unknown run", "{result}");
}
