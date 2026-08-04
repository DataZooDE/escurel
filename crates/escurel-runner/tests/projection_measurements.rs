//! The projection-loop measurements behind §6.6 of `docs/paper`.
//!
//! Two experiments, both against a real gateway and the real `escurel-runner`
//! binary driving the real echo-harness subprocess. The echo harness stands in
//! for a language model so that the numbers describe **the loop** rather than
//! an LLM's latency; its escurel effects are ordinary `/mcp` writes.
//!
//! 1. `cascade_throughput_to_quiescence` — a seeded stream of events, measured
//!    from first capture to quiescence: wall-clock, events/second, runs per
//!    event, and how many runs each loop control stopped.
//!
//! 2. `replay_convergence_under_kill` — the runner is `SIGKILL`ed at a
//!    pseudo-random point in a live cascade, restarted against the *same*
//!    durable ledger, and the fold must land **exactly once**.
//!
//!    On what "converged" means. The paper originally promised a final state
//!    byte-identical to an uninterrupted run. That comparison is not available
//!    across trials: the echo harness keys its appended note on the event id,
//!    and event ids are ULIDs, so two trials differ in their bytes for a
//!    reason that has nothing to do with replay. The invariant that actually
//!    matters is effectively-once — every captured event appears in the
//!    instance body exactly once and ends `processed` — so that is what is
//!    measured, and §6.6 says so rather than claiming a comparison we did not
//!    run.
//!
//! Emits JSON to `$ESCUREL_PAPER_DATA` (default: stdout).
//!
//! Opt-in (slow — one gateway + runner per replay trial):
//! `cargo test -p escurel-runner --features paper-measurements --test projection_measurements -- --nocapture`
#![cfg(feature = "paper-measurements")]

use std::net::TcpListener;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};
use tempfile::TempDir;

const TENANT: &str = "acme";

const MEETING_SKILL: &str = "meeting";
const MEETING_BODY: &str = "---\ntype: skill\nid: meeting\n---\n# meeting\n\n\
     Fold the meeting note into the decision-record instance it concerns.\n";
const DECISION_SKILL: &str = "decision-record";
const DECISION_BODY: &str = "---\ntype: skill\nid: decision-record\n---\n# decision-record\n\n\
     Maintain the running decision record.\n";
const DECISION_ID: &str = "q3-roadmap";

/// Events in the throughput stream.
const STREAM_EVENTS: usize = 20;
/// Kill/restart trials. Override with `ESCUREL_PAPER_TRIALS`.
const DEFAULT_TRIALS: usize = 50;

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind")
        .local_addr()
        .expect("addr")
        .port()
}

/// A deterministic pseudo-random sequence, so a surprising trial can be
/// reproduced exactly. `Instant`-free by construction.
fn lcg(seed: &mut u64) -> u64 {
    *seed = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    *seed >> 33
}

async fn call_mcp(p: &EscurelProcess, name: &str, args: Value) -> Value {
    let token = p.mint_token(TENANT, Role::Agent);
    let body: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": args },
        }))
        .send()
        .await
        .expect("post /mcp")
        .json()
        .await
        .expect("json");
    assert!(body.get("error").is_none(), "tool {name}: {body}");
    let r = body["result"].clone();
    r.get("structuredContent").cloned().unwrap_or(r)
}

async fn spawn_gateway() -> EscurelProcess {
    spawn_gateway_with(1).await
}

/// `targets == 1` points every event at one instance (the contended case);
/// `targets == n` gives each event its own, which is what discriminates
/// per-run cost from write contention on a single page.
async fn spawn_gateway_with(targets: usize) -> EscurelProcess {
    let mut fx = FixtureBuilder::new()
        .tenant(TENANT)
        .skill(MEETING_SKILL, MEETING_BODY)
        .skill(DECISION_SKILL, DECISION_BODY);
    for i in 0..targets {
        let id = target_id(i);
        let body = format!(
            "---\ntype: instance\nid: {id}\nskill: {DECISION_SKILL}\n---\n# {id}\n\n\
             BASELINE decision record.\n"
        );
        fx = fx.instance(DECISION_SKILL, id.as_str(), body.as_str());
    }
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(fx.done()),
        ..Default::default()
    })
    .await
}

fn target_id(i: usize) -> String {
    if i == 0 {
        DECISION_ID.to_owned()
    } else {
        format!("{DECISION_ID}-{i}")
    }
}

fn page_of(i: usize) -> String {
    format!("markdown/instances/{DECISION_SKILL}/{}.md", target_id(i))
}

/// Spawn the real runner against `ledger` (reused across a restart so the
/// idempotency authority survives the kill).
fn spawn_runner(gateway: &EscurelProcess, token: &str, listen: &str, ledger: &str) -> ChildGuard {
    spawn_runner_governed(gateway, token, listen, ledger, true)
}

/// `governed = true` leaves the per-tenant quota governor at its shipped
/// default (120 runs/min). `false` lifts it, so the measurement describes the
/// loop rather than the governor.
fn spawn_runner_governed(
    gateway: &EscurelProcess,
    token: &str,
    listen: &str,
    ledger: &str,
    governed: bool,
) -> ChildGuard {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    if !governed {
        cmd.env("ESCUREL_RUNNER_TENANT_RUNS_PER_MIN", "100000")
            .env("ESCUREL_RUNNER_TENANT_MAX_CONCURRENT", "32");
    }
    cmd.env("ESCUREL_RUNNER_LISTEN", listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gateway.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_TOKEN", token)
        .env("ESCUREL_RUNNER_HARNESS", "echo")
        .env("ESCUREL_RUNNER_LEDGER_PATH", ledger)
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "100ms");
    ChildGuard(cmd.spawn().expect("spawn escurel-runner"))
}

async fn capture_to(p: &EscurelProcess, n: usize, target_ix: usize) -> String {
    let target = page_of(target_ix);
    call_mcp(
        p,
        "capture_event",
        json!({
            "source": "manual", "mime": "text/plain",
            "label_skill": MEETING_SKILL,
            "instance_page_id": target,
            "title": format!("renewal {n}"),
            "body": format!("MEASURED EVENT {n}"),
        }),
    )
    .await["event_id"]
        .as_str()
        .expect("event_id")
        .to_owned()
}

async fn capture(p: &EscurelProcess, n: usize) -> String {
    capture_to(p, n, 0).await
}

async fn ledger_counts(http: &reqwest::Client, listen: &str) -> Value {
    let Ok(resp) = http
        .get(format!("http://{listen}/debug/ledger"))
        .send()
        .await
    else {
        return json!({});
    };
    if !resp.status().is_success() {
        return json!({});
    }
    resp.json::<Value>().await.unwrap_or_else(|_| json!({}))
}

/// How many times each event id appears folded into the instance body.
async fn fold_counts_over(p: &EscurelProcess, ids: &[String], targets: usize) -> Vec<usize> {
    let mut all = String::new();
    for i in 0..targets {
        let b = call_mcp(p, "expand", json!({ "page_id": page_of(i) })).await["body"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        all.push_str(&b);
    }
    ids.iter()
        .map(|id| all.matches(id.as_str()).count())
        .collect()
}

async fn fold_counts(p: &EscurelProcess, ids: &[String]) -> Vec<usize> {
    fold_counts_over(p, ids, 1).await
}

async fn processed_count_over(p: &EscurelProcess, ids: &[String], targets: usize) -> usize {
    let mut arr = Vec::new();
    for i in 0..targets {
        let events = call_mcp(p, "list_events", json!({ "instance_page_id": page_of(i) })).await;
        arr.extend(events["events"].as_array().cloned().unwrap_or_default());
    }
    ids.iter()
        .filter(|id| {
            arr.iter()
                .any(|e| e["event_id"] == json!(*id) && e["status"] == json!("processed"))
        })
        .count()
}

async fn processed_count(p: &EscurelProcess, ids: &[String]) -> usize {
    processed_count_over(p, ids, 1).await
}

fn emit(name: &str, value: Value) {
    let rendered = serde_json::to_string_pretty(&value).unwrap();
    match std::env::var("ESCUREL_PAPER_DATA") {
        Ok(dir) => {
            std::fs::create_dir_all(&dir).ok();
            std::fs::write(format!("{dir}/{name}.json"), &rendered).expect("write paper data");
        }
        Err(_) => println!("--- {name} ---\n{rendered}"),
    }
}

// --- 1. throughput to quiescence ---------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn cascade_throughput_to_quiescence() {
    let mut arms = Vec::new();
    for (governed, targets) in [(true, 1), (false, 1), (false, STREAM_EVENTS)] {
        arms.push(throughput_arm(governed, targets).await);
    }
    emit(
        "cascade",
        json!({ "events": STREAM_EVENTS, "harness": "echo", "arms": arms }),
    );
}

async fn throughput_arm(governed: bool, targets: usize) -> Value {
    let gateway = spawn_gateway_with(targets).await;
    let token = gateway.mint_token(TENANT, Role::Agent);
    let listen = format!("127.0.0.1:{}", free_port());
    let ledger_dir = TempDir::new().unwrap();
    let ledger = ledger_dir.path().join("ledger.sqlite");
    let ledger = ledger.to_str().unwrap();

    let mut ids = Vec::with_capacity(STREAM_EVENTS);
    for n in 0..STREAM_EVENTS {
        ids.push(capture_to(&gateway, n, n % targets).await);
    }

    let started = Instant::now();
    let _runner = spawn_runner_governed(&gateway, &token, &listen, ledger, governed);
    let http = reqwest::Client::new();

    // Quiescence: every seeded event is `processed` on the gateway *and* the
    // ledger has stopped moving. Reading only the ledger would call it done
    // while a write was still in flight.
    let deadline = started + Duration::from_secs(180);
    let mut quiesced_at = None;
    let mut last_total = u64::MAX;
    let mut stable_polls = 0;
    while Instant::now() < deadline {
        let done = processed_count_over(&gateway, &ids, targets).await;
        let l = ledger_counts(&http, &listen).await;
        let total = l["total"].as_u64().unwrap_or(0);
        stable_polls = if total == last_total {
            stable_polls + 1
        } else {
            0
        };
        last_total = total;
        if done == ids.len() && stable_polls >= 3 {
            quiesced_at = Some(started.elapsed());
            break;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    let elapsed = quiesced_at.expect("stream never quiesced within 180s");
    let l = ledger_counts(&http, &listen).await;
    let folds = fold_counts_over(&gateway, &ids, targets).await;

    let arm = json!({
            "governor": if governed { "default (120 runs/min)" } else { "lifted" },
            "target_instances": targets,
            "seconds_to_quiescence": (elapsed.as_secs_f64() * 100.0).round() / 100.0,
            "events_per_second":
                ((STREAM_EVENTS as f64 / elapsed.as_secs_f64()) * 100.0).round() / 100.0,
            "ledger_runs_total": l["total"],
            "runs_per_event":
                ((l["total"].as_u64().unwrap_or(0) as f64 / STREAM_EVENTS as f64) * 100.0).round()
                    / 100.0,
            "dead_lettered_by_loop_control": l["dead_letter"],
            "failed": l["failed"],
            "max_folds_of_any_single_event": folds.iter().max().copied().unwrap_or(0),
    });

    assert!(
        folds.iter().all(|&c| c == 1),
        "each event must fold exactly once, got {folds:?}"
    );
    gateway.shutdown().await;
    arm
}

// --- 2. replay convergence under kill ----------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn replay_convergence_under_kill() {
    let trials: usize = std::env::var("ESCUREL_PAPER_TRIALS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_TRIALS);
    let mut seed = 0x5EED_1234_u64;

    let http = reqwest::Client::new();
    let mut phases: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    let mut converged_in_flight = 0usize;
    let mut in_flight_total = 0usize;
    let mut converged = 0usize;
    let mut duplicated = 0usize;
    let mut unprocessed = 0usize;
    let mut divergences = Vec::new();

    for trial in 0..trials {
        let gateway = spawn_gateway().await;
        let token = gateway.mint_token(TENANT, Role::Agent);
        let listen = format!("127.0.0.1:{}", free_port());
        let ledger_dir = TempDir::new().unwrap();
        let ledger = ledger_dir.path().join("ledger.sqlite");
        let ledger = ledger.to_str().unwrap().to_owned();

        let id = capture(&gateway, trial).await;
        let ids = vec![id.clone()];

        // Kill somewhere inside the window where the run is being packaged,
        // dispatched, or written back.
        let kill_after = Duration::from_millis(20 + (lcg(&mut seed) % 80));
        // Classify what the kill actually interrupted. A trial where the run
        // had already finished, or had not started, proves nothing about
        // replay -- so the meaningful denominator is `in_flight`, and the
        // breakdown is reported rather than averaged away.
        let phase;
        {
            let _r = spawn_runner(&gateway, &token, &listen, &ledger);
            tokio::time::sleep(kill_after).await;
            let done = processed_count(&gateway, &ids).await == 1;
            let l = ledger_counts(&http, &listen).await;
            let total = l["total"].as_u64().unwrap_or(0);
            let terminal = l["terminal"].as_u64().unwrap_or(0);
            phase = if done || (total > 0 && terminal >= total) {
                "after_completion"
            } else if total > 0 {
                "in_flight"
            } else {
                "before_start"
            };
        } // ChildGuard drop = SIGKILL

        // Restart against the same durable ledger and let it converge.
        let listen2 = format!("127.0.0.1:{}", free_port());
        let _r2 = spawn_runner(&gateway, &token, &listen2, &ledger);
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut ok = false;
        while Instant::now() < deadline {
            if processed_count(&gateway, &ids).await == 1 {
                ok = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        // Settle, so a duplicate write in flight is not missed.
        tokio::time::sleep(Duration::from_millis(600)).await;
        let folds = fold_counts(&gateway, &ids).await[0];

        if !ok {
            unprocessed += 1;
            divergences.push(json!({
                "trial": trial, "kill_after_ms": kill_after.as_millis() as u64,
                "class": "never_processed", "phase": phase,
            }));
        } else if folds != 1 {
            duplicated += 1;
            divergences.push(json!({
                "trial": trial, "kill_after_ms": kill_after.as_millis() as u64,
                "class": "duplicate_fold", "folds": folds, "phase": phase,
            }));
        } else {
            converged += 1;
            if phase == "in_flight" {
                converged_in_flight += 1;
            }
        }
        *phases.entry(phase).or_default() += 1;
        if phase == "in_flight" {
            in_flight_total += 1;
        }
        gateway.shutdown().await;
    }

    emit(
        "replay",
        json!({
            "trials": trials,
            "seed": "0x5EED1234 (lcg)",
            "kill_window_ms": [20, 100],
            "converged": converged,
            "duplicate_fold": duplicated,
            "never_processed": unprocessed,
            "convergence_rate":
                ((converged as f64 / trials as f64) * 1000.0).round() / 1000.0,
            "kill_phase": phases,
            "in_flight_trials": in_flight_total,
            "in_flight_converged": converged_in_flight,
            "divergences": divergences,
            "invariant": "exactly one fold per event and terminal status processed",
        }),
    );

    // A duplicate fold is a correctness failure: the ledger's effectively-once
    // guarantee (E-8) did not hold across the kill. A never-processed trial is
    // reported but not asserted here -- it is a liveness question, and the
    // paper reports the rate rather than pretending a deadline is a proof.
    assert_eq!(duplicated, 0, "duplicate folds: {divergences:?}");
}
