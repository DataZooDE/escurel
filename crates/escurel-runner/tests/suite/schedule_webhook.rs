//! A webhook fires a scheduled skill (#827): `POST /hooks/schedule/<skill>`
//! at the gateway, HMAC-signed with `ESCUREL_SCHEDULE_WEBHOOK_SECRET`, for a
//! skill that opts in with `schedule_webhook: true`. The gateway captures
//! `hook:<skill>:<delivery-id>` (first writer wins, so a redelivery is one
//! event); the runner runs it like any inbox event.
//!
//! Real gateway, real runner binary (static admin bearer), real echo harness.

use std::time::{Duration, Instant};

use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

use crate::support::{RunnerProcess, call};

const TENANT: &str = "acme";
const SECRET: &str = "hook-secret-for-tests";
/// Opted in, one run a day; its cron never fires during a test (1 January).
const HOOKED: &str = "---\nkind: skill\nid: hooked\nautonomy: auto\n\
    schedule: \"0 0 1 1 *\"\nschedule_budget: {runs_per_day: 1}\nschedule_webhook: true\n\
    ---\n# hooked\n\nNote the delivery.\n";
/// Scheduled, but not triggerable by a webhook.
const PLAIN: &str = "---\nkind: skill\nid: plain\nautonomy: auto\nschedule: \"0 0 1 1 *\"\n\
    ---\n# plain\n\nNote the time.\n";

async fn gateway() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            schedule_webhook_secret: Some(SECRET.to_owned()),
            ..Default::default()
        },
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("hooked", HOOKED)
                .skill("plain", PLAIN)
                .done(),
        ),
    })
    .await
}

/// `POST /hooks/schedule/<skill>` signed with `secret`; status and JSON body.
async fn hook(
    gw: &EscurelProcess,
    skill: &str,
    delivery: &str,
    body: &str,
    secret: &str,
) -> (u16, Value) {
    let resp = reqwest::Client::new()
        .post(format!("{}/hooks/schedule/{skill}", gw.base_url()))
        .header("content-type", "application/json")
        .header("x-escurel-delivery", delivery)
        .header(
            "x-escurel-webhook-signature",
            escurel_types::webhook_signature::sign(secret, body.as_bytes()),
        )
        .body(body.to_owned())
        .send()
        .await
        .expect("post");
    let status = resp.status().as_u16();
    (status, resp.json().await.unwrap_or(Value::Null))
}

async fn run_starts(gw: &EscurelProcess, event_id: &str) -> usize {
    let admin = gw.mint_token(TENANT, Role::Admin);
    let r = call(
        gw,
        &admin,
        "list_events",
        json!({ "label_skill": "escurel:run", "limit": 500 }),
    )
    .await;
    r["events"].as_array().map_or(0, |events| {
        events
            .iter()
            .filter(|e| {
                e["title"] == "run-started" && e["provenance"]["runner"]["event_id"] == event_id
            })
            .count()
    })
}

#[tokio::test]
async fn a_signed_webhook_runs_an_opted_in_skill_once_within_its_budget() {
    let gw = gateway().await;
    let mut runner = RunnerProcess::echo(&gw, TENANT).spawn();
    runner.wait_healthy().await;
    let payload = r#"{"ref":"refs/heads/main"}"#;

    // Refused before anything is captured: a bad signature, then a skill
    // that did not opt in (the signature is checked first, so a stranger
    // cannot probe which skills exist).
    let (status, _) = hook(&gw, "hooked", "d-1", payload, "wrong-secret").await;
    assert_eq!(status, 401);
    let (status, _) = hook(&gw, "plain", "d-1", payload, SECRET).await;
    assert_eq!(status, 404);
    let (status, _) = hook(&gw, "hooked", "bad id!", payload, SECRET).await;
    assert_eq!(status, 400, "the delivery id is charset-bounded");

    // A signed POST: one event, one run.
    let (status, fired) = hook(&gw, "hooked", "d-1", payload, SECRET).await;
    assert_eq!(status, 202, "{fired}");
    assert_eq!(fired["event_id"], "hook:hooked:d-1", "{fired}");
    let run = runner
        .wait_for_status("hook:hooked:d-1", &["processed"], Duration::from_secs(60))
        .await;
    assert_eq!(run["status"], "processed", "{run}");
    let admin = gw.mint_token(TENANT, Role::Admin);
    let stored = call(
        &gw,
        &admin,
        "list_events",
        json!({ "event_id": "hook:hooked:d-1" }),
    )
    .await;
    let event = &stored["events"][0];
    assert_eq!(event["source"], "schedule-webhook", "{stored}");
    assert_eq!(event["body"], payload, "{stored}");
    assert_eq!(
        event["provenance"]["webhook"]["delivery"], "d-1",
        "{stored}"
    );

    // The same delivery again: nothing new.
    let (status, replay) = hook(&gw, "hooked", "d-1", payload, SECRET).await;
    assert_eq!(status, 200, "{replay}");
    assert_eq!(replay["replayed"], true, "{replay}");

    // A second delivery the same day: past `runs_per_day: 1`, skipped and said so.
    let (status, skipped) = hook(&gw, "hooked", "d-2", payload, SECRET).await;
    assert_eq!(status, 200, "{skipped}");
    assert_eq!(skipped["skipped"], true, "{skipped}");
    let record = call(
        &gw,
        &admin,
        "list_events",
        json!({ "event_id": "hook:hooked:d-2:skipped" }),
    )
    .await;
    let record = &record["events"][0];
    assert_eq!(record["title"], "run-skipped", "{record}");
    let body: Value = serde_json::from_str(record["body"].as_str().unwrap()).unwrap();
    assert_eq!(body["reason"], "budget", "{body}");
    assert_eq!(body["trigger_event_id"], "hook:hooked:d-2", "{body}");

    // A few poll cycles later: still exactly one run, and no event for d-2.
    let settle = Instant::now() + Duration::from_secs(3);
    while Instant::now() < settle {
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert_eq!(run_starts(&gw, "hook:hooked:d-1").await, 1);
    let none = call(
        &gw,
        &admin,
        "list_events",
        json!({ "event_id": "hook:hooked:d-2" }),
    )
    .await;
    assert_eq!(none["events"].as_array().map(Vec::len), Some(0), "{none}");
}

/// `hook:` ids are the gateway's: no caller files one through `capture_event`.
#[tokio::test]
async fn a_hook_id_cannot_be_prefiled_by_a_member() {
    let gw = gateway().await;
    let member = gw.mint_token(TENANT, Role::Agent);
    let r: Value = reqwest::Client::new()
        .post(gw.mcp_url())
        .header("authorization", format!("Bearer {member}"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "capture_event", "arguments": {
                "event_id": "hook:hooked:d-9", "source": "x", "mime": "text/plain",
                "label_skill": "hooked", "title": "t", "body": "b" } } }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(r.get("error").is_some(), "{r}");
    let (status, _) = hook(&gw, "hooked", "d-9", "{}", SECRET).await;
    assert_eq!(
        status, 202,
        "the delivery is still the gateway's to capture"
    );
}
