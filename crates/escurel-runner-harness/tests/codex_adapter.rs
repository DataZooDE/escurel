//! Always-on deterministic DoD test for the Codex CLI adapter (#153).
//!
//! This is a **real subprocess** test, not a mock: the adapter spawns a real
//! stub executable (a shell script this test writes + `chmod +x`es) that
//! mimics the `codex exec` non-interactive I/O contract. The stub records the
//! exact argv it was invoked with, snapshots the `config.toml` the adapter
//! wrote into the per-run `CODEX_HOME`, and emits a canned `codex --json`
//! event stream on stdout *plus* writes the agent's final message to the
//! `-o/--output-last-message` file it was handed. The test then asserts the
//! adapter:
//!
//!   (a) built the correct invocation — `exec <prompt>` (instructions framing
//!       + input), `--json`, `-o <tempfile>`, `--skip-git-repo-check`, the
//!       non-interactive `--dangerously-bypass-approvals-and-sandbox` flag,
//!       and the `-m <model>` override when configured; and that the
//!       per-run `CODEX_HOME/config.toml` declares ONLY the escurel
//!       streamable-HTTP MCP server (`url` + `bearer_token_env_var`), with the
//!       scoped bearer delivered out-of-band via that env var (never in argv
//!       or the config file);
//!   (b) parsed the stub's real output (final-message file + JSONL events)
//!       into a `HarnessOutcome` with `ok:true` and `tool_calls >= 1`.
//!
//! The live test that drives the *real* `codex` against a *real* `/mcp` lives
//! in `crates/escurel-runner/tests/codex_live.rs`, marked `#[ignore]` (LLM
//! quota); this deterministic test is the one the gate runs every time.

use std::time::Duration;

use escurel_runner_core::{SecretString, TaskContext};
use escurel_runner_harness::{CodexHarness, Harness, HarnessStatus};

const TOKEN: &str = "scoped-bearer-DETERMINISTIC";
const MCP_ENDPOINT: &str = "http://127.0.0.1:8080/mcp";

/// Write an executable stub that stands in for the `codex` CLI. It:
///   - dumps its argv (one per line) to `$argv_out`,
///   - copies `$CODEX_HOME/config.toml` (the MCP registration) to `$cfg_out`,
///   - records the value of the bearer-token env var to `$token_out`,
///   - writes a canned final agent message to the file after `-o`,
///   - prints a canned `--json` JSONL event stream (incl. a tool-call event
///     and a completion) on stdout, exit 0.
///
/// Returns the path to the stub binary.
fn write_codex_stub(
    dir: &std::path::Path,
    argv_out: &str,
    cfg_out: &str,
    token_out: &str,
) -> std::path::PathBuf {
    let stub = dir.join("codex-stub.sh");
    // The script walks its own args, recording each and snapshotting the
    // file that follows `-o`/`--output-last-message`. It copies the MCP
    // config out of CODEX_HOME and records the escurel bearer env var so the
    // test can prove the token rode out-of-band.
    let script = format!(
        r#"#!/usr/bin/env bash
set -euo pipefail
: > "{argv_out}"
out_file=""
prev=""
# Record one argv token per NUL-delimited record: a prompt arg may itself
# contain newlines, so a newline-per-arg dump would mis-split it.
for a in "$@"; do
  printf '%s\0' "$a" >> "{argv_out}"
  if [ "$prev" = "-o" ] || [ "$prev" = "--output-last-message" ]; then
    out_file="$a"
  fi
  prev="$a"
done
# Snapshot the per-run MCP registration + the bearer the adapter handed us.
cp "${{CODEX_HOME}}/config.toml" "{cfg_out}"
printf '%s' "${{ESCUREL_MCP_BEARER:-}}" > "{token_out}"
# The agent's final message lands in the -o file (clean to parse).
if [ -n "$out_file" ]; then
  printf '%s' 'folded the renewal event into globex' > "$out_file"
fi
# A canned --json event stream: a tool call then a turn completion.
printf '%s\n' '{{"type":"item.completed","item":{{"item_type":"mcp_tool_call","server":"escurel","tool":"update_page"}}}}'
printf '%s\n' '{{"type":"item.completed","item":{{"item_type":"mcp_tool_call","server":"escurel","tool":"assign_event"}}}}'
printf '%s\n' '{{"type":"turn.completed","usage":{{"input_tokens":10,"output_tokens":5}}}}'
"#,
        argv_out = argv_out,
        cfg_out = cfg_out,
        token_out = token_out,
    );
    std::fs::write(&stub, script).expect("write stub script");
    // chmod +x so the OS will exec it.
    let mut perms = std::fs::metadata(&stub).expect("stat stub").permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&stub, perms).expect("chmod stub");
    stub
}

#[tokio::test]
async fn codex_adapter_builds_invocation_and_parses_real_stub_output() {
    let dir = tempfile::tempdir().expect("tempdir");
    let argv_out = dir.path().join("argv.txt");
    let cfg_out = dir.path().join("config.toml");
    let token_out = dir.path().join("token.txt");
    let stub = write_codex_stub(
        dir.path(),
        argv_out.to_str().unwrap(),
        cfg_out.to_str().unwrap(),
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
    let harness = CodexHarness::new(stub.to_string_lossy().into_owned())
        .with_model(Some("o3".to_owned()))
        .with_timeout(Duration::from_secs(20));
    assert_eq!(harness.name(), "codex");

    // (b) The adapter parsed the stub's REAL output into an outcome.
    let outcome = harness.run(&task).await.expect("codex stub run succeeds");
    assert!(outcome.ok, "clean exit + final message → ok");
    assert_eq!(outcome.status, HarnessStatus::Ok);
    assert_eq!(outcome.summary, "folded the renewal event into globex");
    assert!(
        outcome.tool_calls >= 1,
        "JSONL tool-call events counted: {}",
        outcome.tool_calls
    );
    assert_eq!(outcome.produced_instance, None);

    // (a) The adapter built the correct invocation. Args are NUL-delimited
    // so a prompt containing newlines stays one token.
    let argv = std::fs::read_to_string(&argv_out).expect("stub recorded its argv");
    let args: Vec<&str> = argv.split('\0').filter(|s| !s.is_empty()).collect();

    let pos = |needle: &str| args.iter().position(|a| *a == needle);
    let after = |needle: &str| {
        let i = pos(needle).unwrap_or_else(|| panic!("missing arg {needle} in {args:?}"));
        args[i + 1]
    };

    // First positional is the `exec` subcommand.
    assert_eq!(args.first(), Some(&"exec"), "exec subcommand: {args:?}");
    // The prompt carries BOTH the instructions framing and the input.
    let prompt = after("exec");
    assert!(
        prompt.contains("SKILLBODY fold customer events"),
        "prompt carries the instructions: {prompt:?}"
    );
    assert!(
        prompt.contains("INPUT renewal request for globex"),
        "prompt carries the input: {prompt:?}"
    );
    assert!(args.contains(&"--json"), "--json present: {args:?}");
    assert!(
        args.contains(&"--skip-git-repo-check"),
        "--skip-git-repo-check present: {args:?}"
    );
    assert!(
        args.contains(&"--dangerously-bypass-approvals-and-sandbox"),
        "non-interactive sandbox/approval flag present: {args:?}"
    );
    assert!(
        args.contains(&"-o") || args.contains(&"--output-last-message"),
        "output-last-message flag present: {args:?}"
    );
    assert_eq!(after("-m"), "o3", "model override passed");

    // The per-run CODEX_HOME/config.toml registers ONLY the escurel HTTP
    // server with the bearer-token env var indirection.
    let cfg = std::fs::read_to_string(&cfg_out).expect("stub snapshotted the codex config");
    assert!(
        cfg.contains("[mcp_servers.escurel]"),
        "escurel MCP server registered: {cfg}"
    );
    assert!(
        cfg.contains(&format!("url = \"{MCP_ENDPOINT}\"")),
        "streamable-HTTP url set: {cfg}"
    );
    assert!(
        cfg.contains("bearer_token_env_var"),
        "bearer delivered via env-var indirection (not inline): {cfg}"
    );
    assert!(
        !cfg.contains(TOKEN),
        "the scoped bearer must NOT appear in the config file: {cfg}"
    );
    // The argv must not carry the bearer either.
    assert!(
        !argv.contains(TOKEN),
        "the scoped bearer must NOT appear in argv: {argv}"
    );

    // ...and the bearer DID reach the child out-of-band via the env var.
    let token_seen = std::fs::read_to_string(&token_out).expect("stub recorded the bearer env var");
    assert_eq!(
        token_seen, TOKEN,
        "the scoped bearer is delivered to codex through the env var"
    );
}
