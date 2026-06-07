//! The Claude Code CLI harness adapter (#152).
//!
//! [`ClaudeHarness`] is a real [`crate::Harness`]: it drives the `claude`
//! headless CLI (`claude -p`) as an isolated, timed, kill-on-drop
//! subprocess, registering the escurel gateway as an **HTTP MCP server**
//! (`--mcp-config`) with the packaged scoped bearer, narrowing the tool
//! surface (`--allowedTools mcp__escurel__<tool>`), injecting the skill body
//! as the appended system prompt, and capturing the `--output-format json`
//! result envelope into a [`HarnessOutcome`].
//!
//! Per the contract, the adapter performs **no** escurel writes — every
//! escurel effect flows through the LLM's own `/mcp` tool calls under the
//! scoped token. The adapter is process management + invocation construction
//! + outcome capture, nothing more.
//!
//! The `claude` binary path is configurable (`ESCUREL_RUNNER_CLAUDE_BIN`,
//! default `claude`) so a deterministic test can point it at a real stub
//! executable that mimics the CLI's I/O contract — exercising the whole
//! invocation-build + parse path without burning LLM quota. The live
//! end-to-end test (real `claude` against a real `/mcp`) runs on demand
//! behind `#[ignore]`.

use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use escurel_runner_core::TaskContext;

use crate::harness::{Harness, HarnessError, HarnessOutcome, HarnessStatus};

/// The adapter's stable name — the `ESCUREL_RUNNER_HARNESS=claude` selector
/// and the value reported by [`Harness::name`].
const NAME: &str = "claude";

/// The MCP server name the gateway is registered under in the `--mcp-config`.
/// Claude exposes each of its tools as `mcp__<server>__<tool>`, so this name
/// is the prefix the `--allowedTools` entries are built from.
const MCP_SERVER_NAME: &str = "escurel";

/// Default per-run timeout. A real LLM run makes several `/mcp` round-trips
/// and one or more model turns, so this is generous; deployments may tune it.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);

/// Adapter that drives the real `claude` headless CLI as a subprocess.
#[derive(Debug, Clone)]
pub struct ClaudeHarness {
    /// Path to the `claude` binary to spawn (default `claude`; a test points
    /// this at a stub via `ESCUREL_RUNNER_CLAUDE_BIN`).
    bin_path: String,
    /// Optional model alias/name passed as `--model` (config
    /// `ESCUREL_RUNNER_CLAUDE_MODEL`); `None` lets `claude` pick its default.
    model: Option<String>,
    /// Per-run timeout; the child is killed (kill-on-drop) if it overruns.
    timeout: Duration,
}

impl ClaudeHarness {
    /// Build an adapter that launches the `claude` binary at `bin_path`.
    pub fn new(bin_path: impl Into<String>) -> Self {
        Self {
            bin_path: bin_path.into(),
            model: None,
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// Set the `--model` passed to `claude` (config
    /// `ESCUREL_RUNNER_CLAUDE_MODEL`). An empty/absent value is ignored.
    pub fn with_model(mut self, model: Option<String>) -> Self {
        self.model = model.filter(|m| !m.is_empty());
        self
    }

    /// Override the per-run timeout (tests use a small value).
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Build the escurel HTTP MCP-server config JSON `claude` consumes via
    /// `--mcp-config`. The gateway is an `http` server at the packaged
    /// `/mcp` endpoint, authenticated with the scoped bearer.
    fn mcp_config_json(task: &TaskContext) -> String {
        serde_json::json!({
            "mcpServers": {
                MCP_SERVER_NAME: {
                    "type": "http",
                    "url": task.mcp_endpoint,
                    "headers": {
                        "Authorization": format!("Bearer {}", task.token_str()),
                    },
                },
            },
        })
        .to_string()
    }

    /// Map the packaged `allowed_tools` (bare escurel tool names) onto the
    /// fully-qualified Claude tool ids (`mcp__escurel__<tool>`) that
    /// `--allowedTools` expects for an MCP-server tool.
    fn allowed_tool_ids(task: &TaskContext) -> Vec<String> {
        task.allowed_tools
            .iter()
            .map(|t| format!("mcp__{MCP_SERVER_NAME}__{t}"))
            .collect()
    }

    /// Assemble the full `claude` argv (excluding the binary itself) for the
    /// packaged task. Split out so the deterministic test can assert the
    /// invocation shape without spawning.
    fn build_args(&self, task: &TaskContext, mcp_config_path: &str) -> Vec<String> {
        let mut args: Vec<String> = vec![
            "-p".to_owned(),
            task.input.clone(),
            "--append-system-prompt".to_owned(),
            task.instructions.clone(),
            "--mcp-config".to_owned(),
            mcp_config_path.to_owned(),
            // Only the escurel MCP server should be visible — never the
            // ambient user/project MCP config of whatever host runs the
            // runner.
            "--strict-mcp-config".to_owned(),
            "--output-format".to_owned(),
            "json".to_owned(),
            // Unattended runner: no interactive permission prompts.
            "--permission-mode".to_owned(),
            "bypassPermissions".to_owned(),
        ];
        // `--allowedTools` takes a space/comma-separated list; pass each id as
        // its own token so tool names never need escaping.
        args.push("--allowedTools".to_owned());
        for id in Self::allowed_tool_ids(task) {
            args.push(id);
        }
        if let Some(model) = &self.model {
            args.push("--model".to_owned());
            args.push(model.clone());
        }
        args
    }
}

#[async_trait]
impl Harness for ClaudeHarness {
    fn name(&self) -> &str {
        NAME
    }

    async fn run(&self, task: &TaskContext) -> Result<HarnessOutcome, HarnessError> {
        // Write the MCP-server config to a per-run tempfile. Passing it as a
        // path (rather than inline) keeps the scoped bearer out of the argv /
        // process table; `tempfile` removes it on drop at the end of the run.
        let mut mcp_config = tempfile::Builder::new()
            .prefix("escurel-claude-mcp-")
            .suffix(".json")
            .tempfile()
            .map_err(|source| HarnessError::Io {
                harness: NAME,
                source,
            })?;
        std::io::Write::write_all(&mut mcp_config, Self::mcp_config_json(task).as_bytes())
            .map_err(|source| HarnessError::Io {
                harness: NAME,
                source,
            })?;
        let mcp_config_path = mcp_config.path().to_string_lossy().into_owned();

        let args = self.build_args(task, &mcp_config_path);

        // kill_on_drop ties the child's lifetime to this future: a dropped
        // adapter (panic, cancellation, timeout) reaps the subprocess.
        let child = tokio::process::Command::new(&self.bin_path)
            .args(&args)
            // Headless: nothing on stdin. Capture stdout (the JSON envelope)
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

        parse_outcome(&output.stdout)
    }
}

/// Parse the `claude --output-format json` result envelope into a
/// [`HarnessOutcome`].
///
/// The envelope is a single JSON object of (roughly) the shape:
/// `{"type":"result","subtype":"success","is_error":false,
///   "result":"<final assistant text>","num_turns":N, ...}`.
/// We read the fields we can map and tolerate the rest:
///
/// - `ok`/`status` ← `is_error` (false → ok). `subtype == "success"` is the
///   confirming signal when present.
/// - `summary` ← the `result` text (truncated), falling back to `subtype`.
/// - `tool_calls` ← `num_turns` when present (a coarse but real proxy for how
///   much the model did; the JSON envelope carries no exact tool-call count).
/// - `produced_instance` ← `None`: the envelope does not name the page the
///   model wrote; the runner's reconcile reads that back from the gateway.
fn parse_outcome(stdout: &[u8]) -> Result<HarnessOutcome, HarnessError> {
    let envelope: serde_json::Value =
        serde_json::from_slice(stdout).map_err(|source| HarnessError::BadOutcome {
            harness: NAME,
            source,
        })?;

    let is_error = envelope
        .get("is_error")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let subtype = envelope
        .get("subtype")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let ok = !is_error && subtype != "error";

    let summary = envelope
        .get("result")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().take(2000).collect::<String>())
        .unwrap_or_else(|| {
            if subtype.is_empty() {
                "claude run completed".to_owned()
            } else {
                format!("claude run: {subtype}")
            }
        });

    let tool_calls = envelope
        .get("num_turns")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0) as u32;

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

#[cfg(test)]
mod tests {
    use super::*;
    use escurel_runner_core::SecretString;

    /// Build a `TaskContext` for unit assertions. `TaskContext`'s fields are
    /// crate-private to `escurel-runner-core`, but its public constructor
    /// path is `package(...)`; for a pure unit test of the argv/config shape
    /// we reach for the test-only constructor the core crate exposes.
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
    fn name_is_claude() {
        assert_eq!(ClaudeHarness::new("claude").name(), "claude");
    }

    #[test]
    fn mcp_config_declares_escurel_http_server_with_bearer() {
        let json = ClaudeHarness::mcp_config_json(&task());
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let server = &v["mcpServers"]["escurel"];
        assert_eq!(server["type"], "http");
        assert_eq!(server["url"], "http://127.0.0.1:8080/mcp");
        assert_eq!(
            server["headers"]["Authorization"], "Bearer scoped-bearer-XYZ",
            "the scoped bearer rides in the Authorization header"
        );
    }

    #[test]
    fn allowed_tools_are_namespaced_to_the_escurel_server() {
        let ids = ClaudeHarness::allowed_tool_ids(&task());
        assert_eq!(
            ids,
            vec![
                "mcp__escurel__update_page".to_owned(),
                "mcp__escurel__assign_event".to_owned()
            ]
        );
    }

    #[test]
    fn argv_carries_print_prompt_system_prompt_and_json_output() {
        let h = ClaudeHarness::new("claude").with_model(Some("opus".to_owned()));
        let args = h.build_args(&task(), "/tmp/mcp.json");

        // headless print mode with the input as the prompt
        let p = args.iter().position(|a| a == "-p").expect("-p present");
        assert_eq!(args[p + 1], "INPUTMARK fold this");

        // skill body in the appended system prompt
        let sp = args
            .iter()
            .position(|a| a == "--append-system-prompt")
            .expect("--append-system-prompt present");
        assert_eq!(args[sp + 1], "INSTRUCTMARK skill body");

        // mcp config path + strict + json output + non-interactive perms
        let mc = args
            .iter()
            .position(|a| a == "--mcp-config")
            .expect("--mcp-config present");
        assert_eq!(args[mc + 1], "/tmp/mcp.json");
        assert!(args.iter().any(|a| a == "--strict-mcp-config"));
        let of = args
            .iter()
            .position(|a| a == "--output-format")
            .expect("--output-format present");
        assert_eq!(args[of + 1], "json");
        let pm = args
            .iter()
            .position(|a| a == "--permission-mode")
            .expect("--permission-mode present");
        assert_eq!(args[pm + 1], "bypassPermissions");

        // model, when configured
        let m = args.iter().position(|a| a == "--model").expect("--model");
        assert_eq!(args[m + 1], "opus");

        // namespaced allowed tools
        assert!(args.iter().any(|a| a == "mcp__escurel__update_page"));
        assert!(args.iter().any(|a| a == "mcp__escurel__assign_event"));
    }

    #[test]
    fn model_is_omitted_when_unset() {
        let args = ClaudeHarness::new("claude").build_args(&task(), "/tmp/mcp.json");
        assert!(!args.iter().any(|a| a == "--model"));
    }

    #[test]
    fn parses_success_envelope_into_ok_outcome() {
        let stdout = br#"{"type":"result","subtype":"success","is_error":false,
            "result":"folded the event","num_turns":4,"session_id":"s1"}"#;
        let outcome = parse_outcome(stdout).expect("parse success envelope");
        assert!(outcome.ok);
        assert_eq!(outcome.status, HarnessStatus::Ok);
        assert_eq!(outcome.summary, "folded the event");
        assert_eq!(outcome.tool_calls, 4);
    }

    #[test]
    fn parses_error_envelope_into_failed_outcome() {
        let stdout = br#"{"type":"result","subtype":"error_during_execution",
            "is_error":true,"result":"","num_turns":1}"#;
        let outcome = parse_outcome(stdout).expect("parse error envelope");
        assert!(!outcome.ok);
        assert_eq!(outcome.status, HarnessStatus::Failed);
    }

    #[test]
    fn unparseable_stdout_is_a_bad_outcome_error() {
        let err = parse_outcome(b"not json at all").expect_err("must be BadOutcome");
        assert!(matches!(err, HarnessError::BadOutcome { .. }));
    }
}
