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
/// Opted in, one run a day, and nobody runs it: for the gateway-only cases.
const CAPPED: &str = "---\nkind: skill\nid: capped\nautonomy: auto\n\
    schedule: \"0 0 1 1 *\"\nschedule_budget: {runs_per_day: 1}\nschedule_webhook: true\n\
    ---\n# capped\n\nNote the delivery.\n";
/// A workflow skill that opted in: the webhook cannot fire one yet.
const FLOW: &str = "---\nkind: skill\nid: flow\nbackend: {kind: workflow}\n\
    run_skill: workflow-run\nschedule_webhook: true\n---\n# flow\n\nA plan.\n";
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
                .skill("capped", CAPPED)
                .skill("flow", FLOW)
                .skill("plain", PLAIN)
                .done(),
        ),
    })
    .await
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// One delivery as a sender builds it: what it signs, and what it sends.
struct Delivery<'a> {
    skill: &'a str,
    delivery: &'a str,
    body: &'a [u8],
    secret: &'a str,
    /// `X-Escurel-Timestamp`; `None` sends none.
    timestamp: Option<i64>,
    /// The delivery header actually sent, when it differs from the signed one.
    sent_delivery: Option<&'a str>,
    /// The skill actually posted to, when it differs from the signed one.
    sent_skill: Option<&'a str>,
}

impl<'a> Delivery<'a> {
    fn new(skill: &'a str, delivery: &'a str, body: &'a str) -> Self {
        Self {
            skill,
            delivery,
            body: body.as_bytes(),
            secret: SECRET,
            timestamp: Some(unix_now()),
            sent_delivery: None,
            sent_skill: None,
        }
    }

    async fn send(&self, gw: &EscurelProcess) -> (u16, Value) {
        let ts = self.timestamp.map(|t| t.to_string());
        let message = escurel_types::webhook_signature::schedule_message(
            self.skill,
            self.delivery,
            ts.as_deref().unwrap_or(""),
            self.body,
        );
        let mut req = reqwest::Client::new()
            .post(format!(
                "{}/hooks/schedule/{}",
                gw.base_url(),
                self.sent_skill.unwrap_or(self.skill)
            ))
            .header("content-type", "application/json")
            .header(
                "x-escurel-delivery",
                self.sent_delivery.unwrap_or(self.delivery),
            )
            .header(
                "x-escurel-webhook-signature",
                escurel_types::webhook_signature::sign(self.secret, &message),
            );
        if let Some(ts) = ts {
            req = req.header("x-escurel-timestamp", ts);
        }
        let resp = req.body(self.body.to_vec()).send().await.expect("post");
        let status = resp.status().as_u16();
        (status, resp.json().await.unwrap_or(Value::Null))
    }
}

/// `POST /hooks/schedule/<skill>` signed with `secret`, now; status and JSON body.
async fn hook(
    gw: &EscurelProcess,
    skill: &str,
    delivery: &str,
    body: &str,
    secret: &str,
) -> (u16, Value) {
    Delivery {
        secret,
        ..Delivery::new(skill, delivery, body)
    }
    .send(gw)
    .await
}

async fn event_exists(gw: &EscurelProcess, event_id: &str) -> bool {
    let admin = gw.mint_token(TENANT, Role::Admin);
    let r = call(gw, &admin, "list_events", json!({ "event_id": event_id })).await;
    r["events"].as_array().is_some_and(|e| !e.is_empty())
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

/// The signature covers the skill, the delivery id and a timestamp, not just
/// the body: a captured request cannot be replayed under a new delivery id
/// (each would be a new run), nor at all once it is five minutes old.
#[tokio::test]
async fn the_signature_covers_the_delivery_and_a_fresh_timestamp() {
    let gw = gateway().await;
    let (status, body) = Delivery {
        sent_delivery: Some("d-2"),
        ..Delivery::new("capped", "d-1", "{}")
    }
    .send(&gw)
    .await;
    assert_eq!(
        status, 401,
        "a new delivery header under the old signature: {body}"
    );
    for (ts, why) in [
        (Some(unix_now() - 301), "stale"),
        (Some(unix_now() + 301), "from the future"),
        (None, "missing"),
    ] {
        let (status, body) = Delivery {
            timestamp: ts,
            ..Delivery::new("capped", "d-1", "{}")
        }
        .send(&gw)
        .await;
        assert_eq!(status, 401, "a {why} timestamp: {body}");
    }
    let (status, body) = Delivery {
        sent_skill: Some("hooked"),
        ..Delivery::new("capped", "d-1", "{}")
    }
    .send(&gw)
    .await;
    assert_eq!(status, 401, "signed for another skill: {body}");
    assert!(!event_exists(&gw, "hook:capped:d-1").await);
    assert!(!event_exists(&gw, "hook:capped:d-2").await);
    let (status, body) = Delivery::new("capped", "d-1", "{}").send(&gw).await;
    assert_eq!(status, 202, "{body}");
}

/// A delivery once skipped for the budget stays skipped: redelivered after
/// the budget was raised it is a replay, never a run. Past the budget only
/// the day's first skip is recorded; later ones are answered and not stored.
#[tokio::test]
async fn a_skipped_delivery_stays_skipped_and_skips_are_not_recorded_twice_a_day() {
    let gw = gateway().await;
    let admin = gw.mint_token(TENANT, Role::Admin);
    assert_eq!(hook(&gw, "capped", "d-1", "{}", SECRET).await.0, 202);
    let (status, skipped) = hook(&gw, "capped", "d-2", "{}", SECRET).await;
    assert_eq!(
        (status, &skipped["skipped"]),
        (200, &json!(true)),
        "{skipped}"
    );
    assert!(event_exists(&gw, "hook:capped:d-2:skipped").await);
    let (status, later) = hook(&gw, "capped", "d-3", "{}", SECRET).await;
    assert_eq!((status, &later["skipped"]), (200, &json!(true)), "{later}");
    assert!(
        !event_exists(&gw, "hook:capped:d-3:skipped").await,
        "one skipped record a day: {later}"
    );

    let raised = CAPPED.replace("runs_per_day: 1", "runs_per_day: 50");
    call(
        &gw,
        &admin,
        "update_page",
        json!({ "page_id": "markdown/skills/capped.md", "content": raised }),
    )
    .await;
    let (status, again) = hook(&gw, "capped", "d-2", "{}", SECRET).await;
    assert_eq!(status, 200, "{again}");
    assert_eq!(again["skipped"], true, "{again}");
    assert_eq!(again["replayed"], true, "{again}");
    assert!(
        !event_exists(&gw, "hook:capped:d-2").await,
        "no runnable event"
    );
    // A fresh delivery under the raised budget runs.
    assert_eq!(hook(&gw, "capped", "d-4", "{}", SECRET).await.0, 202);
}

/// Count-then-capture is serialised per skill: deliveries racing for a budget
/// of one admit exactly one. Multi-threaded, so the in-process gateway really
/// handles them in parallel, as a production gateway does.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_deliveries_admit_exactly_the_budget() {
    let gw = gateway().await;
    let ids: Vec<String> = (0..16).map(|n| format!("race-{n}")).collect();
    let answers = futures_util::future::join_all(
        ids.iter()
            .map(|d| async { hook(&gw, "capped", d, "{}", SECRET).await }),
    )
    .await;
    let fired = answers.iter().filter(|(s, _)| *s == 202).count();
    let skipped = answers
        .iter()
        .filter(|(s, b)| *s == 200 && b["skipped"] == true)
        .count();
    assert_eq!((fired, skipped), (1, ids.len() - 1), "{answers:?}");
}

/// The edges: a workflow skill cannot be fired yet (422), a body that is not
/// UTF-8 is refused (400), an oversized one is a JSON 413, and with no secret
/// configured the endpoint does not exist.
#[tokio::test]
async fn the_webhook_refuses_workflows_bad_bodies_and_is_absent_without_a_secret() {
    let gw = gateway().await;
    let (status, body) = hook(&gw, "flow", "d-1", "{}", SECRET).await;
    assert_eq!(status, 422, "{body}");
    assert_eq!(body["error"], "workflow_not_supported", "{body}");
    let (status, body) = Delivery {
        body: &[0xff, 0xfe, 0x00],
        ..Delivery::new("capped", "d-1", "")
    }
    .send(&gw)
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (400, Some("bad_body")),
        "{body}"
    );
    let big = "x".repeat(64 * 1024 + 1);
    let (status, body) = hook(&gw, "capped", "d-1", &big, SECRET).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (413, Some("bad_body")),
        "{body}"
    );

    let bare = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("capped", CAPPED)
                .done(),
        ),
        ..Default::default()
    })
    .await;
    let (status, _) = hook(&bare, "capped", "d-1", "{}", SECRET).await;
    assert_eq!(status, 404, "no secret, no endpoint");
}

/// Cron windows and webhook deliveries spend ONE budget: with `runs_per_day:
/// 1` and a delivery first, the runner's next window is a `run-skipped
/// {reason: budget}`, not a second run.
#[tokio::test]
async fn cron_windows_and_webhook_deliveries_share_one_budget() {
    const BOTH: &str = "---\nkind: skill\nid: both\nautonomy: auto\n\
        schedule: \"*/2 * * * * *\"\nschedule_budget: {runs_per_day: 1}\nschedule_webhook: true\n\
        ---\n# both\n\nNote it.\n";
    let gw = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            schedule_webhook_secret: Some(SECRET.to_owned()),
            ..Default::default()
        },
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("both", BOTH)
                .done(),
        ),
    })
    .await;
    assert_eq!(hook(&gw, "both", "d-1", "{}", SECRET).await.0, 202);
    let mut runner = RunnerProcess::echo(&gw, TENANT)
        .env("ESCUREL_RUNNER_SCHEDULE_TICK", "250ms")
        .env("ESCUREL_RUNNER_SCHEDULE_GRACE", "2s")
        .spawn();
    runner.wait_healthy().await;
    let admin = gw.mint_token(TENANT, Role::Admin);
    let deadline = Instant::now() + Duration::from_secs(30);
    let skipped = loop {
        let r = call(
            &gw,
            &admin,
            "list_events",
            json!({ "label_skill": "escurel:run", "limit": 500 }),
        )
        .await;
        let found = r["events"].as_array().and_then(|events| {
            events
                .iter()
                .find(|e| {
                    e["title"] == "run-skipped"
                        && e["event_id"]
                            .as_str()
                            .is_some_and(|id| id.starts_with("sched:both:"))
                })
                .cloned()
        });
        if let Some(found) = found {
            break found;
        }
        assert!(Instant::now() < deadline, "no window was skipped: {r}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let body: Value = serde_json::from_str(skipped["body"].as_str().unwrap()).unwrap();
    assert_eq!(body["reason"], "budget", "{body}");
    let triggers = call(
        &gw,
        &admin,
        "list_events",
        json!({ "label_skill": "both", "limit": 100 }),
    )
    .await;
    let sched_triggers = triggers["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| {
            e["event_id"]
                .as_str()
                .is_some_and(|id| id.starts_with("sched:"))
        })
        .count();
    assert_eq!(
        sched_triggers, 0,
        "the delivery spent the day's run: {triggers}"
    );
}
