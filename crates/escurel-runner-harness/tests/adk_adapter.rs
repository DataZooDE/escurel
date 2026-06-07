//! Always-on deterministic adapter test for the Google ADK adapter (#154).
//!
//! This is a **real subprocess** test, not a mock: the adapter spawns a real
//! stub executable (a shell script this test writes + `chmod +x`es) that
//! mimics the adk-rust runner I/O contract. The stub records the exact
//! [`AdkTask`] JSON it received on stdin and the value of the
//! `ESCUREL_MCP_BEARER` env var the adapter set, then prints a canned
//! [`HarnessOutcome`] JSON on stdout. The test then asserts the adapter:
//!
//!   (a) delivered the token-less task on stdin (instructions framing, input,
//!       `/mcp` endpoint, allowed tools) and the scoped bearer out-of-band via
//!       the `ESCUREL_MCP_BEARER` env var (never on stdin); and
//!   (b) parsed the stub's real stdout into a `HarnessOutcome` with `ok:true`.
//!
//! The full real-`/mcp`-fold DoD lives in
//! `crates/escurel-runner/tests/adk_end_to_end.rs` (it needs a real gateway);
//! the live adk-rust `LlmAgent` path lives in
//! `crates/escurel-runner/tests/adk_live.rs`, `#[ignore]`'d. This adapter test
//! pins the invocation/parse contract without a gateway.

use std::time::Duration;

use escurel_runner_core::{SecretString, TaskContext};
use escurel_runner_harness::{AdkHarness, Harness, HarnessStatus};

const TOKEN: &str = "scoped-bearer-DETERMINISTIC";
const MCP_ENDPOINT: &str = "http://127.0.0.1:8080/mcp";

/// Write an executable stub that stands in for the adk-rust runner. It:
///   - reads the `AdkTask` JSON on stdin and copies it to `$task_out`,
///   - records the value of `ESCUREL_MCP_BEARER` to `$token_out`,
///   - prints a canned `HarnessOutcome` JSON on stdout, exit 0.
///
/// Returns the path to the stub binary.
fn write_adk_stub(dir: &std::path::Path, task_out: &str, token_out: &str) -> std::path::PathBuf {
    let stub = dir.join("adk-stub.sh");
    let script = format!(
        r#"#!/usr/bin/env bash
set -euo pipefail
# Capture the AdkTask JSON the adapter wrote to our stdin.
cat > "{task_out}"
# Record the scoped bearer the adapter handed us out-of-band.
printf '%s' "${{ESCUREL_MCP_BEARER:-}}" > "{token_out}"
# Emit a canned HarnessOutcome on stdout.
printf '%s\n' '{{"ok":true,"status":"ok","summary":"adk runner folded the renewal event","tool_calls":3,"produced_instance":null}}'
"#
    );
    std::fs::write(&stub, script).expect("write stub script");
    let mut perms = std::fs::metadata(&stub).expect("stat stub").permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&stub, perms).expect("chmod stub");
    stub
}

#[tokio::test]
async fn adk_adapter_delivers_task_and_parses_real_stub_output() {
    let dir = tempfile::tempdir().expect("tempdir");
    let task_out = dir.path().join("task.json");
    let token_out = dir.path().join("token.txt");
    let stub = write_adk_stub(
        dir.path(),
        task_out.to_str().unwrap(),
        token_out.to_str().unwrap(),
    );

    let task = TaskContext::for_test(
        "SKILLBODY fold customer events".to_owned(),
        "INPUT renewal request for globex".to_owned(),
        MCP_ENDPOINT.to_owned(),
        vec!["update_page".to_owned(), "assign_event".to_owned()],
        SecretString::from(TOKEN.to_owned()),
    );

    // Point the adapter at the stub via the configured binary path.
    let harness = AdkHarness::new(stub.to_string_lossy().into_owned())
        .with_model(Some("gemini-3.5-flash".to_owned()))
        .with_timeout(Duration::from_secs(20));
    assert_eq!(harness.name(), "adk");

    // (b) The adapter parsed the stub's REAL stdout into an outcome.
    let outcome = harness.run(&task).await.expect("adk stub run succeeds");
    assert!(outcome.ok, "clean exit + outcome JSON → ok");
    assert_eq!(outcome.status, HarnessStatus::Ok);
    assert_eq!(outcome.summary, "adk runner folded the renewal event");
    assert_eq!(outcome.tool_calls, 3);
    assert_eq!(outcome.produced_instance, None);

    // (a) The adapter delivered the token-less task on stdin.
    let task_json = std::fs::read_to_string(&task_out).expect("stub captured the stdin task");
    assert!(
        task_json.contains("SKILLBODY fold customer events"),
        "instructions delivered: {task_json}"
    );
    assert!(
        task_json.contains("INPUT renewal request for globex"),
        "input delivered: {task_json}"
    );
    assert!(
        task_json.contains(MCP_ENDPOINT),
        "mcp endpoint delivered: {task_json}"
    );
    assert!(
        task_json.contains("update_page") && task_json.contains("assign_event"),
        "allowed tools delivered: {task_json}"
    );
    // The bearer must NOT appear in the stdin payload.
    assert!(
        !task_json.contains(TOKEN),
        "the scoped bearer must NOT appear in the stdin task: {task_json}"
    );

    // ...and the bearer DID reach the child out-of-band via the env var.
    let token_seen = std::fs::read_to_string(&token_out).expect("stub recorded the bearer env var");
    assert_eq!(
        token_seen, TOKEN,
        "the scoped bearer is delivered to the runner through ESCUREL_MCP_BEARER"
    );
}
