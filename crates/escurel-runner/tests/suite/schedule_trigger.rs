//! Scheduled triggers declared in skill frontmatter (#819): a skill with
//! `schedule:` is fired by the runner itself, once per window, as a
//! `capture_event` with id `sched:<skill>:<window-start>`.
//!
//! Real gateway, real runner binary (static admin bearer), real echo harness.
//! The schedules fire every two seconds (a 6-field cron), so a few seconds of
//! wall clock cover several windows.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use chrono::{DateTime, Timelike, Utc};
use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

use crate::support::{RunnerProcess, RunningRunner};

const TENANT: &str = "acme";
const TICK_SKILL: &str = "---\nkind: skill\nid: tick\ndescription: Every two seconds.\n\
    autonomy: auto\nschedule: \"*/2 * * * * *\"\n---\n# tick\n\nNote the time.\n";
const CAPPED_SKILL: &str = "---\nkind: skill\nid: capped\ndescription: Once a day at most.\n\
    autonomy: auto\nschedule: \"*/2 * * * * *\"\nschedule_budget: {runs_per_day: 1}\n---\n\
    # capped\n\nNote the time.\n";

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

/// The runner on the echo harness with a fast schedule tick, healthy.
async fn spawn_runner(gw: &EscurelProcess) -> RunningRunner {
    let mut runner = RunnerProcess::echo(gw, TENANT)
        .env("ESCUREL_RUNNER_SCHEDULE_TICK", "250ms")
        .env("ESCUREL_RUNNER_SCHEDULE_GRACE", "2s")
        .spawn();
    runner.wait_healthy().await;
    runner
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

/// The newest 1000 events under `label`, any status.
async fn events_under(gw: &EscurelProcess, label: &str) -> Vec<Value> {
    let r = call(
        gw,
        "list_events",
        json!({ "label_skill": label, "limit": 1000, "newest_first": true }),
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

async fn await_terminal(runner: &RunningRunner, event_id: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(run) = runner.debug_run(event_id).await
            && let Some(status) = run["status"].as_str()
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
    let mut runner = spawn_runner(&gw).await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    runner.kill();
    let before = trigger_windows(&gw, "tick").await;
    assert!(
        before.len() >= 2,
        "5 s of a 2 s schedule fires at least twice: {before:?}"
    );
    assert_contiguous(&before);

    runner.restart();
    runner.wait_healthy().await;
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
    let status = await_terminal(&runner, &trigger_id("tick", before[0])).await;
    assert_eq!(status, "processed", "the scheduled run finished");
}

/// A spent budget does not fire the window; it files a visible `run-skipped`.
#[tokio::test]
async fn an_exhausted_budget_skips_the_window_visibly() {
    let gw = gateway(&[("capped", CAPPED_SKILL)]).await;
    let _runner = spawn_runner(&gw).await;
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
/// one trigger, and its work really runs — counted from the runs' own
/// `run-finished`, not only from the ids the gateway already dedups (crew
/// review PR-1, F7). No contiguity: a tick stalled on a busy CI box may miss
/// a window, which is not what this test is about.
///
/// Not asserted: that the harness runs ONCE per window. Today it does not —
/// both runners' inbox pollers pick up the one trigger and each runs it (as
/// for any inbox event; nothing claims an event across runners). Run
/// ownership/leases are PR-2 (#818), which should tighten this to `== 1`.
#[tokio::test]
async fn two_runners_on_one_schedule_do_not_double_fire() {
    let gw = gateway(&[("tick", TICK_SKILL)]).await;
    let _ra = spawn_runner(&gw).await;
    let _rb = spawn_runner(&gw).await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    let windows = trigger_windows(&gw, "tick").await;
    assert!(windows.len() >= 2, "{windows:?}");
    // Let the first windows' runs finish before counting.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let runs = events_under(&gw, "escurel:run").await;
    let ran = |trigger: &str| {
        runs.iter()
            .filter(|e| e["title"] == "run-finished")
            .any(|e| e["provenance"]["runner"]["event_id"] == trigger)
    };
    for w in &windows[..2] {
        let trigger = trigger_id("tick", *w);
        assert!(ran(&trigger), "{trigger} fired but its work never ran");
    }
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
    let _runner = spawn_runner(&gw).await;
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

/// The budget counts the day's TRIGGERS, not the label's history (crew review PR-1, F7): a skill
/// with thousands of ordinary events under its label still fires. The old count paged newest-first
/// through at most 4000 label events and read "pages ran out" as "budget spent", so any member could
/// stop a schedule by filing events under its label. Seeded in-process (processed, so the runner
/// does not dispatch them) before the gateway starts.
#[tokio::test]
async fn thousands_of_unrelated_events_under_the_label_do_not_spend_the_budget() {
    use std::sync::Arc;
    const BUSY: &str = "---\nkind: skill\nid: busy\ndescription: Busy label.\n\
        autonomy: auto\nschedule: \"*/2 * * * * *\"\nschedule_budget: {runs_per_day: 3}\n---\n\
        # busy\n\nNote the time.\n";
    let store_dir = tempfile::TempDir::new().unwrap();
    let db_dir = tempfile::TempDir::new().unwrap();
    let store: Arc<dyn escurel_storage::LaneStore> = Arc::new(escurel_storage::FsStore::new(
        store_dir.path().to_path_buf(),
    ));
    let embedder: Arc<dyn escurel_embed::Embedder> =
        Arc::new(escurel_embed::ZeroEmbedder::default());
    let conn = duckdb::Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    escurel_index::Migrator::up(&conn).unwrap();
    let indexer = Arc::new(escurel_index::Indexer::new(store, embedder, conn, TENANT).unwrap());
    indexer
        .update_page("markdown/skills/busy.md", BUSY)
        .await
        .unwrap();
    for n in 0..4100 {
        let e = indexer
            .capture_event(escurel_index::NewEvent {
                event_id: Some(format!("noise-{n}")),
                source: "member".to_owned(),
                mime: "text/plain".to_owned(),
                label_skill: "busy".to_owned(),
                title: "noise".to_owned(),
                body: "unrelated".to_owned(),
                ..Default::default()
            })
            .await
            .unwrap();
        indexer
            .assign_event(&e.event_id, "markdown/instances/busy/n.md")
            .await
            .unwrap();
    }
    let gw = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: escurel_test_support::ConfigOverrides {
            indexer: Some(Arc::clone(&indexer)),
            ..Default::default()
        },
        ..Default::default()
    })
    .await;
    let _runner = spawn_runner(&gw).await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    let fired = trigger_windows(&gw, "busy").await;
    assert!(
        fired.len() >= 2,
        "the budget (3/day) is not spent by 4100 unrelated events: {fired:?}"
    );
    assert!(fired.len() <= 3, "and it still holds: {fired:?}");
}

/// A malformed `schedule_budget:` fails CLOSED (crew review PR-1, F8): nothing fires, and every
/// window files `run-skipped { reason: "invalid_budget" }`. `runs_per_day: 0` used to read as "no
/// cap". Written through `update_page`: validate flags it, but validate is not a write gate.
#[tokio::test]
async fn a_malformed_budget_fires_nothing_and_says_why() {
    let gw = gateway(&[]).await;
    let page = "---\nkind: skill\nid: broke\ndescription: Bad budget.\nautonomy: auto\n\
        schedule: \"*/2 * * * * *\"\nschedule_budget: {runs_per_day: 0}\n---\n# broke\n";
    call(
        &gw,
        "update_page",
        json!({ "page_id": "markdown/skills/broke.md", "content": page }),
    )
    .await;
    let listed = call(&gw, "list_skills", json!({})).await;
    let broke = listed["skills"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "broke")
        .cloned()
        .unwrap_or_else(|| panic!("{listed}"));
    assert_eq!(
        broke["schedule_budget"],
        json!({ "runs_per_day": 0, "invalid": true }),
        "{broke}"
    );

    let _runner = spawn_runner(&gw).await;
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert!(
        trigger_windows(&gw, "broke").await.is_empty(),
        "a malformed budget must not fire"
    );
    let skipped: Vec<Value> = events_under(&gw, "escurel:run")
        .await
        .into_iter()
        .filter(|e| {
            e["event_id"]
                .as_str()
                .is_some_and(|id| id.starts_with("sched:broke:") && id.ends_with(":skipped"))
        })
        .collect();
    assert!(!skipped.is_empty(), "the refusal is visible");
    let body: Value = serde_json::from_str(skipped[0]["body"].as_str().unwrap()).unwrap();
    assert_eq!(body["reason"], "invalid_budget", "{body}");
}

/// An unreadable `schedule:` is not silence (crew review PR-1, F3): `list_skills` flags it
/// `schedule_invalid`, nothing fires, and the runner files ONE `run-skipped { reason:
/// "invalid_schedule" }` per (skill, expression) — not one per tick, and not one per runner.
#[tokio::test]
async fn an_unreadable_schedule_fires_nothing_and_says_why_once() {
    let gw = gateway(&[]).await;
    let page = "---\nkind: skill\nid: typo\ndescription: Bad cron.\nautonomy: auto\n\
        schedule: \"15 7 * * Mo-Fr\"\n---\n# typo\n";
    call(
        &gw,
        "update_page",
        json!({ "page_id": "markdown/skills/typo.md", "content": page }),
    )
    .await;
    let listed = call(&gw, "list_skills", json!({})).await;
    let typo = listed["skills"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "typo")
        .cloned()
        .unwrap_or_else(|| panic!("{listed}"));
    assert_eq!(typo["schedule"], "15 7 * * Mo-Fr", "{typo}");
    assert_eq!(typo["schedule_invalid"], true, "{typo}");

    let _ra = spawn_runner(&gw).await;
    let _rb = spawn_runner(&gw).await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(trigger_windows(&gw, "typo").await.is_empty());
    let skipped: Vec<Value> = events_under(&gw, "escurel:run")
        .await
        .into_iter()
        .filter(|e| e["title"] == "run-skipped")
        .collect();
    assert_eq!(
        skipped.len(),
        1,
        "once per (skill, expression): {skipped:?}"
    );
    let body: Value = serde_json::from_str(skipped[0]["body"].as_str().unwrap()).unwrap();
    assert_eq!(body["reason"], "invalid_schedule", "{body}");
    assert_eq!(body["skill"], "typo", "{body}");
    assert_eq!(body["schedule"], "15 7 * * Mo-Fr", "{body}");
    assert!(
        skipped[0]["event_id"]
            .as_str()
            .unwrap()
            .starts_with("sched:typo:invalid:"),
        "{skipped:?}"
    );
}

/// A zoned schedule's trigger id is the UTC instant of its window (crew review PR-1, F6), even in a
/// half-hour-offset zone (Asia/Kolkata, UTC+05:30): never local wall-clock time, never an offset.
#[tokio::test]
async fn a_zoned_schedule_names_its_window_in_utc() {
    const ZONED: &str = "---\nkind: skill\nid: zoned\ndescription: Kolkata.\nautonomy: auto\n\
        schedule: \"CRON_TZ=Asia/Kolkata */2 * * * * *\"\n---\n# zoned\n\nNote the time.\n";
    let gw = gateway(&[("zoned", ZONED)]).await;
    let _runner = spawn_runner(&gw).await;
    let started = Utc::now();
    tokio::time::sleep(Duration::from_secs(4)).await;
    let ids: Vec<String> = events_under(&gw, "zoned")
        .await
        .iter()
        .filter_map(|e| e["event_id"].as_str().map(str::to_owned))
        .filter(|id| id.starts_with("sched:zoned:"))
        .collect();
    assert!(!ids.is_empty(), "the zoned schedule fired");
    for id in &ids {
        assert!(id.ends_with('Z'), "a UTC instant, not an offset: {id}");
    }
    let windows = trigger_windows(&gw, "zoned").await;
    for w in &windows {
        let drift = (*w - started).num_seconds().abs();
        assert!(drift <= 10, "the window is now in UTC, not 5h30 away: {w}");
    }
}
