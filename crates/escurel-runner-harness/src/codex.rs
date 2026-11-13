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

use std::time::Duration;

use async_trait::async_trait;
use escurel_runner_core::{TaskContext, TranscriptEntry, TranscriptKind};

use crate::harness::{Harness, HarnessError, HarnessOutcome, HarnessStatus, Usage};

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

    /// Assemble the full `codex` argv (excluding the binary itself). Split
    /// out so the deterministic test can assert the invocation shape without
    /// spawning. The task itself no longer appears here — the prompt travels
    /// on stdin — but the parameter is kept so the signature stays parallel
    /// to the other adapters and the argv-size test can pass a big task.
    fn build_args(&self, _task: &TaskContext, output_last_message_path: &str) -> Vec<String> {
        let mut args: Vec<String> = vec![
            "exec".to_owned(),
            // `-` in the PROMPT position means "read the prompt from stdin".
            // Linux caps a SINGLE argv string at 32 pages (MAX_ARG_STRLEN),
            // independently of the 2 MB total ARG_MAX, so a packaged
            // transcript over ~128 KB made spawn fail with E2BIG and left the
            // event permanently undispatchable.
            "-".to_owned(),
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
        if task.plan_mode {
            // No plan-only mode to hand this harness (workbench backend
            // P2-5b, OQ-3: refuse rather than run a narrowed surface it
            // does not enforce).
            return Err(HarnessError::Unsupported {
                harness: NAME,
                reason: "plan mode is not supported by this harness; use claude, gemini or echo"
                    .to_owned(),
            });
        }
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

        let lines = crate::harness::LineLog::new();
        let output = crate::harness::run_capture(crate::harness::Spawn {
            harness: NAME,
            bin: &self.bin_path,
            args: &args,
            // Point codex at the per-run config and hand it the bearer
            // through the env var its config names — out of argv and off
            // disk.
            envs: vec![
                ("CODEX_HOME", codex_home.path().into()),
                (BEARER_ENV_VAR, task.token_str().into()),
            ],
            // codex reads the prompt until stdin closes.
            stdin: Some(Self::build_prompt(task).as_bytes()),
            stdout_lines: Some(lines.sender()),
            turns: None,
            timeout: self.timeout,
            cancel: task.cancel.as_ref(),
        })
        .await;
        lines.transcribe(task, transcript_entries);
        let output = output?;

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
    // `turn.completed` events carry the turn's `usage`; the run's is the sum.
    let mut usage: Option<Usage> = None;
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
        for entry in event_entries(&event) {
            match entry.kind {
                TranscriptKind::ToolCall => tool_calls += 1,
                // A run-level error event; a refused tool call (named) is
                // the model's to recover from, not the run's verdict.
                TranscriptKind::Error if entry.name.is_none() => saw_error = true,
                _ => {}
            }
        }
        if event["type"] == "turn.completed"
            && let Some(u) = event.get("usage").and_then(serde_json::Value::as_object)
        {
            let count = |k: &str| u.get(k).and_then(serde_json::Value::as_u64).unwrap_or(0);
            // `cached_input_tokens` is the cached SUBSET of `input_tokens`,
            // not an addition to it (codex second-opinion review of P3).
            usage.get_or_insert_with(Usage::default).accumulate(&Usage {
                input_tokens: count("input_tokens"),
                output_tokens: count("output_tokens"),
                billed_tokens: None,
                cost_usd: None,
                model: None,
            });
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
        reply: None,
        result_ref: None,
        usage,
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

/// One codex `--json` stdout line as transcript entries; banner lines and
/// anything else that is not a JSON object yield none.
pub(crate) fn transcript_entries(line: &str) -> Vec<TranscriptEntry> {
    serde_json::from_str::<serde_json::Value>(line.trim())
        .map(|event| event_entries(&event))
        .unwrap_or_default()
}

/// One codex event as transcript entries — the one parser of the codex
/// stream: the outcome's `tool_calls` and its error verdict count these.
///
/// - an MCP tool-call event ([`is_tool_call_event`]) is a tool call (its
///   `item.tool`, `item.arguments`), followed by a tool result when the
///   item carries its `result` or an error when it carries an `error`;
/// - an error event ([`is_error_event`]) is an error (its `message`);
/// - an `agent_message` item is a message (its `text`).
fn event_entries(event: &serde_json::Value) -> Vec<TranscriptEntry> {
    let item = &event["item"];
    if is_tool_call_event(event) {
        let name = item["tool"].as_str().unwrap_or("unknown").to_owned();
        let mut out = vec![TranscriptEntry::tool_call(
            name.clone(),
            item.get("arguments").cloned().unwrap_or_default(),
        )];
        if let Some(err) = item.get("error").filter(|e| !e.is_null()) {
            out.push(TranscriptEntry::new(TranscriptKind::Error, preview(err)).named(name));
        } else if let Some(result) = item.get("result").filter(|r| !r.is_null()) {
            out.push(TranscriptEntry::new(TranscriptKind::ToolResult, preview(result)).named(name));
        }
        return out;
    }
    if is_error_event(event) {
        let text = event["message"]
            .as_str()
            .or_else(|| event["error"]["message"].as_str())
            .unwrap_or("codex error event");
        return vec![TranscriptEntry::new(TranscriptKind::Error, text)];
    }
    let item_type = item["type"].as_str().or_else(|| item["item_type"].as_str());
    if item_type == Some("agent_message")
        && let Some(text) = item["text"].as_str().filter(|t| !t.trim().is_empty())
    {
        return vec![TranscriptEntry::new(TranscriptKind::Message, text)];
    }
    Vec::new()
}

/// A JSON value as preview text: a string as-is, anything else compact.
fn preview(v: &serde_json::Value) -> String {
    v.as_str().map_or_else(|| v.to_string(), str::to_owned)
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

    /// `codex exec` reads one prompt and has no way to take a follow-up
    /// turn into the live session (`exec resume` is a new process), so an
    /// injection into a codex run is answered `unsupported` (#820).
    #[test]
    fn codex_cannot_take_an_injected_message() {
        assert!(!CodexHarness::new("codex").supports_inject());
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

    /// Same per-argument ceiling as the Claude adapter. `codex exec` reads
    /// the prompt from stdin when `-` is given in its place, so the packaged
    /// input never has to fit in an argv string.
    #[test]
    fn a_prompt_too_large_for_argv_is_not_placed_in_argv() {
        const MAX_ARG_STRLEN: usize = 32 * 4096;
        let h = CodexHarness::new("codex");
        let big = TaskContext::for_test(
            "INSTRUCTMARK skill body".to_owned(),
            "x".repeat(220 * 1024),
            "http://127.0.0.1:8080/mcp".to_owned(),
            vec!["update_page".to_owned()],
            SecretString::from("scoped-bearer-XYZ".to_owned()),
        );
        let args = h.build_args(&big, "/tmp/msg.txt");
        for (i, a) in args.iter().enumerate() {
            assert!(
                a.len() < MAX_ARG_STRLEN,
                "argv[{i}] is {} bytes, over the per-argument limit; spawn \
                 would fail with E2BIG",
                a.len()
            );
        }
        assert!(
            args.iter().any(|a| a == "-"),
            "`-` marks the prompt as coming from stdin: {args:?}"
        );
    }

    #[test]
    fn argv_carries_exec_json_output_skip_git_and_bypass() {
        let h = CodexHarness::new("codex").with_model(Some("o3".to_owned()));
        let args = h.build_args(&task(), "/tmp/msg.txt");

        assert_eq!(args.first().map(String::as_str), Some("exec"));
        // The prompt is read from stdin, so `-` stands in its place and the
        // input must not appear in argv at all.
        assert_eq!(args.get(1).map(String::as_str), Some("-"));
        assert!(
            !args.iter().any(|a| a.contains("INPUTMARK")),
            "the input must NOT be an argv element: {args:?}"
        );
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
    fn turn_usage_is_summed_and_absent_usage_is_none() {
        let jsonl = br#"{"type":"turn.completed","usage":{"input_tokens":100,"cached_input_tokens":40,"output_tokens":9}}
{"type":"turn.completed","usage":{"input_tokens":50,"cached_input_tokens":0,"output_tokens":11}}"#;
        let usage = parse_outcome("ok", jsonl)
            .expect("parse")
            .usage
            .expect("usage");
        assert_eq!(
            (usage.input_tokens, usage.output_tokens),
            (150, 20),
            "cached input is a subset of input, never added to it"
        );
        assert_eq!(usage.cost_usd, None);
        assert_eq!(
            parse_outcome("ok", br#"{"type":"turn.completed"}"#)
                .expect("parse")
                .usage,
            None
        );
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
    fn the_event_stream_normalises_to_calls_results_messages_and_errors() {
        let lines = [
            "Reading prompt from stdin...",
            r#"{"type":"item.completed","item":{"id":"i1","type":"mcp_tool_call","item_type":"mcp_tool_call","server":"escurel","tool":"update_page","arguments":{"page_id":"p","content":"x"},"result":{"content":[{"type":"text","text":"ok"}]},"error":null}}"#,
            r#"{"type":"item.completed","item":{"item_type":"mcp_tool_call","tool":"assign_event","arguments":{},"error":{"message":"forbidden"}}}"#,
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"folded"}}"#,
            r#"{"type":"error","message":"model refused"}"#,
        ];
        let entries: Vec<TranscriptEntry> =
            lines.iter().flat_map(|l| transcript_entries(l)).collect();
        let kinds: Vec<TranscriptKind> = entries.iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds,
            [
                TranscriptKind::ToolCall,
                TranscriptKind::ToolResult,
                TranscriptKind::ToolCall,
                TranscriptKind::Error,
                TranscriptKind::Message,
                TranscriptKind::Error,
            ]
        );
        assert_eq!(entries[0].name.as_deref(), Some("update_page"));
        assert_eq!(
            entries[0].args,
            Some(serde_json::json!({"page_id":"p","content":"x"}))
        );
        assert_eq!(entries[3].name.as_deref(), Some("assign_event"));
        assert_eq!(entries[5].text, "model refused");
        let outcome = parse_outcome("", lines.join("\n").as_bytes()).expect("parse");
        assert_eq!(outcome.tool_calls, 2, "counted by the same parser");
        assert!(!outcome.ok, "the run-level error event fails the run");
        let refused_call_only = parse_outcome("", lines[2].as_bytes()).expect("parse");
        assert!(refused_call_only.ok, "a refused tool call alone does not");
    }

    #[test]
    fn non_json_banner_lines_are_tolerated() {
        let jsonl = b"Reading prompt from stdin...\n{\"type\":\"turn.completed\"}\nDone.\n";
        let outcome = parse_outcome("ok", jsonl).expect("parse");
        assert!(outcome.ok);
        assert_eq!(outcome.tool_calls, 0);
    }
}
