//! End-to-end test for `escurel workflow run|status|stop`.
//!
//! Real running gateway (real DuckDB indexer + FsStore) seeded with a
//! `kind: workflow` plan, driving the compiled `escurel` binary. No runner
//! and no mocks: this exercises the operator surface (create the run board +
//! capture the invocation, render per-phase status, mark stopped) against the
//! real `/mcp` tools the CLI composes.

use assert_cmd::Command;
use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::Value;

const TENANT: &str = "acme";

const WF_SKILL_BODY: &str = "---\n\
type: skill\n\
id: deep-research\n\
description: Two-phase workflow test plan.\n\
backend: {kind: workflow}\n\
run_skill: workflow-run\n\
phases: [{id: scope, produces: research-angle, fan_out: 1}, {id: synthesize, produces: research-report, fan_out: 1}]\n\
---\n\
# deep-research\n\nFan out, then synthesize.\n";

const ANGLE_SKILL_BODY: &str = "---\ntype: skill\nid: research-angle\n---\n# research-angle\n";
const REPORT_SKILL_BODY: &str = "---\ntype: skill\nid: research-report\n---\n# research-report\n";
const RUN_SKILL_BODY: &str = "---\ntype: skill\nid: workflow-run\n---\n# workflow-run\n";

struct Harness {
    process: EscurelProcess,
    addr: String,
    bearer: String,
}

async fn start() -> Harness {
    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("deep-research", WF_SKILL_BODY)
                .skill("research-angle", ANGLE_SKILL_BODY)
                .skill("research-report", REPORT_SKILL_BODY)
                .skill("workflow-run", RUN_SKILL_BODY)
                .done(),
        ),
        config_overrides: Default::default(),
    })
    .await;
    let addr = process
        .base_url()
        .strip_prefix("http://")
        .unwrap()
        .to_owned();
    let bearer = process.mint_token(TENANT, Role::Agent);
    Harness {
        process,
        addr,
        bearer,
    }
}

async fn run(h: &Harness, args: Vec<String>) -> std::process::Output {
    let addr = h.addr.clone();
    let bearer = h.bearer.clone();
    tokio::task::spawn_blocking(move || {
        Command::cargo_bin("escurel")
            .unwrap()
            .env("ESCUREL_SERVER", format!("http://{addr}"))
            .env("ESCUREL_TOKEN", bearer)
            .args(&args)
            .output()
            .unwrap()
    })
    .await
    .unwrap()
}

fn json(out: &std::process::Output) -> Value {
    assert!(
        out.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("stdout is JSON")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workflow_run_status_stop_round_trips() {
    let h = start().await;

    // run: creates the board (recording wf_skill) + captures the invocation.
    let invoked = json(
        &run(
            &h,
            vec![
                "--format".into(),
                "json".into(),
                "workflow".into(),
                "run".into(),
                "deep-research".into(),
                "--run".into(),
                "cli1".into(),
                "--params".into(),
                "{\"q\":\"why is the sky blue\"}".into(),
            ],
        )
        .await,
    );
    assert_eq!(invoked["run"], "cli1");
    assert_eq!(invoked["wf_skill"], "deep-research");
    assert!(
        invoked["event_id"].as_str().is_some_and(|s| !s.is_empty()),
        "invocation returns an event id: {invoked}"
    );

    // status: the plan's phases render with zero produced (no runner ran).
    let status = json(
        &run(
            &h,
            vec![
                "--format".into(),
                "json".into(),
                "workflow".into(),
                "status".into(),
                "cli1".into(),
            ],
        )
        .await,
    );
    assert_eq!(status["wf_skill"], "deep-research");
    assert_eq!(status["status"], "running");
    let phases = status["phases"].as_array().expect("phases array");
    assert_eq!(phases.len(), 2);
    assert_eq!(phases[0]["phase"], "scope");
    assert_eq!(phases[0]["produces"], "research-angle");
    assert_eq!(phases[0]["produced"], 0);
    assert_eq!(phases[1]["phase"], "synthesize");

    // stop: marks the board stopped; a re-read status reflects it.
    let stopped = json(
        &run(
            &h,
            vec![
                "--format".into(),
                "json".into(),
                "workflow".into(),
                "stop".into(),
                "cli1".into(),
            ],
        )
        .await,
    );
    assert_eq!(stopped["status"], "stopped");

    let after = json(
        &run(
            &h,
            vec![
                "--format".into(),
                "json".into(),
                "workflow".into(),
                "status".into(),
                "cli1".into(),
            ],
        )
        .await,
    );
    assert_eq!(after["status"], "stopped", "stop persisted to the board");

    h.process.shutdown().await;
}
