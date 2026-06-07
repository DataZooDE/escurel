//! The Codex CLI harness adapter (#153).
//!
//! [`CodexHarness`] is a real [`crate::Harness`]: it drives the `codex exec`
//! non-interactive CLI as an isolated, timed, kill-on-drop subprocess,
//! registering the escurel gateway as a **streamable-HTTP MCP server** in a
//! per-run `CODEX_HOME/config.toml` (`[mcp_servers.escurel]` with `url` +
//! `bearer_token_env_var`), delivering the packaged scoped bearer to the
//! child **out-of-band via an environment variable** (never in argv or the
//! config file), prepending the skill body as the prompt framing in front of
//! the input, and capturing the `-o`/`--json` output into a
//! [`HarnessOutcome`].
//!
//! Per the contract, the adapter performs **no** escurel writes — every
//! escurel effect flows through the LLM's own `/mcp` tool calls under the
//! scoped token. The adapter is process management + invocation construction
//! + outcome capture, nothing more.
//!
//! The `codex` binary path is configurable (`ESCUREL_RUNNER_CODEX_BIN`,
//! default `codex`) so a deterministic test can point it at a real stub
//! executable that mimics the CLI's I/O contract — exercising the whole
//! invocation-build + parse path without burning LLM quota. The live
//! end-to-end test (real `codex` against a real `/mcp`) runs on demand behind
//! `#[ignore]`.
//!
//! ## Verified against codex-cli 0.137.0
//!
//! `codex mcp add escurel --url <u> --bearer-token-env-var <V>` writes exactly
//! `[mcp_servers.escurel]\nurl = "<u>"\nbearer_token_env_var = "<V>"` to
//! `$CODEX_HOME/config.toml`, so that is the streamable-HTTP MCP schema this
//! adapter emits directly. We write our own per-run `CODEX_HOME` (rather than
//! mutating the operator's `~/.codex/config.toml`) so ONLY the escurel server
//! is visible to the run — the isolation analogue of claude's
//! `--strict-mcp-config`. The full-auto-writes gotcha
//! (`docs/notes/discovered/2026-05-24-codex-full-auto-writes.md`) is contained
//! by the non-interactive bypass flag + the runner's own per-run working dir.

use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use escurel_runner_core::TaskContext;

use crate::harness::{Harness, HarnessError, HarnessOutcome, HarnessStatus};

/// The adapter's stable name — the `ESCUREL_RUNNER_HARNESS=codex` selector
/// and the value reported by [`Harness::name`].
const NAME: &str = "codex";

/// The MCP server name the gateway is registered under in `config.toml`.
const MCP_SERVER_NAME: &str = "escurel";

/// The environment variable the per-run `config.toml` names via
/// `bearer_token_env_var`; codex reads the scoped bearer from it at startup.
/// Keeping the token in the child's env (not argv / the config file) keeps it
/// out of the process table and any on-disk config snapshot.
const BEARER_ENV_VAR: &str = "ESCUREL_MCP_BEARER";

/// Default per-run timeout. A real LLM run makes several `/mcp` round-trips
/// and one or more model turns, so this is generous; deployments may tune it.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);

/// Adapter that drives the real `codex exec` non-interactive CLI.
#[derive(Debug, Clone)]
pub struct CodexHarness {
    /// Path to the `codex` binary to spawn (default `codex`; a test points
    /// this at a stub via `ESCUREL_RUNNER_CODEX_BIN`).
    bin_path: String,
    /// Optional model id passed as `-m` (config `ESCUREL_RUNNER_CODEX_MODEL`);
    /// `None` lets `codex` pick its default.
    model: Option<String>,
    /// Per-run timeout; the child is killed (kill-on-drop) if it overruns.
    timeout: Duration,
}

impl CodexHarness {
    /// Build an adapter that launches the `codex` binary at `bin_path`.
    pub fn new(bin_path: impl Into<String>) -> Self {
        Self {
            bin_path: bin_path.into(),
            model: None,
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// Set the `-m`/`--model` passed to `codex` (config
    /// `ESCUREL_RUNNER_CODEX_MODEL`). An empty/absent value is ignored.
    pub fn with_model(mut self, model: Option<String>) -> Self {
        self.model = model.filter(|m| !m.is_empty());
        self
    }

    /// Override the per-run timeout (tests use a small value).
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Render the per-run `config.toml` that registers the escurel gateway as
    /// codex's only streamable-HTTP MCP server. The bearer is **not** written
    /// here — codex reads it from the [`BEARER_ENV_VAR`] env var at startup.
    fn config_toml(task: &TaskContext) -> String {
        // NOTE: this is the exact schema `codex mcp add --url … \
        // --bearer-token-env-var …` emits in codex-cli 0.137.0. If a future
        // codex changes the streamable-HTTP key names, the deterministic stub
        // test (which asserts on these keys) is what flags the drift.
        format!(
            "[mcp_servers.{server}]\nurl = \"{url}\"\nbearer_token_env_var = \"{env}\"\n",
            server = MCP_SERVER_NAME,
            url = task.mcp_endpoint,
            env = BEARER_ENV_VAR,
        )
    }

    /// Build the single prompt argument: the skill body as framing in front
    /// of the input. Codex `exec` has no separate system-prompt flag, so we
    /// concatenate with a clear delimiter (mirroring how the Claude adapter
    /// splits `--append-system-prompt` from the `-p` prompt).
    fn build_prompt(task: &TaskContext) -> String {
        format!(
            "{instructions}\n\n---\n\n# Task input\n\n{input}",
            instructions = task.instructions,
            input = task.input,
        )
    }

    /// Assemble the full `codex` argv (excluding the binary itself) for the
    /// packaged task. Split out so the deterministic test can assert the
    /// invocation shape without spawning.
    fn build_args(&self, task: &TaskContext, output_last_message_path: &str) -> Vec<String> {
        let mut args: Vec<String> = vec![
            "exec".to_owned(),
            Self::build_prompt(task),
            // Emit JSONL events so we can count tool calls / detect errors.
            "--json".to_owned(),
            // Write the agent's final message to a file (clean to parse).
            "-o".to_owned(),
            output_last_message_path.to_owned(),
            // The runner's per-run working dir is not a git repo.
            "--skip-git-repo-check".to_owned(),
            // Unattended runner: no approval prompts, no sandbox blocking the
            // model's `/mcp` tool calls. (Contained by the runner's own
            // per-run working dir + the full-auto-writes note.)
            "--dangerously-bypass-approvals-and-sandbox".to_owned(),
        ];
        if let Some(model) = &self.model {
            args.push("-m".to_owned());
            args.push(model.clone());
        }
        args
    }
}

#[async_trait]
impl Harness for CodexHarness {
    fn name(&self) -> &str {
        NAME
    }

    async fn run(&self, task: &TaskContext) -> Result<HarnessOutcome, HarnessError> {
        // A per-run CODEX_HOME isolates the run from the operator's ambient
        // `~/.codex/config.toml`: only the escurel server is visible. The
        // tempdir (and its config.toml) is removed when this future drops.
        let codex_home = tempfile::Builder::new()
            .prefix("escurel-codex-home-")
            .tempdir()
            .map_err(|source| HarnessError::Io {
                harness: NAME,
                source,
            })?;
        std::fs::write(
            codex_home.path().join("config.toml"),
            Self::config_toml(task),
        )
        .map_err(|source| HarnessError::Io {
            harness: NAME,
            source,
        })?;

        // The agent's final message lands in this per-run tempfile.
        let last_message = tempfile::Builder::new()
            .prefix("escurel-codex-msg-")
            .suffix(".txt")
            .tempfile()
            .map_err(|source| HarnessError::Io {
                harness: NAME,
                source,
            })?;
        let last_message_path = last_message.path().to_string_lossy().into_owned();

        let args = self.build_args(task, &last_message_path);

        // kill_on_drop ties the child's lifetime to this future: a dropped
        // adapter (panic, cancellation, timeout) reaps the subprocess.
        let child = tokio::process::Command::new(&self.bin_path)
            .args(&args)
            // Point codex at the per-run config and hand it the bearer through
            // the env var its config names (out of argv / on-disk config).
            .env("CODEX_HOME", codex_home.path())
            .env(BEARER_ENV_VAR, task.token_str())
            // Headless: nothing on stdin. Capture stdout (the JSONL events)
            // and stderr (diagnostics on failure).
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|source| HarnessError::Spawn {
                harness: NAME,
                path: self.bin_path.clone(),
                source,
            })?;

        let output = match tokio::time::timeout(self.timeout, child.wait_with_output()).await {
            Ok(result) => result.map_err(|source| HarnessError::Io {
                harness: NAME,
                source,
            })?,
            Err(_elapsed) => {
                // The cancelled `wait_with_output` future drops the `Child`
                // it consumed; kill_on_drop then reaps the overrunning child.
                return Err(HarnessError::Timeout {
                    harness: NAME,
                    timeout_ms: self.timeout.as_millis() as u64,
                });
            }
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(HarnessError::NonZeroExit {
                harness: NAME,
                code: output.status.code(),
                stderr: stderr.chars().take(2000).collect(),
            });
        }

        // Read the final agent message the `-o` file captured (the clean
        // summary); the JSONL on stdout carries tool-call / error signal.
        let final_message = std::fs::read_to_string(&last_message_path).unwrap_or_default();
        parse_outcome(&final_message, &output.stdout)
    }
}

/// Parse the codex `exec` output into a [`HarnessOutcome`].
///
/// Two streams:
/// - `final_message` — the `-o`/`--output-last-message` file: the agent's
///   final text. Used (truncated) as the summary.
/// - `jsonl` — the `--json` event stream on stdout, one JSON object per line.
///   We scan it for **MCP tool-call** events (to count `tool_calls`, a coarse
///   but real proxy for how much the model did) and for **error** events (to
///   flip `ok` false even on a zero exit).
///
/// `produced_instance` ← `None`: the codex stream does not name the page the
/// model wrote; the runner's reconcile reads that back from the gateway.
fn parse_outcome(final_message: &str, jsonl: &[u8]) -> Result<HarnessOutcome, HarnessError> {
    let text = String::from_utf8_lossy(jsonl);

    let mut tool_calls: u32 = 0;
    let mut saw_error = false;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // Codex emits non-JSON banner lines around the JSONL too; tolerate
        // anything that does not parse as a JSON object.
        let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if is_tool_call_event(&event) {
            tool_calls += 1;
        }
        if is_error_event(&event) {
            saw_error = true;
        }
    }

    let ok = !saw_error;
    let summary = {
        let trimmed = final_message.trim();
        if trimmed.is_empty() {
            "codex run completed".to_owned()
        } else {
            trimmed.chars().take(2000).collect()
        }
    };

    Ok(HarnessOutcome {
        ok,
        status: if ok {
            HarnessStatus::Ok
        } else {
            HarnessStatus::Failed
        },
        summary,
        tool_calls,
        produced_instance: None,
    })
}

/// Recognise a codex `--json` event that represents an MCP tool call.
///
/// Codex's event taxonomy is coarser than claude's and has churned across
/// versions, so we match defensively on the type/item-type strings rather
/// than pinning one exact schema: any event whose `type` or nested
/// `item.item_type` contains both an "mcp"/"tool" hint counts. The
/// deterministic stub pins the shape we emit; this keeps a minor codex
/// version bump from silently zeroing the count.
fn is_tool_call_event(event: &serde_json::Value) -> bool {
    let type_str = event
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let item_type = event
        .get("item")
        .and_then(|i| i.get("item_type"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let hay = format!("{type_str} {item_type}").to_ascii_lowercase();
    (hay.contains("mcp") && hay.contains("tool")) || hay.contains("tool_call")
}

/// Recognise a codex `--json` event that signals the run errored.
fn is_error_event(event: &serde_json::Value) -> bool {
    let type_str = event
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    type_str.eq_ignore_ascii_case("error") || type_str.to_ascii_lowercase().contains("error")
}

#[cfg(test)]
mod tests {
    use super::*;
    use escurel_runner_core::SecretString;

    fn task() -> TaskContext {
        TaskContext::for_test(
            "INSTRUCTMARK skill body".to_owned(),
            "INPUTMARK fold this".to_owned(),
            "http://127.0.0.1:8080/mcp".to_owned(),
            vec!["update_page".to_owned(), "assign_event".to_owned()],
            SecretString::from("scoped-bearer-XYZ".to_owned()),
        )
    }

    #[test]
    fn name_is_codex() {
        assert_eq!(CodexHarness::new("codex").name(), "codex");
    }

    #[test]
    fn config_toml_declares_escurel_http_server_with_env_var_bearer() {
        let toml = CodexHarness::config_toml(&task());
        assert!(toml.contains("[mcp_servers.escurel]"), "{toml}");
        assert!(
            toml.contains("url = \"http://127.0.0.1:8080/mcp\""),
            "{toml}"
        );
        assert!(
            toml.contains("bearer_token_env_var = \"ESCUREL_MCP_BEARER\""),
            "{toml}"
        );
        // The token itself is never written to the config — only the env-var
        // name that codex reads it from.
        assert!(!toml.contains("scoped-bearer-XYZ"), "{toml}");
    }

    #[test]
    fn prompt_carries_instructions_then_input() {
        let prompt = CodexHarness::build_prompt(&task());
        assert!(prompt.contains("INSTRUCTMARK skill body"));
        assert!(prompt.contains("INPUTMARK fold this"));
        // Instructions framing precedes the task input.
        assert!(
            prompt.find("INSTRUCTMARK").unwrap() < prompt.find("INPUTMARK").unwrap(),
            "instructions frame the input: {prompt}"
        );
    }

    #[test]
    fn argv_carries_exec_json_output_skip_git_and_bypass() {
        let h = CodexHarness::new("codex").with_model(Some("o3".to_owned()));
        let args = h.build_args(&task(), "/tmp/msg.txt");

        assert_eq!(args.first().map(String::as_str), Some("exec"));
        assert_eq!(args.get(1).map(|s| s.contains("INPUTMARK")), Some(true));
        assert!(args.iter().any(|a| a == "--json"));
        assert!(args.iter().any(|a| a == "--skip-git-repo-check"));
        assert!(
            args.iter()
                .any(|a| a == "--dangerously-bypass-approvals-and-sandbox")
        );
        let o = args.iter().position(|a| a == "-o").expect("-o present");
        assert_eq!(args[o + 1], "/tmp/msg.txt");
        let m = args.iter().position(|a| a == "-m").expect("-m present");
        assert_eq!(args[m + 1], "o3");
    }

    #[test]
    fn model_is_omitted_when_unset() {
        let args = CodexHarness::new("codex").build_args(&task(), "/tmp/msg.txt");
        assert!(!args.iter().any(|a| a == "-m"));
    }

    #[test]
    fn parses_tool_calls_and_final_message_into_ok_outcome() {
        let jsonl =
            br#"{"type":"item.completed","item":{"item_type":"mcp_tool_call","tool":"update_page"}}
{"type":"item.completed","item":{"item_type":"mcp_tool_call","tool":"assign_event"}}
{"type":"turn.completed"}"#;
        let outcome = parse_outcome("folded the event", jsonl).expect("parse");
        assert!(outcome.ok);
        assert_eq!(outcome.status, HarnessStatus::Ok);
        assert_eq!(outcome.summary, "folded the event");
        assert_eq!(outcome.tool_calls, 2);
        assert_eq!(outcome.produced_instance, None);
    }

    #[test]
    fn error_event_flips_outcome_failed() {
        let jsonl = br#"{"type":"item.completed","item":{"item_type":"mcp_tool_call"}}
{"type":"error","message":"model refused"}"#;
        let outcome = parse_outcome("", jsonl).expect("parse");
        assert!(!outcome.ok);
        assert_eq!(outcome.status, HarnessStatus::Failed);
        assert_eq!(outcome.tool_calls, 1);
        // Empty final message falls back to a stable summary.
        assert_eq!(outcome.summary, "codex run completed");
    }

    #[test]
    fn non_json_banner_lines_are_tolerated() {
        let jsonl = b"Reading prompt from stdin...\n{\"type\":\"turn.completed\"}\nDone.\n";
        let outcome = parse_outcome("ok", jsonl).expect("parse");
        assert!(outcome.ok);
        assert_eq!(outcome.tool_calls, 0);
    }
}
