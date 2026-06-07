//! The [`Harness`] adapter trait + its structured outcome/error types.

use async_trait::async_trait;
use escurel_runner_core::TaskContext;

/// Terminal status of a harness run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessStatus {
    /// The harness completed and its escurel writes were confirmed.
    Ok,
    /// The harness ran but reported a non-fatal failure (its work did not
    /// complete). The reconciler (#155) decides retry-vs-dead.
    Failed,
}

/// The structured result of one harness run.
///
/// Captured by the adapter from the harness subprocess's exit + stdout and
/// handed back to the runner's reconciler. Enough for the minimal reconcile
/// in #151 and the richer retry/cascade logic in #155+ to act on without a
/// second round-trip.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HarnessOutcome {
    /// Whether the run succeeded.
    pub ok: bool,
    /// Terminal status (mirrors [`HarnessOutcome::ok`] but names the
    /// failure mode explicitly for the reconciler).
    pub status: HarnessStatus,
    /// A short human-readable summary of what the harness did.
    pub summary: String,
    /// How many `/mcp` tool calls the harness made.
    pub tool_calls: u32,
    /// The instance page the harness wrote to, if it materialised/updated
    /// one. `None` when the run made no instance write. The reconciler reads
    /// this to confirm the produced state.
    pub produced_instance: Option<String>,
}

/// Errors raised by a harness adapter while managing its subprocess.
///
/// These are *adapter-level* failures (could not spawn, timed out, the
/// child crashed, its stdout was unparseable) — distinct from a harness that
/// ran cleanly but reported [`HarnessStatus::Failed`] in its
/// [`HarnessOutcome`].
#[derive(Debug, thiserror::Error)]
pub enum HarnessError {
    /// The harness subprocess could not be spawned.
    #[error("could not spawn harness {harness:?} at {path:?}: {source}")]
    Spawn {
        /// The adapter name.
        harness: &'static str,
        /// The binary path the adapter tried to launch.
        path: String,
        /// The underlying OS error.
        #[source]
        source: std::io::Error,
    },
    /// The harness ran past its deadline and was killed.
    #[error("harness {harness:?} timed out after {timeout_ms}ms")]
    Timeout {
        /// The adapter name.
        harness: &'static str,
        /// The configured timeout.
        timeout_ms: u64,
    },
    /// The harness exited non-zero (and emitted no parseable outcome).
    #[error("harness {harness:?} exited with status {code:?}: {stderr}")]
    NonZeroExit {
        /// The adapter name.
        harness: &'static str,
        /// The process exit code (`None` if killed by a signal).
        code: Option<i32>,
        /// Captured stderr (truncated) for diagnosis.
        stderr: String,
    },
    /// The harness stdout was not the expected JSON outcome.
    #[error("could not parse harness {harness:?} outcome: {source}")]
    BadOutcome {
        /// The adapter name.
        harness: &'static str,
        /// The JSON parse error.
        #[source]
        source: serde_json::Error,
    },
    /// An I/O error writing the task to / reading the result from the child.
    #[error("harness {harness:?} I/O error: {source}")]
    Io {
        /// The adapter name.
        harness: &'static str,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },
}

/// A harness adapter: spawns a real agent harness as a subprocess, points
/// it at the gateway `/mcp` with the packaged scoped token + skill
/// instructions, and captures a structured [`HarnessOutcome`].
///
/// Implementors **must not** write to escurel directly — every escurel
/// effect flows through the harness's own `/mcp` tool calls. The adapter is
/// only process management + outcome capture.
#[async_trait]
pub trait Harness: Send + Sync {
    /// The adapter's stable name (`"echo"`, later `"claude"` / `"codex"` /
    /// `"adk"`). Used for harness selection (`ESCUREL_RUNNER_HARNESS`) and
    /// logging.
    fn name(&self) -> &str;

    /// Run the packaged task: spawn the harness subprocess, wait (with a
    /// timeout + kill-on-drop), and return its captured outcome.
    async fn run(&self, task: &TaskContext) -> Result<HarnessOutcome, HarnessError>;
}
