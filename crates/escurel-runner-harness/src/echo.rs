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
use escurel_runner_core::TaskContext;

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

    async fn run(&self, task: &TaskContext) -> Result<HarnessOutcome, HarnessError> {
        let payload = serde_json::to_vec(&HarnessTask::from_context(task)).map_err(|source| {
            HarnessError::BadOutcome {
                harness: NAME,
                source,
            }
        })?;

        let output = crate::harness::run_capture(crate::harness::Spawn {
            harness: NAME,
            bin: &self.bin_path,
            args: &[],
            envs: Vec::new(),
            // Streamed in, then closed so the harness's read-to-end completes.
            stdin: Some(&payload),
            timeout: self.timeout,
            cancel: task.cancel.as_ref(),
        })
        .await?;

        serde_json::from_slice::<HarnessOutcome>(&output.stdout).map_err(|source| {
            HarnessError::BadOutcome {
                harness: NAME,
                source,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::HarnessStatus;

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
            result_ref: None,
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
