//! DoD test for issue #157 — **loop controls (depth/budget/cycle)**, with
//! **no mocks**.
//!
//! Per the issue's Scope/DoD: against a real `EscurelProcess` seed real skills
//! that cascade in a **cycle A→B→A**; `capture_event` a real seed event, run
//! the real runner + real echo-harness, and assert the cascade STOPS at the
//! depth limit / cycle detection and the blocked run lands in `dead_letter`
//! with the correct reason in the REAL ledger (read back via `/debug/run`).
//!
//! ## The real A→B→A cycle fixture
//!
//! Two skills whose folds each produce an instance of the OTHER skill's type,
//! so each confirmed write is a *cross-skill* change that cascades to the
//! other skill — without controls it would oscillate forever:
//!
//! - skill **alpha**, instance `a1` (skill alpha);
//! - skill **beta**, instance `b1` (skill beta);
//! - each skill declares a frontmatter `cascade_target`: alpha → `b1`,
//!   beta → `a1`. The cascade emitter pre-flags the follow-on event onto the
//!   produced skill's `cascade_target`, so the next hop folds into a
//!   cross-skill instance and cascades again.
//!
//! Seed event E0: label `alpha`, pre-flagged to `b1` (cross-skill: an `alpha`
//! event landing on a `beta` instance). Hop 0 folds E0 into `b1` → produced
//! skill `beta` ≠ `alpha` → cascade emits E1 (label `beta`) pre-flagged to
//! `a1` (beta's `cascade_target`). Hop 1 folds E1 into `a1` → produced skill
//! `alpha` → cascade emits E2 (label `alpha`) pre-flagged to `b1` AGAIN —
//! revisiting `b1`. Without controls this never stops.
//!
//! The runner is configured with a low `ESCUREL_RUNNER_MAX_DEPTH` so the depth
//! cap is the hard backstop AND instance-revisit cycle detection fires; the
//! test asserts a `dead_letter` run with reason `cycle` or `depth_exceeded`,
//! and — crucially — that the chain TERMINATED (bounded run count), so a buggy
//! control can never hang the test unbounded.

use std::net::TcpListener;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "acme";

const ALPHA_SKILL: &str = "alpha";
const BETA_SKILL: &str = "beta";

// Each skill declares its cascade_target: the instance a confirmed cross-skill
// write should pre-flag its follow-on event onto. alpha cascades to b1, beta
// cascades to a1 — closing the A→B→A loop.
const ALPHA_SKILL_BODY: &str = "---\ntype: skill\nid: alpha\ncascade_target: markdown/instances/beta/b1.md\n---\n# alpha\n\nFold the event into the beta instance it concerns.\n";
const BETA_SKILL_BODY: &str = "---\ntype: skill\nid: beta\ncascade_target: markdown/instances/alpha/a1.md\n---\n# beta\n\nFold the event into the alpha instance it concerns.\n";

const A_INSTANCE_BODY: &str =
    "---\ntype: instance\nid: a1\nskill: alpha\n---\n# A1\n\nBASELINE alpha.\n";
const B_INSTANCE_BODY: &str =
    "---\ntype: instance\nid: b1\nskill: beta\n---\n# B1\n\nBASELINE beta.\n";

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    listener.local_addr().expect("read local_addr").port()
}

async fn call_mcp(p: &EscurelProcess, role: Role, name: &str, args: Value) -> Value {
    let token = p.mint_token(TENANT, role);
    let resp = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": name, "arguments": args },
        }))
        .send()
        .await
        .expect("post /mcp");
    assert_eq!(resp.status(), 200, "http status");
    let body: Value = resp.json().await.unwrap();
    assert!(body.get("error").is_none(), "tool {name} error: {body}");
    let result = body["result"].clone();
    result.get("structuredContent").cloned().unwrap_or(result)
}

#[tokio::test]
async fn cascade_cycle_is_dead_lettered_at_depth_or_cycle() {
    // 1. Real gateway with the A→B→A cycle fixture.
    let gateway = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill(ALPHA_SKILL, ALPHA_SKILL_BODY)
                .skill(BETA_SKILL, BETA_SKILL_BODY)
                .instance(ALPHA_SKILL, "a1", A_INSTANCE_BODY)
                .instance(BETA_SKILL, "b1", B_INSTANCE_BODY)
                .done(),
        ),
        ..Default::default()
    })
    .await;

    let b1_page = "markdown/instances/beta/b1.md".to_owned();

    // 2. Seed event E0: an alpha event pre-flagged to the beta instance b1 — a
    //    cross-skill write that cascades.
    let captured = call_mcp(
        &gateway,
        Role::Agent,
        "capture_event",
        json!({
            "source": "manual",
            "mime": "text/plain",
            "label_skill": ALPHA_SKILL,
            "instance_page_id": b1_page,
            "title": "seed",
            "body": "kick off the cycle"
        }),
    )
    .await;
    let seed_event_id = captured["event_id"].as_str().unwrap().to_owned();

    // 3. Spawn the real runner with a LOW depth cap so the cap is the hard
    //    backstop and the test terminates fast.
    let token = gateway.mint_token(TENANT, Role::Agent);
    let port = free_port();
    let listen = format!("127.0.0.1:{port}");
    let ledger_dir = tempfile::tempdir().expect("tempdir for ledger");
    let ledger_path = ledger_dir.path().join("ledger.sqlite");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gateway.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_TOKEN", &token)
        .env("ESCUREL_RUNNER_HARNESS", "echo")
        .env("ESCUREL_RUNNER_LEDGER_PATH", ledger_path.to_str().unwrap())
        .env("ESCUREL_RUNNER_MAX_DEPTH", "3")
        .env("ESCUREL_RUNNER_MAX_RUNS_PER_ROOT", "16")
        .env("ESCUREL_RUNNER_MAX_ATTEMPTS", "2")
        .env("ESCUREL_RUNNER_RETRY_BACKOFF", "100ms")
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "200ms");
    let _runner = ChildGuard(cmd.spawn().expect("spawn escurel-runner"));

    let http = reqwest::Client::new();
    let ledger_url = format!("http://{listen}/debug/ledger");

    // 4. Poll the REAL ledger until a dead_letter run appears (cycle or depth),
    //    bounded by a hard deadline. Also assert the chain stays BOUNDED — a
    //    runaway cascade (a broken control) would blow past the run ceiling.
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Ok(resp) = http.get(&ledger_url).send().await
            && resp.status().is_success()
        {
            let body: Value = resp.json().await.unwrap_or(json!({}));
            let dead_lettered = body["dead_letter"].as_u64().unwrap_or(0);
            let total = body["total"].as_u64().unwrap_or(0);
            assert!(
                total < 64,
                "cascade ran away ({total} runs) — a loop control failed to fire: {body}"
            );
            if dead_lettered >= 1 {
                break;
            }
        }
        if Instant::now() >= deadline {
            panic!("no run was dead-lettered within the deadline (loop control never fired)");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // 5. Find the dead-lettered run and assert its reason is one of the loop
    //    controls (cycle / depth_exceeded / budget_exceeded). Read it back from
    //    the REAL ledger via /debug/run by walking the inbox events.
    let inbox = call_mcp(&gateway, Role::Agent, "list_inbox", json!({ "limit": 200 })).await;
    let mut found_reason: Option<String> = None;
    if let Some(events) = inbox["events"].as_array() {
        for e in events {
            let Some(eid) = e["event_id"].as_str() else {
                continue;
            };
            let run_url = format!("http://{listen}/debug/run?tenant={TENANT}&event_id={eid}");
            if let Ok(resp) = http.get(&run_url).send().await
                && resp.status().is_success()
            {
                let body: Value = resp.json().await.unwrap_or(json!({}));
                if body["status"].as_str() == Some("dead_letter") {
                    found_reason = body["reason"].as_str().map(str::to_owned);
                    break;
                }
            }
        }
    }
    let reason = found_reason.expect("a dead_letter run must expose its reason via /debug/run");
    assert!(
        matches!(
            reason.as_str(),
            "cycle" | "depth_exceeded" | "budget_exceeded"
        ),
        "dead-letter reason must be a loop control, got {reason:?}"
    );

    // 6. The seed event genuinely processed (the cycle started from real work).
    let events = call_mcp(
        &gateway,
        Role::Agent,
        "list_events",
        json!({ "instance_page_id": "markdown/instances/beta/b1.md" }),
    )
    .await;
    let processed = events["events"]
        .as_array()
        .map(|es| {
            es.iter()
                .any(|e| e["event_id"] == json!(seed_event_id) && e["status"] == json!("processed"))
        })
        .unwrap_or(false);
    assert!(processed, "the seed event must be processed: {events}");
}
