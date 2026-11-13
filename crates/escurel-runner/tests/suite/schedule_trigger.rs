//! Scheduled triggers declared in skill frontmatter (#819): a skill with
//! `schedule:` is fired by the runner itself, once per window, as a
//! `capture_event` with id `sched:<skill>:<window-start>`.
//!
//! Real gateway, real runner binary (static admin bearer), real echo harness.
//! The schedules fire every two seconds (a 6-field cron), so a few seconds of
//! wall clock cover several windows.

use std::collections::HashSet;
use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use chrono::{DateTime, Timelike, Utc};
use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role, free_port};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const TICK_SKILL: &str = "---\nkind: skill\nid: tick\ndescription: Every two seconds.\n\
    autonomy: auto\nschedule: \"*/2 * * * * *\"\n---\n# tick\n\nNote the time.\n";
const CAPPED_SKILL: &str = "---\nkind: skill\nid: capped\ndescription: Once a day at most.\n\
    autonomy: auto\nschedule: \"*/2 * * * * *\"\nschedule_budget: {runs_per_day: 1}\n---\n\
    # capped\n\nNote the time.\n";

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn gateway(skills: &[(&str, &str)]) -> EscurelProcess {
    let mut fixtures = FixtureBuilder::new().tenant(TENANT);
    for (id, body) in skills {
        fixtures = fixtures.skill(id, *body);
    }
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(fixtures.done()),
        ..Default::default()
    })
    .await
}

/// The runner on the echo harness with a fast schedule tick, its ledger in
/// `ledger_dir` (shared across a restart). Returns once `/healthz` answers.
///
/// The one place this file spawns a runner, so a shared harness can replace it.
async fn spawn_runner(gw: &EscurelProcess, ledger_dir: &Path) -> (ChildGuard, String) {
    let listen = format!("127.0.0.1:{}", free_port());
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gw.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_TOKEN", gw.mint_token(TENANT, Role::Admin))
        .env("ESCUREL_RUNNER_HARNESS", "echo")
        .env(
            "ESCUREL_RUNNER_LEDGER_PATH",
            ledger_dir.join("ledger.duckdb"),
        )
        .env("ESCUREL_RUNNER_RETRY_BACKOFF", "50ms")
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms")
        .env("ESCUREL_RUNNER_SCHEDULE_TICK", "250ms")
        .env("ESCUREL_RUNNER_SCHEDULE_GRACE", "2s");
    let mut runner = ChildGuard(cmd.spawn().expect("spawn escurel-runner"));
    let url = format!("http://{listen}/healthz");
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(status) = runner.0.try_wait().expect("try_wait") {
            panic!("runner exited before it became healthy: {status:?}");
        }
        if let Ok(resp) = reqwest::get(&url).await
            && resp.status().is_success()
        {
            return (runner, listen);
        }
        assert!(Instant::now() < deadline, "runner never became healthy");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn call(gw: &EscurelProcess, name: &str, args: Value) -> Value {
    let body: Value = reqwest::Client::new()
        .post(gw.mcp_url())
        .header(
            "authorization",
            format!("Bearer {}", gw.mint_token(TENANT, Role::Admin)),
        )
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": { "name": name, "arguments": args } }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json");
    assert!(body.get("error").is_none(), "{name}: {body}");
    body["result"]["structuredContent"].clone()
}

/// Every event under `label`, any status.
async fn events_under(gw: &EscurelProcess, label: &str) -> Vec<Value> {
    let r = call(
        gw,
        "list_events",
        json!({ "label_skill": label, "limit": 1000 }),
    )
    .await;
    r["events"].as_array().cloned().unwrap_or_default()
}

/// The window starts of `skill`'s scheduled triggers, in id order. Panics on
/// a duplicate id (the gateway's key would make one impossible, so a
/// duplicate here means two ids for one window — the double-fire).
async fn trigger_windows(gw: &EscurelProcess, skill: &str) -> Vec<DateTime<Utc>> {
    let prefix = format!("sched:{skill}:");
    let ids: Vec<String> = events_under(gw, skill)
        .await
        .iter()
        .filter_map(|e| e["event_id"].as_str())
        .filter(|id| id.starts_with(&prefix))
        .map(str::to_owned)
        .collect();
    let unique: HashSet<&String> = ids.iter().collect();
    assert_eq!(unique.len(), ids.len(), "duplicate trigger ids: {ids:?}");
    let mut windows: Vec<DateTime<Utc>> = ids
        .iter()
        .map(|id| {
            DateTime::parse_from_rfc3339(&id[prefix.len()..])
                .unwrap_or_else(|e| panic!("`{id}` does not end in an RFC 3339 window: {e}"))
                .with_timezone(&Utc)
        })
        .collect();
    windows.sort();
    windows
}

fn assert_contiguous(windows: &[DateTime<Utc>]) {
    for pair in windows.windows(2) {
        assert_eq!(
            (pair[1] - pair[0]).num_seconds(),
            2,
            "windows must be consecutive 2 s windows: {windows:?}"
        );
    }
}

async fn await_terminal(listen: &str, event_id: &str) -> String {
    let url = format!("http://{listen}/debug/run?tenant={TENANT}&event_id={event_id}");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(resp) = reqwest::get(&url).await
            && let Ok(v) = resp.json::<Value>().await
            && let Some(status) = v["status"].as_str()
            && status != "pending"
        {
            return status.to_owned();
        }
        assert!(
            Instant::now() < deadline,
            "the run for {event_id} never reached a terminal"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn trigger_id(skill: &str, window: DateTime<Utc>) -> String {
    format!(
        "sched:{skill}:{}",
        window.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    )
}

/// One trigger per window across a kill and a restart on the same ledger:
/// nothing fires twice, and the run behind a trigger really happened.
#[tokio::test]
async fn a_schedule_fires_once_per_window_across_a_runner_restart() {
    let gw = gateway(&[("tick", TICK_SKILL)]).await;
    let ledger = tempfile::tempdir().expect("ledger dir");

    let (first, _) = spawn_runner(&gw, ledger.path()).await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    drop(first);
    let before = trigger_windows(&gw, "tick").await;
    assert!(
        before.len() >= 2,
        "5 s of a 2 s schedule fires at least twice: {before:?}"
    );
    assert_contiguous(&before);

    let (_second, listen) = spawn_runner(&gw, ledger.path()).await;
    let restarted = Utc::now();
    tokio::time::sleep(Duration::from_secs(4)).await;
    let all = trigger_windows(&gw, "tick").await;
    let after: Vec<DateTime<Utc>> = all
        .iter()
        .copied()
        .filter(|w| *w > before[before.len() - 1])
        .collect();
    assert!(
        after.len() >= 2,
        "the restarted runner keeps firing: {all:?}"
    );
    assert_contiguous(&after);
    assert!(
        after
            .iter()
            .any(|w| *w >= restarted - chrono::Duration::seconds(2)),
        "a window after the restart fired: {after:?}"
    );

    // The trigger is real work: the first window's run finished, and the
    // restarted runner reads it from the shared ledger.
    let status = await_terminal(&listen, &trigger_id("tick", before[0])).await;
    assert_eq!(status, "processed", "the scheduled run finished");
}

/// A spent budget does not fire the window; it files a visible `run-skipped`.
#[tokio::test]
async fn an_exhausted_budget_skips_the_window_visibly() {
    let gw = gateway(&[("capped", CAPPED_SKILL)]).await;
    let ledger = tempfile::tempdir().expect("ledger dir");
    let (_runner, _) = spawn_runner(&gw, ledger.path()).await;
    tokio::time::sleep(Duration::from_secs(5)).await;

    let fired = trigger_windows(&gw, "capped").await;
    assert_eq!(fired.len(), 1, "runs_per_day: 1 fires once: {fired:?}");

    let skipped: Vec<Value> = events_under(&gw, "escurel:run")
        .await
        .into_iter()
        .filter(|e| {
            e["event_id"]
                .as_str()
                .is_some_and(|id| id.starts_with("sched:capped:") && id.ends_with(":skipped"))
        })
        .collect();
    assert!(
        !skipped.is_empty(),
        "a window past the budget is reported, not silently dropped"
    );
    for e in &skipped {
        assert_eq!(e["title"], "run-skipped", "{e}");
        assert_eq!(e["kind"], "system", "{e}");
        let body: Value = serde_json::from_str(e["body"].as_str().unwrap_or("{}")).expect("body");
        assert_eq!(body["reason"], "budget", "{e}");
        assert_eq!(body["runs_per_day"], 1, "{e}");
    }
    let skipped_windows: HashSet<&str> = skipped
        .iter()
        .filter_map(|e| e["event_id"].as_str())
        .collect();
    assert!(
        !skipped_windows.contains(format!("{}:skipped", trigger_id("capped", fired[0])).as_str()),
        "the window that fired is not also skipped"
    );
}

/// Two runners on one gateway with the same schedule: each window still has
/// one trigger.
#[tokio::test]
async fn two_runners_on_one_schedule_do_not_double_fire() {
    let gw = gateway(&[("tick", TICK_SKILL)]).await;
    let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let _ra = spawn_runner(&gw, a.path()).await;
    let _rb = spawn_runner(&gw, b.path()).await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    let windows = trigger_windows(&gw, "tick").await;
    assert!(windows.len() >= 2, "{windows:?}");
    assert_contiguous(&windows);
}

/// A window older than the catch-up grace does not fire: a daily job whose
/// time passed an hour ago waits for tomorrow instead of running late. The
/// two-second skill beside it proves the loop was running all along.
#[tokio::test]
async fn a_window_older_than_the_grace_does_not_fire() {
    let hour_ago = Utc::now() - chrono::Duration::hours(1);
    let stale = format!(
        "---\nkind: skill\nid: stale\ndescription: Once a day, an hour ago.\n\
         autonomy: auto\nschedule: \"{} {} {} * * *\"\n---\n# stale\n",
        hour_ago.second(),
        hour_ago.minute(),
        hour_ago.hour()
    );
    let gw = gateway(&[("tick", TICK_SKILL), ("stale", stale.as_str())]).await;
    let ledger = tempfile::tempdir().expect("ledger dir");
    let (_runner, _) = spawn_runner(&gw, ledger.path()).await;
    tokio::time::sleep(Duration::from_secs(3)).await;

    assert!(
        !trigger_windows(&gw, "tick").await.is_empty(),
        "the loop runs: the in-grace schedule fired"
    );
    assert_eq!(
        trigger_windows(&gw, "stale").await,
        Vec::<DateTime<Utc>>::new(),
        "a window an hour past is outside the 2 s grace and must not fire"
    );
}
