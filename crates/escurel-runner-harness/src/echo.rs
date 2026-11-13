//! The echo harness adapter (#151).
//!
//! [`EchoHarness`] is a real [`crate::Harness`]: it spawns the
//! `escurel-echo-harness` binary as an isolated, timed, kill-on-drop
//! subprocess, writes the packaged [`HarnessTask`] to its stdin, and parses
//! the [`HarnessOutcome`] the binary prints to stdout. The binary itself
//! makes the real `/mcp` `update_page` + `assign_event` calls — the adapter
//! performs **no** escurel writes, keeping "skills as instructions + tools"
//! honest.
//!
//! The echo harness is deterministic: it is the test stand-in for an LLM,
//! but its escurel effects are 100% real. The richer Claude/Codex/Gemini
//! adapters (#152-154) reuse this exact spawn-and-capture shape.

use std::time::Duration;

use async_trait::async_trait;
use escurel_runner_core::{TaskContext, TranscriptEntry, TranscriptKind};

use crate::harness::{Harness, HarnessError, HarnessOutcome};
use crate::task::HarnessTask;

/// The adapter's stable name — the `ESCUREL_RUNNER_HARNESS=echo` selector
/// and the value reported by [`Harness::name`].
const NAME: &str = "echo";

/// Default per-run timeout. The echo harness makes two `/mcp` calls against
/// a local gateway, so this is generous; deployments may shorten it.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Adapter that spawns the real `escurel-echo-harness` subprocess.
#[derive(Debug, Clone)]
pub struct EchoHarness {
    /// Path to the `escurel-echo-harness` binary to spawn.
    bin_path: String,
    /// Per-run timeout; the child is killed (kill-on-drop) if it overruns.
    timeout: Duration,
}

impl EchoHarness {
    /// Build an adapter that launches the echo-harness binary at `bin_path`.
    pub fn new(bin_path: impl Into<String>) -> Self {
        Self {
            bin_path: bin_path.into(),
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// Override the per-run timeout (tests use a small value).
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

#[async_trait]
impl Harness for EchoHarness {
    fn name(&self) -> &str {
        NAME
    }

    /// Injected messages arrive as further stdin lines while the echo
    /// harness idles (`ESCUREL_ECHO_SLEEP_MS`); it folds them into its note.
    fn supports_inject(&self) -> bool {
        true
    }

    async fn run(&self, task: &TaskContext) -> Result<HarnessOutcome, HarnessError> {
        let mut payload =
            serde_json::to_vec(&HarnessTask::from_context(task)).map_err(|source| {
                HarnessError::BadOutcome {
                    harness: NAME,
                    source,
                }
            })?;
        let mut inbox = match &task.inject {
            Some(i) => Some(i.open_session().await),
            None => None,
        };
        // With an inbox the task is one line and stdin stays open for the
        // turns that follow, each a JSON string line; the binary is told so,
        // because without it it reads the task to EOF.
        let (envs, turns) = match inbox.as_mut() {
            Some(rx) => {
                payload.push(b'\n');
                (
                    vec![(ECHO_STDIN_TURNS_ENV, std::ffi::OsString::from("1"))],
                    Some(crate::harness::Turns {
                        inbox: Some(rx),
                        encode: json_line,
                        // The echo harness decides when it is done: it reads
                        // turns until it acts, then exits.
                        turn_ended: |_| false,
                    }),
                )
            }
            None => (Vec::new(), None),
        };

        let lines = crate::harness::LineLog::new();
        let output = crate::harness::run_capture(crate::harness::Spawn {
            harness: NAME,
            bin: &self.bin_path,
            args: &[],
            envs,
            stdin: Some(&payload),
            stdout_lines: Some(lines.sender()),
            turns,
            timeout: self.timeout,
            cancel: task.cancel.as_ref(),
        })
        .await;
        lines.transcribe(task, transcript_entries);

        parse_outcome(&output?.stdout)
    }
}

/// The outcome is the binary's LAST stdout line; scripted transcript lines
/// (`ESCUREL_ECHO_TRANSCRIPT`) come before it.
fn parse_outcome(stdout: &[u8]) -> Result<HarnessOutcome, HarnessError> {
    let text = String::from_utf8_lossy(stdout);
    let last = text
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("");
    serde_json::from_str::<HarnessOutcome>(last).map_err(|source| HarnessError::BadOutcome {
        harness: NAME,
        source,
    })
}

/// Makes the echo binary print each line of this JSONL file to stdout
/// before it works (`{token}` in a line becomes the run's bearer), so a test
/// scripts the stream the transcript is built from.
pub const ECHO_TRANSCRIPT_ENV: &str = "ESCUREL_ECHO_TRANSCRIPT";

/// One echo stdout line as transcript entries: a scripted
/// `{"transcript": {kind, name?, args?, text?}}` line is one entry; the
/// outcome line and anything else yield none.
pub(crate) fn transcript_entries(line: &str) -> Vec<TranscriptEntry> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
        return Vec::new();
    };
    let Some(t) = v.get("transcript") else {
        return Vec::new();
    };
    let Ok(kind) = serde_json::from_value::<TranscriptKind>(t["kind"].clone()) else {
        return Vec::new();
    };
    let mut entry = TranscriptEntry::new(kind, t["text"].as_str().unwrap_or(""));
    entry.name = t["name"].as_str().map(str::to_owned);
    entry.args = t
        .get("args")
        .cloned()
        .filter(|_| kind == TranscriptKind::ToolCall);
    vec![entry]
}

/// Tells the echo binary its stdin is a conversation: the task on the first
/// line, then one JSON string per injected turn.
pub const ECHO_STDIN_TURNS_ENV: &str = "ESCUREL_ECHO_STDIN_TURNS";

/// One turn as a JSON string line — a message may hold newlines.
fn json_line(text: &str) -> Vec<u8> {
    let mut line = serde_json::to_vec(text).unwrap_or_default();
    line.push(b'\n');
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::HarnessStatus;

    #[test]
    fn scripted_lines_normalise_and_the_outcome_is_the_last_line() {
        let stdout =
            br#"{"transcript":{"kind":"tool_call","name":"update_page","args":{"page_id":"p"}}}
{"transcript":{"kind":"error","text":"boom"}}
{"transcript":{"kind":"bogus"}}
{"ok":true,"status":"ok","summary":"folded","tool_calls":2,"produced_instance":null}
"#;
        let text = String::from_utf8_lossy(stdout);
        let entries: Vec<TranscriptEntry> = text.lines().flat_map(transcript_entries).collect();
        assert_eq!(entries.len(), 2, "{entries:?}");
        assert_eq!(entries[0].kind, TranscriptKind::ToolCall);
        assert_eq!(entries[0].args, Some(serde_json::json!({"page_id": "p"})));
        assert_eq!(entries[1].kind, TranscriptKind::Error);
        assert_eq!(entries[1].text, "boom");
        let outcome = parse_outcome(stdout).expect("last line");
        assert_eq!(outcome.summary, "folded");
        assert_eq!(outcome.tool_calls, 2, "echo reports its own real calls");
    }

    #[test]
    fn name_is_echo() {
        assert_eq!(EchoHarness::new("escurel-echo-harness").name(), "echo");
    }

    /// The `HarnessOutcome` JSON is the wire contract between the
    /// echo-harness binary (which prints it) and the adapter (which parses
    /// it); pin it so a field rename can't silently break the subprocess
    /// boundary.
    #[test]
    fn outcome_wire_contract_round_trips() {
        let outcome = HarnessOutcome {
            reply: None,
            result_ref: None,
            usage: None,
            ok: true,
            status: HarnessStatus::Ok,
            summary: "folded".to_owned(),
            tool_calls: 4,
            produced_instance: Some("markdown/instances/customer/globex.md".to_owned()),
        };
        let json = serde_json::to_string(&outcome).expect("serialize");
        assert!(
            json.contains("\"status\":\"ok\""),
            "snake_case status: {json}"
        );
        let back: HarnessOutcome = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, outcome);
    }
}
