//! Which inbox events a runner dispatches (`ESCUREL_RUNNER_DISPATCH`), and a
//! requeue that has nothing left to run.
//!
//! Real gateway, real `escurel-runner` on the echo harness, minted per-run
//! tokens. Under `ESCUREL_RUNNER_DISPATCH=agent` only work an agent profile
//! (an `escurel-agent` page) is declared for is run: a skill whose `agent:`
//! names one. An event of a skill without one (an audit record, a receipt,
//! work another worker consumes) is left in the inbox, unclaimed and
//! unprocessed, and the runner records no run for it.
//!
//! Independent of the policy: an operator requeue of a dead-lettered run whose
//! event the gateway already records `processed` does not run it again.

use std::time::{Duration, Instant};

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

use crate::support::call;

const TENANT: &str = "acme";

/// Runs as the `scribe` agent.
const WORK: &str = "---\nkind: skill\nid: work\nautonomy: auto\n\
    agent: \"[[escurel-agent::scribe]]\"\nacl:\n  create: [ops]\n  update: [ops]\n---\n\
    # work\n\nFold the event into the instance.\n";
/// No agent: an audit record that some other consumer reads.
const AUDIT: &str = "---\nkind: skill\nid: audit\nautonomy: auto\n---\n# audit\n\n\
    A record of a decision. Not work.\n";
/// Names an agent that does not exist: its run is refused and dead-lettered.
const ORPHAN: &str = "---\nkind: skill\nid: orphan\nautonomy: auto\n\
    agent: \"[[escurel-agent::ghost]]\"\n---\n# orphan\n\nFold the event into the instance.\n";
const SCRIBE: &str = "---\nkind: instance\nskill: escurel-agent\nid: scribe\nharness: echo\n\
    skills: [\"[[skill::work]]\"]\n---\n# scribe\n";

fn instance(skill: &str) -> String {
    format!("---\nkind: instance\nid: c1\nskill: {skill}\n---\n# C1\n\nBASELINE.\n")
}

fn page(skill: &str) -> String {
    format!("markdown/instances/{skill}/c1.md")
}

async fn gateway() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("work", WORK)
                .skill("audit", AUDIT)
                .skill("orphan", ORPHAN)
                .instance("work", "c1", instance("work"))
                .instance("audit", "c1", instance("audit"))
                .instance("orphan", "c1", instance("orphan"))
                .instance("escurel-agent", "scribe", SCRIBE)
                .done(),
        ),
        ..Default::default()
    })
    .await
}

fn admin(gw: &EscurelProcess) -> String {
    gw.mint_token(TENANT, Role::Admin)
}

async fn capture(gw: &EscurelProcess, skill: &str) -> String {
    let r = call(
        gw,
        &admin(gw),
        "capture_event",
        json!({ "source": "test", "mime": "text/plain", "label_skill": skill,
                "instance_page_id": page(skill), "title": "t",
                "body": format!("ECHO_FOLD_MARKER work for {skill}") }),
    )
    .await;
    r["event_id"].as_str().unwrap().to_owned()
}

async fn lookup(gw: &EscurelProcess, event_id: &str) -> Value {
    call(
        gw,
        &admin(gw),
        "list_events",
        json!({ "event_id": event_id }),
    )
    .await["events"][0]
        .clone()
}

/// `run-started` records at the gateway for runs of `event_id`.
async fn run_starts(gw: &EscurelProcess, event_id: &str) -> usize {
    let r = call(
        gw,
        &admin(gw),
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

async fn admission(base: &str, event_id: &str) -> Option<String> {
    let resp = reqwest::get(format!("{base}/debug/admission?event_id={event_id}"))
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body: Value = resp.json().await.ok()?;
    body["decision"].as_str().map(str::to_owned)
}

async fn eventually<T, F, Fut>(within: Duration, what: &str, mut probe: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let deadline = Instant::now() + within;
    loop {
        if let Some(v) = probe().await {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

#[tokio::test]
async fn under_the_agent_policy_only_work_with_a_declared_agent_runs() {
    let gw = gateway().await;
    let mut runner = escurel_test_support::RunnerProcess::echo(&gw, TENANT)
        .binary(env!("CARGO_BIN_EXE_escurel-runner"))
        .minted()
        .env("ESCUREL_RUNNER_DISPATCH", "agent")
        .spawn();
    runner.wait_healthy().await;

    let audit = capture(&gw, "audit").await;
    let work = capture(&gw, "work").await;

    // The positive control: the agent's work runs, as the agent.
    let run = runner
        .wait_for_status(&work, &["processed"], Duration::from_secs(60))
        .await;
    assert_eq!(run["status"], "processed", "{run}");

    // The runner has looked at the audit record and left it.
    let base = runner.base_url();
    eventually(
        Duration::from_secs(20),
        "the audit record's decision",
        || async { (admission(&base, &audit).await? == "no_agent").then_some(()) },
    )
    .await;
    // Several more polls: it stays left.
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(
        runner.debug_run(&audit).await.is_none(),
        "the runner recorded a run for an event no agent is declared for"
    );
    assert_eq!(run_starts(&gw, &audit).await, 0);
    let looked = lookup(&gw, &audit).await;
    assert_eq!(looked["status"], "inbox", "{looked}");
    assert!(looked.get("claim").is_none(), "{looked}");
    let inbox = call(&gw, &admin(&gw), "list_inbox", json!({ "limit": 100 })).await;
    let ids: Vec<&str> = inbox["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["event_id"].as_str())
        .collect();
    assert!(ids.contains(&audit.as_str()), "{inbox}");
}

#[tokio::test]
async fn a_requeue_of_an_event_already_processed_runs_nothing() {
    let gw = gateway().await;
    let runner = escurel_test_support::RunnerProcess::echo(&gw, TENANT)
        .binary(env!("CARGO_BIN_EXE_escurel-runner"))
        .minted()
        .env("ESCUREL_RUNNER_MAX_ATTEMPTS", "1")
        .spawn();
    let mut runner = runner;
    runner.wait_healthy().await;

    // A run that is refused (its declared agent does not exist) dead-letters.
    let orphan = capture(&gw, "orphan").await;
    let dead = runner
        .wait_for_status(&orphan, &["dead_letter"], Duration::from_secs(60))
        .await;
    let starts = run_starts(&gw, &orphan).await;
    // Its consumer files the event meanwhile: the gateway records it processed.
    call(
        &gw,
        &admin(&gw),
        "assign_event",
        json!({ "event_id": orphan, "instance_page_id": page("orphan") }),
    )
    .await;
    assert_eq!(lookup(&gw, &orphan).await["status"], "processed");

    let resp: Value = reqwest::Client::new()
        .post(format!("{}/dlq/requeue", runner.base_url()))
        .json(&json!({ "run_id": dead["run_id"] }))
        .send()
        .await
        .expect("requeue")
        .json()
        .await
        .expect("json");
    assert_eq!(resp["requeued"], false, "{resp}");
    assert_eq!(resp["reason"], "event_processed", "{resp}");
    let row = runner
        .wait_for_status(&orphan, &["processed"], Duration::from_secs(10))
        .await;
    assert_eq!(row["status"], "processed", "{row}");
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        run_starts(&gw, &orphan).await,
        starts,
        "the requeue started another run"
    );
}
