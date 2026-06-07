//! Always-on deterministic DoD test for the Claude Code adapter (#152).
//!
//! This is a **real subprocess** test, not a mock: the adapter spawns a real
//! stub executable (a shell script this test writes + `chmod +x`es) that
//! mimics the `claude` headless CLI's I/O contract. The stub records the
//! exact argv it was invoked with, reads the `--mcp-config` file the adapter
//! wrote, and emits a canned `claude --output-format json` result envelope on
//! stdout. The test then asserts the adapter:
//!
//!   (a) built the correct invocation — `-p <input>`,
//!       `--append-system-prompt <instructions>`, `--mcp-config <path>`,
//!       `--output-format json`, `--permission-mode bypassPermissions`, the
//!       namespaced `--allowedTools mcp__escurel__<tool>`; and that the
//!       MCP-config file declares the escurel HTTP server with the scoped
//!       `Authorization: Bearer <token>`;
//!   (b) parsed the stub's real stdout into a `HarnessOutcome`.
//!
//! The live test that drives the *real* `claude` against a *real* `/mcp`
//! lives in `crates/escurel-runner/tests/claude_live.rs`, marked `#[ignore]`
//! (LLM quota); this deterministic test is the one the gate runs every time.

use std::time::Duration;

use escurel_runner_core::{SecretString, TaskContext};
use escurel_runner_harness::{ClaudeHarness, Harness, HarnessStatus};

const TOKEN: &str = "scoped-bearer-DETERMINISTIC";
const MCP_ENDPOINT: &str = "http://127.0.0.1:8080/mcp";

/// Write an executable stub that stands in for the `claude` CLI. It:
///   - dumps its argv (one per line) to `$argv_out`,
///   - copies the `--mcp-config` file it was handed to `$mcp_out`,
///   - prints a canned success result envelope on stdout, exit 0.
///
/// Returns the path to the stub binary.
fn write_claude_stub(dir: &std::path::Path, argv_out: &str, mcp_out: &str) -> std::path::PathBuf {
    let stub = dir.join("claude-stub.sh");
    // The script walks its own args, recording each and snapshotting the file
    // that follows `--mcp-config`. POSIX sh keeps it portable on the test box.
    let script = format!(
        r#"#!/usr/bin/env bash
set -euo pipefail
: > "{argv_out}"
prev=""
for a in "$@"; do
  printf '%s\n' "$a" >> "{argv_out}"
  if [ "$prev" = "--mcp-config" ]; then
    cp "$a" "{mcp_out}"
  fi
  prev="$a"
done
printf '%s' '{{"type":"result","subtype":"success","is_error":false,"result":"folded the renewal event","num_turns":3,"session_id":"stub-1"}}'
"#
    );
    std::fs::write(&stub, script).expect("write stub script");
    // chmod +x so the OS will exec it.
    let mut perms = std::fs::metadata(&stub).expect("stat stub").permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&stub, perms).expect("chmod stub");
    stub
}

#[tokio::test]
async fn claude_adapter_builds_invocation_and_parses_real_stub_output() {
    let dir = tempfile::tempdir().expect("tempdir");
    let argv_out = dir.path().join("argv.txt");
    let mcp_out = dir.path().join("mcp-config.json");
    let stub = write_claude_stub(
        dir.path(),
        argv_out.to_str().unwrap(),
        mcp_out.to_str().unwrap(),
    );

    let task = TaskContext::for_test(
        "SKILLBODY fold customer events".to_owned(),
        "INPUT renewal request for globex".to_owned(),
        MCP_ENDPOINT.to_owned(),
        vec!["update_page".to_owned(), "assign_event".to_owned()],
        SecretString::from(TOKEN.to_owned()),
    );

    // Point the adapter at the stub via the configured binary path.
    let harness = ClaudeHarness::new(stub.to_string_lossy().into_owned())
        .with_timeout(Duration::from_secs(20));
    assert_eq!(harness.name(), "claude");

    // (b) The adapter parsed the stub's REAL stdout into an outcome.
    let outcome = harness.run(&task).await.expect("claude stub run succeeds");
    assert!(outcome.ok, "success envelope → ok");
    assert_eq!(outcome.status, HarnessStatus::Ok);
    assert_eq!(outcome.summary, "folded the renewal event");
    assert_eq!(outcome.tool_calls, 3, "num_turns mapped to tool_calls");
    assert_eq!(outcome.produced_instance, None);

    // (a) The adapter built the correct invocation.
    let argv = std::fs::read_to_string(&argv_out).expect("stub recorded its argv");
    let args: Vec<&str> = argv.lines().collect();

    let pos = |needle: &str| args.iter().position(|a| *a == needle);
    let after = |needle: &str| {
        let i = pos(needle).unwrap_or_else(|| panic!("missing arg {needle} in {args:?}"));
        args[i + 1]
    };

    assert_eq!(after("-p"), "INPUT renewal request for globex");
    assert_eq!(
        after("--append-system-prompt"),
        "SKILLBODY fold customer events"
    );
    assert_eq!(after("--output-format"), "json");
    assert_eq!(after("--permission-mode"), "bypassPermissions");
    assert!(
        args.contains(&"--strict-mcp-config"),
        "only the escurel server should be visible: {args:?}"
    );
    assert!(args.contains(&"--mcp-config"), "mcp config flag present");
    assert!(
        args.contains(&"mcp__escurel__update_page"),
        "namespaced allowed tool present: {args:?}"
    );
    assert!(
        args.contains(&"mcp__escurel__assign_event"),
        "namespaced allowed tool present: {args:?}"
    );

    // ...and the MCP-config file declares the escurel HTTP server + bearer.
    let mcp_cfg = std::fs::read_to_string(&mcp_out).expect("stub snapshotted the mcp config");
    let cfg: serde_json::Value = serde_json::from_str(&mcp_cfg).expect("mcp config is JSON");
    let server = &cfg["mcpServers"]["escurel"];
    assert_eq!(server["type"], "http");
    assert_eq!(server["url"], MCP_ENDPOINT);
    assert_eq!(
        server["headers"]["Authorization"],
        format!("Bearer {TOKEN}"),
        "the scoped bearer is carried into the MCP config"
    );
}
