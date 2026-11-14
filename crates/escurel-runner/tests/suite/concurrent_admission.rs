//! Concurrent admission (#828): one poll claims its inbox events with up to
//! `ESCUREL_RUNNER_ADMIT_CONCURRENCY` `claim_event` calls in flight, instead
//! of one call after another.
//!
//! A gateway stand-in sits in front of the real gateway, holds every
//! `claim_event` for a moment and counts how many were in flight at once — so
//! the measure is overlap, not wall time, and a slow CI host cannot make it
//! flaky. The two concurrencies run one after the other. Real gateway, real
//! runner, real echo harness; the stand-in only delays and counts.

use std::time::{Duration, Instant};

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::json;

use crate::support::{GatewayProxy, ProxyRule, RunnerProcess, RunningRunner, call};

const TENANT: &str = "acme";
const NOTE: &str =
    "---\nkind: skill\nid: note\nautonomy: auto\n---\n# note\n\nFold the event in.\n";
const EVENTS: usize = 8;

async fn gateway() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("note", NOTE)
                .instance(
                    "note",
                    "c1",
                    "---\nkind: instance\nid: c1\nskill: note\n---\n# C1\n\nBASELINE.\n",
                )
                .done(),
        ),
        ..Default::default()
    })
    .await
}

/// The most `claim_event` calls in flight at once while a runner at
/// `concurrency` admits [`EVENTS`] inbox events.
async fn claims_in_flight(concurrency: &str) -> usize {
    let gw = gateway().await;
    let admin = gw.mint_token(TENANT, Role::Admin);
    let mut events = Vec::new();
    for n in 0..EVENTS {
        let r = call(
            &gw,
            &admin,
            "capture_event",
            json!({ "source": "test", "mime": "text/plain", "label_skill": "note",
                    "instance_page_id": "markdown/instances/note/c1.md",
                    "title": format!("work {n}"), "body": "work" }),
        )
        .await;
        events.push(r["event_id"].as_str().unwrap().to_owned());
    }
    // Long enough that calls issued together overlap however slow the host is.
    let proxy = GatewayProxy::start(
        gw.base_url(),
        ProxyRule {
            tool: "claim_event",
            delay: Duration::from_millis(500),
            fail_with: None,
        },
    )
    .await;
    let mut runner: RunningRunner = RunnerProcess::echo(&gw, TENANT)
        .env("ESCUREL_RUNNER_GATEWAY_URL", &proxy.url)
        .env("ESCUREL_RUNNER_ADMIT_CONCURRENCY", concurrency)
        .spawn();
    runner.wait_healthy().await;
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let mut admitted = 0;
        for e in &events {
            if runner.debug_run(e).await.is_some() {
                admitted += 1;
            }
        }
        if admitted == EVENTS {
            return proxy.max_in_flight();
        }
        assert!(
            Instant::now() < deadline,
            "only {admitted}/{EVENTS} admitted at concurrency {concurrency}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn a_poll_claims_its_inbox_concurrently_and_serially_only_when_asked() {
    let serial = claims_in_flight("1").await;
    assert_eq!(serial, 1, "concurrency 1 claims one event at a time");
    let concurrent = claims_in_flight("8").await;
    assert!(
        concurrent >= 2,
        "concurrency 8 has several claims in flight together, saw at most {concurrent}"
    );
}
