//! The Claude Code CLI harness adapter (#152).
//!
//! [`ClaudeHarness`] is a real [`crate::Harness`]: it drives the `claude`
//! headless CLI (`claude -p`) as an isolated, timed, kill-on-drop
//! subprocess, registering the escurel gateway as an **HTTP MCP server**
//! (`--mcp-config`) with the packaged scoped bearer, narrowing the tool
//! surface (`--allowedTools mcp__escurel__<tool>`), injecting the skill body
//! as the appended system prompt, and capturing the final `result` envelope
//! of its `--output-format stream-json` output into a [`HarnessOutcome`].
//!
//! The session is stream-json both ways (`--input-format stream-json`): the
//! prompt is the first user message on stdin, and stdin stays open until
//! every user turn has produced its `result` line, so a further turn can be
//! written into the live session.
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

use std::time::Duration;

use async_trait::async_trait;
use escurel_runner_core::TaskContext;

use crate::harness::{Harness, HarnessError, HarnessOutcome, HarnessStatus, Usage};

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
            // `-p` with no inline value: the prompt is written to the child's
            // stdin instead, as a stream-json user message. Linux caps a SINGLE argv string at 32 pages
            // (MAX_ARG_STRLEN), independently of the 2 MB total ARG_MAX, so a
            // packaged transcript over ~128 KB made spawn fail with E2BIG and
            // the event permanently undispatchable. stdin has no such limit.
            "-p".to_owned(),
            "--append-system-prompt".to_owned(),
            task.instructions.clone(),
            "--mcp-config".to_owned(),
            mcp_config_path.to_owned(),
            // Only the escurel MCP server should be visible — never the
            // ambient user/project MCP config of whatever host runs the
            // runner.
            "--strict-mcp-config".to_owned(),
            // stream-json both ways: one JSON object per line, a user turn
            // in, the session's events and one `result` per turn out.
            // `-p` refuses stream-json output without `--verbose`.
            "--input-format".to_owned(),
            "stream-json".to_owned(),
            "--output-format".to_owned(),
            "stream-json".to_owned(),
            "--verbose".to_owned(),
            // Unattended runner: no interactive permission prompts — or, for
            // a planning run (workbench backend P2-5b), claude's own plan
            // mode, which reads and plans but never writes.
            "--permission-mode".to_owned(),
            if task.plan_mode {
                "plan".to_owned()
            } else {
                "bypassPermissions".to_owned()
            },
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

    /// Each injected message is one more stream-json user turn.
    fn supports_inject(&self) -> bool {
        true
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
        // Held for the attempt: a message sent between attempts waits for
        // the next one rather than being lost.
        let mut inbox = match &task.inject {
            Some(i) => Some(i.open_session().await),
            None => None,
        };

        let output = crate::harness::run_capture(crate::harness::Spawn {
            harness: NAME,
            bin: &self.bin_path,
            args: &args,
            envs: Vec::new(),
            stdin: Some(&user_message(&task.input)),
            stdout_lines: None,
            // Stdin stays open while a turn is in flight; claude exits at
            // the EOF sent once the last turn's `result` is out.
            turns: Some(crate::harness::Turns {
                inbox: inbox.as_mut(),
                encode: user_message,
                turn_ended: is_result_line,
            }),
            timeout: self.timeout,
            cancel: task.cancel.as_ref(),
        })
        .await?;

        parse_outcome(&output.stdout)
    }
}

/// One user turn as a stream-json input line.
fn user_message(text: &str) -> Vec<u8> {
    let mut line = serde_json::json!({
        "type": "user",
        "message": { "role": "user", "content": [{ "type": "text", "text": text }] },
    })
    .to_string()
    .into_bytes();
    line.push(b'\n');
    line
}

/// Whether a stream-json output line is a turn's `result` envelope.
fn is_result_line(line: &str) -> bool {
    // Cheap prefilter: most lines are assistant/tool events.
    line.contains("\"result\"")
        && serde_json::from_str::<serde_json::Value>(line)
            .is_ok_and(|v| v.get("type").and_then(serde_json::Value::as_str) == Some("result"))
}

/// Every turn's `result` envelope out of the stream-json output, in order.
/// A stdout that is one bare envelope (the `--output-format json` shape) is
/// read as-is, so a parse error still names what was wrong.
fn result_envelopes(stdout: &[u8]) -> Result<Vec<serde_json::Value>, HarnessError> {
    let text = String::from_utf8_lossy(stdout);
    let lines: Vec<&str> = text.lines().filter(|l| is_result_line(l)).collect();
    if lines.is_empty() {
        return serde_json::from_slice(stdout)
            .map(|v| vec![v])
            .map_err(|source| HarnessError::BadOutcome {
                harness: NAME,
                source,
            });
    }
    lines
        .into_iter()
        .map(|l| {
            serde_json::from_str(l).map_err(|source| HarnessError::BadOutcome {
                harness: NAME,
                source,
            })
        })
        .collect()
}

fn parse_outcome(stdout: &[u8]) -> Result<HarnessOutcome, HarnessError> {
    let envelopes = result_envelopes(stdout)?;
    let envelope = envelopes.last().expect("at least one envelope");

    let is_error = envelope
        .get("is_error")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let subtype = envelope
        .get("subtype")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let ok = !is_error && subtype != "error";

    // The final assistant message, whole: a chat run's answer. The summary
    // below is the same text cut for the Runs view.
    let reply = envelope
        .get("result")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let summary = reply
        .as_deref()
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
        reply: reply.filter(|_| ok),
        result_ref: None,
        usage: session_usage(&envelopes),
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

/// A session's usage over its turns' `result` envelopes, or `None` when no
/// turn reported any. Each envelope's `usage` is that turn's alone, so the
/// tokens add up; `total_cost_usd` and `modelUsage` are the session's
/// running totals, so they come from the last envelope that has them.
/// (Verified against a real two-turn stream-json session; see the test
/// `a_multi_turn_session_sums_per_turn_tokens_and_takes_the_running_cost`.)
fn session_usage(envelopes: &[serde_json::Value]) -> Option<Usage> {
    let mut total: Option<Usage> = None;
    for envelope in envelopes {
        let Some(usage) = envelope.get("usage").and_then(serde_json::Value::as_object) else {
            continue;
        };
        let count = |k: &str| {
            usage
                .get(k)
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0)
        };
        let t = total.get_or_insert_with(Usage::default);
        t.input_tokens += count("input_tokens")
            + count("cache_read_input_tokens")
            + count("cache_creation_input_tokens");
        t.output_tokens += count("output_tokens");
        if let Some(cost) = envelope
            .get("total_cost_usd")
            .and_then(serde_json::Value::as_f64)
        {
            t.cost_usd = Some(cost);
        }
        if let Some(model) = envelope
            .get("modelUsage")
            .and_then(serde_json::Value::as_object)
            .and_then(|m| m.keys().next().cloned())
        {
            t.model = Some(model);
        }
    }
    total
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

    /// A `TaskContext` whose packaged input is larger than a single argv
    /// string may be on Linux. A two-hour meeting transcript is 150-220 KB.
    fn big_task(input_bytes: usize) -> TaskContext {
        TaskContext::for_test(
            "INSTRUCTMARK skill body".to_owned(),
            "x".repeat(input_bytes),
            "http://127.0.0.1:8080/mcp".to_owned(),
            vec!["update_page".to_owned()],
            SecretString::from("scoped-bearer-XYZ".to_owned()),
        )
    }

    /// Linux caps a SINGLE argv string at 32 pages, independently of the 2 MB
    /// total `ARG_MAX`. Passing the packaged input as an argument therefore
    /// made every event with a body over ~128 KB permanently undispatchable:
    /// `spawn` failed with E2BIG, all three attempts, straight to the DLQ.
    /// Observed against a real 220 KB meeting transcript.
    const MAX_ARG_STRLEN: usize = 32 * 4096;

    #[test]
    fn a_prompt_too_large_for_argv_is_not_placed_in_argv() {
        let h = ClaudeHarness::new("claude");
        let big = big_task(220 * 1024);
        let args = h.build_args(&big, "/tmp/mcp.json");

        for (i, a) in args.iter().enumerate() {
            assert!(
                a.len() < MAX_ARG_STRLEN,
                "argv[{i}] is {} bytes, over the {MAX_ARG_STRLEN}-byte per-argument \
                 limit; spawn would fail with E2BIG",
                a.len()
            );
        }
    }

    #[test]
    fn argv_carries_print_prompt_system_prompt_and_json_output() {
        let h = ClaudeHarness::new("claude").with_model(Some("opus".to_owned()));
        let args = h.build_args(&task(), "/tmp/mcp.json");

        // Headless print mode. The prompt itself goes on stdin, not argv —
        // see `a_prompt_too_large_for_argv_is_not_placed_in_argv`.
        assert!(args.iter().any(|a| a == "-p"), "-p present");
        assert!(
            !args.iter().any(|a| a == "INPUTMARK fold this"),
            "the input must NOT be an argv element"
        );

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
        assert_eq!(args[of + 1], "stream-json");
        let inf = args
            .iter()
            .position(|a| a == "--input-format")
            .expect("--input-format present");
        assert_eq!(args[inf + 1], "stream-json");
        assert!(
            args.iter().any(|a| a == "--verbose"),
            "-p needs it for stream-json"
        );
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
    fn parses_usage_cost_and_model_off_the_envelope() {
        let stdout = br#"{"type":"result","subtype":"success","is_error":false,
            "result":"done","num_turns":2,"total_cost_usd":0.0412,
            "usage":{"input_tokens":12,"cache_creation_input_tokens":300,
                     "cache_read_input_tokens":1000,"output_tokens":88},
            "modelUsage":{"claude-sonnet-5":{"inputTokens":12,"outputTokens":88}}}"#;
        let usage = parse_outcome(stdout).expect("parse").usage.expect("usage");
        assert_eq!(
            usage.input_tokens, 1312,
            "fresh + cache-read + cache-creation"
        );
        assert_eq!(usage.output_tokens, 88);
        assert_eq!(usage.cost_usd, Some(0.0412));
        assert_eq!(usage.model.as_deref(), Some("claude-sonnet-5"));
    }

    #[test]
    fn an_envelope_without_usage_reports_none() {
        let stdout = br#"{"type":"result","subtype":"success","is_error":false,
            "result":"done","num_turns":1}"#;
        assert_eq!(parse_outcome(stdout).expect("parse").usage, None);
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
    fn the_last_result_line_of_a_stream_is_the_outcome() {
        let stdout = br#"{"type":"system","subtype":"init","session_id":"s1"}
{"type":"assistant","message":{"content":[{"type":"text","text":"first"}]}}
{"type":"result","subtype":"success","is_error":false,"result":"first","num_turns":1}
{"type":"assistant","message":{"content":[{"type":"text","text":"steered"}]}}
{"type":"result","subtype":"success","is_error":false,"result":"steered answer","num_turns":3,"usage":{"input_tokens":5,"output_tokens":7}}
"#;
        let outcome = parse_outcome(stdout).expect("parse stream");
        assert!(outcome.ok);
        assert_eq!(outcome.summary, "steered answer");
        assert_eq!(outcome.tool_calls, 3);
        assert_eq!(outcome.usage.expect("usage").output_tokens, 7);
    }

    /// Recorded from a real two-turn `claude -p --input-format stream-json`
    /// session (claude-code 2.x, 2026-11): each turn's `result` carries that
    /// turn's `usage` (turn 2's output 49 is its own message, not 80 + 49),
    /// while `total_cost_usd` and `modelUsage` are the session's running
    /// totals. Tokens add up across the results; the cost is the last one.
    #[test]
    fn a_multi_turn_session_sums_per_turn_tokens_and_takes_the_running_cost() {
        let stdout = br#"{"type":"system","subtype":"init","session_id":"s1","model":"claude-haiku-5-5"}
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"echo hi"}}]}}
{"type":"user","message":{"content":[{"tool_use_id":"t1","type":"tool_result","content":"hi","is_error":false}]}}
{"type":"assistant","message":{"content":[{"type":"text","text":"one"}]}}
{"type":"result","subtype":"success","is_error":false,"result":"one","num_turns":2,"total_cost_usd":0.20389395,"usage":{"input_tokens":4,"cache_creation_input_tokens":195273,"cache_read_input_tokens":168379,"output_tokens":80},"modelUsage":{"claude-haiku-5-5":{"inputTokens":4,"outputTokens":80,"costUSD":0.20389395}}}
{"type":"system","subtype":"init","session_id":"s1","model":"claude-haiku-5-5"}
{"type":"assistant","message":{"content":[{"type":"text","text":"Two is prime."}]}}
{"type":"result","subtype":"success","is_error":false,"result":"Two is prime.","num_turns":1,"total_cost_usd":0.2136006,"usage":{"input_tokens":2,"cache_creation_input_tokens":60,"cache_read_input_tokens":190463,"output_tokens":49},"modelUsage":{"claude-haiku-5-5":{"inputTokens":6,"outputTokens":129,"costUSD":0.2136006}}}
"#;
        let usage = parse_outcome(stdout).expect("parse").usage.expect("usage");
        assert_eq!(
            usage.input_tokens,
            (4 + 195_273 + 168_379) + (2 + 60 + 190_463)
        );
        assert_eq!(usage.output_tokens, 80 + 49);
        assert_eq!(usage.cost_usd, Some(0.2136006), "the running total, once");
        assert_eq!(usage.model.as_deref(), Some("claude-haiku-5-5"));
    }

    #[test]
    fn a_turn_is_a_stream_json_user_message_line() {
        let line = user_message("fold \"this\"\nplease");
        assert_eq!(line.last(), Some(&b'\n'), "newline-terminated");
        let v: serde_json::Value = serde_json::from_slice(&line).unwrap();
        assert_eq!(v["type"], "user");
        assert_eq!(v["message"]["role"], "user");
        assert_eq!(v["message"]["content"][0]["text"], "fold \"this\"\nplease");
        assert!(is_result_line(r#"{"type":"result","result":"x"}"#));
        assert!(!is_result_line(
            r#"{"type":"assistant","message":{"content":"the \"result\" is"}}"#
        ));
    }

    #[test]
    fn unparseable_stdout_is_a_bad_outcome_error() {
        let err = parse_outcome(b"not json at all").expect_err("must be BadOutcome");
        assert!(matches!(err, HarnessError::BadOutcome { .. }));
    }
}
