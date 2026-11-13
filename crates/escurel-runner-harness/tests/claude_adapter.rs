//! Always-on deterministic DoD test for the Claude Code adapter (#152).
//!
//! This is a **real subprocess** test, not a mock: the adapter spawns a real
//! stub executable (a shell script this test writes + `chmod +x`es) that
//! mimics the `claude` headless CLI's I/O contract. The stub records the
//! exact argv it was invoked with, reads the `--mcp-config` file the adapter
//! wrote, and speaks the `--input-format stream-json --output-format
//! stream-json` session contract: one user message per stdin line, one
//! `result` line per turn, exit at stdin EOF. The test then asserts the adapter:
//!
//!   (a) built the correct invocation — `-p <input>`,
//!       `--append-system-prompt <instructions>`, `--mcp-config <path>`,
//!       stream-json in and out, `--permission-mode bypassPermissions`, the
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
///   - answers each **stdin** line (a stream-json user message: this is
///     where the prompt arrives) with an assistant event and a `result`
///     line, recording the lines to `$stdin_out`,
///   - copies the `--mcp-config` file it was handed to `$mcp_out`,
///   - exits 0 at stdin EOF — which only comes if the adapter closes stdin
///     once the turn's result is out.
///
/// Returns the path to the stub binary.
fn write_claude_stub(
    dir: &std::path::Path,
    argv_out: &str,
    mcp_out: &str,
    stdin_out: &str,
) -> std::path::PathBuf {
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
: > "{stdin_out}"
printf '%s\n' '{{"type":"system","subtype":"init","session_id":"stub-1"}}'
# One turn per stdin line. The adapter must close stdin after the result, or
# this loop blocks until the timeout — itself part of the contract asserted.
while IFS= read -r line; do
  printf '%s\n' "$line" >> "{stdin_out}"
  printf '%s\n' '{{"type":"assistant","message":{{"content":[{{"type":"tool_use","id":"t1","name":"mcp__escurel__update_page","input":{{"page_id":"p"}}}}]}}}}'
  printf '%s\n' '{{"type":"assistant","message":{{"content":[{{"type":"text","text":"working"}}]}}}}'
  printf '%s\n' '{{"type":"result","subtype":"success","is_error":false,"result":"folded the renewal event","num_turns":3,"session_id":"stub-1"}}'
done
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
    let stdin_out = dir.path().join("stdin.txt");
    let stub = write_claude_stub(
        dir.path(),
        argv_out.to_str().unwrap(),
        mcp_out.to_str().unwrap(),
        stdin_out.to_str().unwrap(),
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
    assert_eq!(
        outcome.reply.as_deref(),
        Some("folded the renewal event"),
        "the final assistant message is the user-facing reply"
    );
    assert_eq!(
        outcome.tool_calls, 1,
        "the stream's tool_use lines are counted"
    );
    assert_eq!(outcome.produced_instance, None);

    // (a) The adapter built the correct invocation.
    let argv = std::fs::read_to_string(&argv_out).expect("stub recorded its argv");
    let args: Vec<&str> = argv.lines().collect();

    let pos = |needle: &str| args.iter().position(|a| *a == needle);
    let after = |needle: &str| {
        let i = pos(needle).unwrap_or_else(|| panic!("missing arg {needle} in {args:?}"));
        args[i + 1]
    };

    // The prompt travels on stdin, not argv. Linux caps a single argv string
    // at 32 pages, so a packaged meeting transcript (150-220 KB) made spawn
    // fail with E2BIG and the event permanently undispatchable.
    assert!(args.contains(&"-p"), "headless print mode: {args:?}");
    assert!(
        !args.iter().any(|a| a.contains("INPUT renewal")),
        "the input must NOT be in argv: {args:?}"
    );
    let stdin_seen = std::fs::read_to_string(&stdin_out).expect("stub recorded its stdin");
    let turns: Vec<serde_json::Value> = stdin_seen
        .lines()
        .map(|l| serde_json::from_str(l).expect("a stream-json line"))
        .collect();
    assert_eq!(turns.len(), 1, "one user turn: {stdin_seen}");
    assert_eq!(turns[0]["type"], "user");
    assert_eq!(
        turns[0]["message"]["content"][0]["text"], "INPUT renewal request for globex",
        "the adapter wrote the prompt to the child's stdin as a user message"
    );

    assert_eq!(
        after("--append-system-prompt"),
        "SKILLBODY fold customer events"
    );
    assert_eq!(after("--input-format"), "stream-json");
    assert_eq!(after("--output-format"), "stream-json");
    assert!(args.contains(&"--verbose"), "-p needs it for stream-json");
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

/// #820: a message injected into a live claude run is written into the
/// session as one more stream-json user turn, stdin stays open until that
/// turn's `result` too, and the outcome is the last turn's.
#[tokio::test]
async fn an_injected_message_is_one_more_user_turn_in_the_live_session() {
    let dir = tempfile::tempdir().expect("tempdir");
    let stdin_out = dir.path().join("stdin.txt");
    let stub = dir.path().join("claude-stub.sh");
    // One result per user turn, each naming the turn; exit only at EOF.
    let script = format!(
        r#"#!/usr/bin/env bash
set -euo pipefail
: > "{out}"
n=0
while IFS= read -r line; do
  n=$((n+1))
  printf '%s\n' "$line" >> "{out}"
  printf '{{"type":"result","subtype":"success","is_error":false,"result":"turn %s","num_turns":%s}}\n' "$n" "$n"
done
"#,
        out = stdin_out.display()
    );
    std::fs::write(&stub, script).expect("write stub");
    let mut perms = std::fs::metadata(&stub).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&stub, perms).unwrap();

    let mut task = TaskContext::for_test(
        "SKILLBODY".to_owned(),
        "INPUT first".to_owned(),
        MCP_ENDPOINT.to_owned(),
        vec!["update_page".to_owned()],
        SecretString::from(TOKEN.to_owned()),
    );
    let (tx, inbox) = escurel_runner_core::InjectInbox::channel();
    assert!(tx.send("STEER use 2027"));
    task.inject = Some(inbox);

    let harness = ClaudeHarness::new(stub.to_string_lossy().into_owned())
        .with_timeout(Duration::from_secs(20));
    assert!(harness.supports_inject());
    let outcome = harness.run(&task).await.expect("the session ended at EOF");
    assert_eq!(outcome.summary, "turn 2", "the last turn's result");

    let seen = std::fs::read_to_string(&stdin_out).unwrap();
    let texts: Vec<String> = seen
        .lines()
        .map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).unwrap();
            assert_eq!(v["type"], "user", "{l}");
            v["message"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(texts, ["INPUT first", "STEER use 2027"]);
    drop(tx);
}

/// A stub `claude` that answers each stdin turn with one `result` line
/// after `delay` seconds, logs each turn to `stdin_out` as it reads it, and
/// writes `eof_out` when its stdin closes — then lingers a second before it
/// exits, as a real CLI winding down would.
fn write_slow_claude_stub(
    dir: &std::path::Path,
    stdin_out: &std::path::Path,
    eof_out: &std::path::Path,
    delay: &str,
) -> std::path::PathBuf {
    let stub = dir.join("claude-slow.sh");
    let script = format!(
        r#"#!/usr/bin/env bash
set -euo pipefail
: > "{out}"
n=0
while IFS= read -r line; do
  n=$((n+1))
  printf '%s\n' "$line" >> "{out}"
  sleep {delay}
  printf '{{"type":"result","subtype":"success","is_error":false,"result":"turn %s","num_turns":%s}}\n' "$n" "$n"
done
echo eof > "{eof}"
sleep 1
"#,
        out = stdin_out.display(),
        eof = eof_out.display(),
    );
    std::fs::write(&stub, script).expect("write stub");
    let mut perms = std::fs::metadata(&stub).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&stub, perms).unwrap();
    stub
}

fn inject_task() -> (TaskContext, escurel_runner_core::InjectSender) {
    let mut task = TaskContext::for_test(
        "SKILLBODY".to_owned(),
        "INPUT first".to_owned(),
        MCP_ENDPOINT.to_owned(),
        vec!["update_page".to_owned()],
        SecretString::from(TOKEN.to_owned()),
    );
    let (tx, inbox) = escurel_runner_core::InjectInbox::channel();
    task.inject = Some(inbox);
    (task, tx)
}

async fn wait_for_file(path: &std::path::Path, what: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !std::fs::read_to_string(path).is_ok_and(|s| !s.is_empty()) {
        assert!(
            std::time::Instant::now() < deadline,
            "the stub never reached {what}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn turn_texts(stdin_out: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(stdin_out)
        .unwrap()
        .lines()
        .map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).unwrap();
            v["message"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect()
}

/// Crew review PR-1, F9: a message sent while the first turn is still being
/// worked on (the stub has read it and not answered) is accepted and becomes
/// the next turn of the same session.
#[tokio::test]
async fn a_message_injected_mid_turn_is_delivered_as_the_next_turn() {
    let dir = tempfile::tempdir().expect("tempdir");
    let stdin_out = dir.path().join("stdin.txt");
    let eof_out = dir.path().join("eof");
    let stub = write_slow_claude_stub(dir.path(), &stdin_out, &eof_out, "1");
    let (task, tx) = inject_task();
    let harness = ClaudeHarness::new(stub.to_string_lossy().into_owned())
        .with_timeout(Duration::from_secs(20));
    let run = tokio::spawn(async move { harness.run(&task).await });

    wait_for_file(&stdin_out, "the first turn").await;
    assert!(tx.send("STEER mid-turn"), "the live session accepts it");

    let outcome = run.await.unwrap().expect("the session ended at EOF");
    assert_eq!(outcome.summary, "turn 2", "the injected turn was answered");
    assert_eq!(turn_texts(&stdin_out), ["INPUT first", "STEER mid-turn"]);
}

/// Crew review PR-1, F9: once the last turn's `result` is out the session is
/// ending — stdin is closed even while the process winds down — so a message
/// is refused (`not_live` at the runner), never accepted and then dropped.
#[tokio::test]
async fn a_message_after_the_final_result_is_refused_not_dropped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let stdin_out = dir.path().join("stdin.txt");
    let eof_out = dir.path().join("eof");
    let stub = write_slow_claude_stub(dir.path(), &stdin_out, &eof_out, "0");
    let (task, tx) = inject_task();
    let harness = ClaudeHarness::new(stub.to_string_lossy().into_owned())
        .with_timeout(Duration::from_secs(20));
    let run = tokio::spawn(async move { harness.run(&task).await });

    // The stub saw its stdin close but has not exited yet.
    wait_for_file(&eof_out, "its stdin EOF").await;
    assert!(!run.is_finished(), "the process is still winding down");
    assert!(!tx.send("STEER too late"), "refused: nothing will read it");

    let outcome = run.await.unwrap().expect("the session ended at EOF");
    assert_eq!(outcome.summary, "turn 1");
    assert_eq!(turn_texts(&stdin_out), ["INPUT first"]);
}
