//! End-to-end test for the `escurel session` commands (live CRDT
//! co-editing surface: `open` / `apply` / `close`).
//!
//! Real running gateway with a real `DuckdbCrdtBackend` wired (via the
//! `crdt_testkit` harness helper), driving the compiled `escurel`
//! binary. The op blob is a real Loro update minted by the same helper —
//! no mocks at the `LiveDoc`/backend boundary. Mirrors the tool-level
//! `escurel-server/tests/mcp_session_tools.rs`, one layer up through the
//! CLI.

use assert_cmd::Command;
use escurel_test_support::crdt_testkit::{duckdb_crdt_backend, loro_insert_op};
use escurel_test_support::{ConfigOverrides, EscurelProcess, Opts};
use serde_json::Value;

struct Harness {
    process: EscurelProcess,
    http_addr: String,
}

async fn start() -> Harness {
    // Auth disabled + indexer disabled: the session tools route before
    // the indexer gate, and the CRDT backend carries its own in-memory
    // DuckDB, so we need neither here.
    let process = EscurelProcess::spawn(Opts {
        auth: escurel_test_support::AuthMode::Disabled,
        fixtures: None,
        config_overrides: ConfigOverrides {
            crdt_backend: Some(duckdb_crdt_backend()),
            disable_indexer: true,
            ..Default::default()
        },
    })
    .await;
    let http_addr = process
        .base_url()
        .strip_prefix("http://")
        .unwrap()
        .to_owned();
    Harness { process, http_addr }
}

fn run(h: &Harness, args: &[&str]) -> std::process::Output {
    let mut cmd = Command::cargo_bin("escurel").unwrap();
    cmd.arg("--server")
        .arg(format!("http://{}", h.http_addr))
        .args(args);
    cmd.output().unwrap()
}

fn run_stdin(h: &Harness, args: &[&str], stdin: &str) -> std::process::Output {
    let mut cmd = Command::cargo_bin("escurel").unwrap();
    cmd.arg("--server")
        .arg(format!("http://{}", h.http_addr))
        .args(args)
        .write_stdin(stdin.to_owned());
    cmd.output().unwrap()
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
async fn session_open_apply_close_round_trips() {
    let h = start().await;

    // open → sess_… id + head v0 + advisory ws_url.
    let opened = json(&run(&h, &["session", "open", "page-cli"]));
    let session = opened["session"].as_str().expect("session id").to_owned();
    assert!(session.starts_with("sess_"), "session id: {opened}");
    assert_eq!(opened["head_version"], "v0");
    assert!(
        opened["ws_url"].as_str().unwrap_or("").contains("/ws"),
        "ws_url should advertise /ws: {opened}"
    );

    // apply a real Loro op (op flag path) → the merged version advances
    // off v0, proving the op reached the real LiveDoc actor.
    let op = loro_insert_op("hello");
    let applied = json(&run(&h, &["session", "apply", &session, "--op", &op]));
    assert_eq!(applied["ok"], true, "apply should succeed: {applied}");
    assert_ne!(
        applied["merged_version"], "v0",
        "applying an op must advance merged_version: {applied}"
    );

    // close with commit (the default) → terminal final_version reported.
    let closed = json(&run(&h, &["session", "close", &session]));
    assert_eq!(closed["ok"], true, "close should succeed: {closed}");
    assert_ne!(
        closed["final_version"], "v0",
        "committed close reports the advanced final_version: {closed}"
    );

    h.process.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_apply_reads_op_from_stdin() {
    let h = start().await;
    let opened = json(&run(&h, &["session", "open", "page-stdin"]));
    let session = opened["session"].as_str().unwrap().to_owned();

    let op = loro_insert_op("piped");
    let applied = json(&run_stdin(&h, &["session", "apply", &session], &op));
    assert_eq!(applied["ok"], true, "stdin op must apply: {applied}");
    assert_ne!(
        applied["merged_version"], "v0",
        "stdin op must apply: {applied}"
    );

    // close --no-commit → the command is accepted (the snapshot-skip
    // semantics are asserted at the tool level in
    // escurel-server/tests/mcp_session_tools.rs).
    let closed = json(&run(&h, &["session", "close", &session, "--no-commit"]));
    assert_eq!(
        closed["ok"], true,
        "--no-commit close must succeed: {closed}"
    );

    h.process.shutdown().await;
}
