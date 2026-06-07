//! The Google ADK (adk-rust) harness adapter (#154).
//!
//! [`AdkHarness`] is a real [`crate::Harness`]: it drives an **external
//! adk-rust runner binary** as an isolated, timed, kill-on-drop subprocess,
//! delivers the packaged [`TaskContext`] to it over a small documented I/O
//! contract (a token-less [`AdkTask`] JSON on the child's stdin + the scoped
//! bearer out-of-band in an environment variable, mirroring the Codex
//! adapter), and parses the [`crate::HarnessOutcome`] JSON the runner prints
//! to stdout.
//!
//! Per the contract, the adapter performs **no** escurel writes — every
//! escurel effect flows through the runner's own `/mcp` tool calls under the
//! scoped token. The adapter is process management + invocation construction
//! + outcome capture, nothing more.
//!
//! ## Why an external binary, not in-tree adk-rust (deviation from the issue)
//!
//! The issue text specced *"a thin **Python** ADK runner script (shipped with
//! the crate) … via ADK's `MCPToolset` … → behind a feature flag."* That is
//! **superseded**: Python `google.adk` is not installed on the target
//! machine, and the chosen ADK example is DataZoo's **adk-rust**
//! `datazoo-agent-template`. That template deliberately keeps `adk-rust` +
//! its bundled `duckdb` in its **own standalone workspace** because they pull
//! a large native dependency tree. Vendoring `adk-rust` into escurel's
//! workspace would bloat *every* build and break the runner crates' deliberate
//! independence (they depend only on `escurel-client` + `escurel-types`).
//!
//! So this adapter follows the **exact same spawn-an-external-binary-by-path**
//! pattern as the Claude (#152) and Codex (#153) adapters: the heavy adk-rust
//! runtime lives in an **external** runner binary (built from
//! `datazoo-agent-template`), which the adapter spawns by configurable path
//! (`ESCUREL_RUNNER_ADK_BIN`). That mirrors how the template itself isolates
//! adk-rust in a standalone workspace. **No cargo feature flag is needed** —
//! the original "behind a feature flag" wording was for the in-tree Python
//! runtime we do not have; this adapter is just a subprocess spawn, so it adds
//! no heavy deps to gate.
//!
//! ## Runner I/O contract
//!
//! An adk-rust runner that this adapter can drive must:
//! - read a JSON [`AdkTask`] on **stdin** — `{ instructions, input,
//!   mcp_endpoint, allowed_tools }`. `instructions` is the skill body (the
//!   adk-rust `LlmAgent`'s `.instruction(...)`); `mcp_endpoint` is the escurel
//!   gateway `/mcp` URL the runner registers as a streamable-HTTP
//!   `MCPToolset`; `allowed_tools` narrows the escurel tool surface;
//! - read the scoped escurel bearer from the **`ESCUREL_MCP_BEARER`**
//!   environment variable (NOT from stdin/argv — keeps the token out of the
//!   process table and any captured payload) and send it as
//!   `Authorization: Bearer <token>` on the `/mcp` `MCPToolset`;
//! - optionally read `LLM_PROVIDER` / the provider key (`GEMINI_API_KEY`,
//!   …) and `LLM_MODEL` (set from [`AdkHarness::with_model`]) for the brain;
//! - perform the fold via real `/mcp` tool calls, then print a single
//!   [`crate::HarnessOutcome`] JSON object on **stdout** and exit `0`. A
//!   non-zero exit (with a stderr message) marks an adapter-level failure.
//!
//! The `adk` binary path is configurable (`ESCUREL_RUNNER_ADK_BIN`, default
//! `datazoo-agent-adk-runner` — no such binary exists on `PATH` by default,
//! so a deployment MUST point this at its built runner) so the always-on
//! deterministic DoD test can point it at a real scripted runner that performs
//! the real `/mcp` fold. The live test (a real adk-rust `LlmAgent` against a
//! real `/mcp`) runs on demand behind `#[ignore]`.

use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use escurel_runner_core::TaskContext;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

use crate::harness::{Harness, HarnessError, HarnessOutcome};

/// The adapter's stable name — the `ESCUREL_RUNNER_HARNESS=adk` selector and
/// the value reported by [`Harness::name`].
const NAME: &str = "adk";

/// The environment variable the adapter delivers the scoped bearer through.
/// The adk-rust runner reads it and sets `Authorization: Bearer <token>` on
/// its `/mcp` `MCPToolset`. Keeping the token in the child's env (not stdin /
/// argv) keeps it out of the process table and any captured task payload —
/// the same out-of-band delivery the Codex adapter uses.
const BEARER_ENV_VAR: &str = "ESCUREL_MCP_BEARER";

/// The environment variable carrying the optional LLM model id to the runner
/// (the adk-rust template reads `LLM_MODEL`).
const MODEL_ENV_VAR: &str = "LLM_MODEL";

/// Default per-run timeout. A real LLM run makes several `/mcp` round-trips
/// and one or more model turns, so this is generous; deployments may tune it.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);

/// The token-less task projection the adapter writes to the runner's stdin.
///
/// It deliberately omits the bearer (delivered via [`BEARER_ENV_VAR`]); the
/// runner builds its `/mcp` `MCPToolset` from `mcp_endpoint` + that env var,
/// uses `instructions` as the agent instruction, and folds `input`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AdkTask {
    /// The agent's instructions: the skill body + task framing + event. The
    /// runner uses this as the adk-rust `LlmAgent`'s `.instruction(...)`.
    pub instructions: String,
    /// The agent's input: the event payload + the target instance's state.
    pub input: String,
    /// The escurel gateway `/mcp` endpoint the runner registers as a
    /// streamable-HTTP `MCPToolset`.
    pub mcp_endpoint: String,
    /// The narrowed escurel tool surface the run may call.
    pub allowed_tools: Vec<String>,
}

impl AdkTask {
    /// Project a [`TaskContext`] into the token-less stdin payload. The
    /// scoped bearer is intentionally NOT included here — it rides in
    /// [`BEARER_ENV_VAR`].
    fn from_context(ctx: &TaskContext) -> Self {
        Self {
            instructions: ctx.instructions.clone(),
            input: ctx.input.clone(),
            mcp_endpoint: ctx.mcp_endpoint.clone(),
            allowed_tools: ctx.allowed_tools.clone(),
        }
    }
}

/// Adapter that drives an external adk-rust runner binary as a subprocess.
#[derive(Debug, Clone)]
pub struct AdkHarness {
    /// Path to the adk-rust runner binary to spawn (default
    /// `datazoo-agent-adk-runner`; a test points this at a real scripted
    /// runner via `ESCUREL_RUNNER_ADK_BIN`).
    bin_path: String,
    /// Optional model id passed to the runner via [`MODEL_ENV_VAR`] (config
    /// `ESCUREL_RUNNER_ADK_MODEL`); `None` lets the runner pick its default.
    model: Option<String>,
    /// Per-run timeout; the child is killed (kill-on-drop) if it overruns.
    timeout: Duration,
}

impl AdkHarness {
    /// Build an adapter that launches the adk-rust runner binary at `bin_path`.
    pub fn new(bin_path: impl Into<String>) -> Self {
        Self {
            bin_path: bin_path.into(),
            model: None,
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// Set the LLM model id passed to the runner via [`MODEL_ENV_VAR`] (config
    /// `ESCUREL_RUNNER_ADK_MODEL`). An empty/absent value is ignored.
    pub fn with_model(mut self, model: Option<String>) -> Self {
        self.model = model.filter(|m| !m.is_empty());
        self
    }

    /// Override the per-run timeout (tests use a small value).
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

#[async_trait]
impl Harness for AdkHarness {
    fn name(&self) -> &str {
        NAME
    }

    async fn run(&self, task: &TaskContext) -> Result<HarnessOutcome, HarnessError> {
        let payload = serde_json::to_vec(&AdkTask::from_context(task)).map_err(|source| {
            HarnessError::BadOutcome {
                harness: NAME,
                source,
            }
        })?;

        // kill_on_drop ties the child's lifetime to this future: a dropped
        // adapter (panic, cancellation, timeout) reaps the subprocess.
        let mut command = tokio::process::Command::new(&self.bin_path);
        command
            // The scoped bearer rides out-of-band in the env var the runner
            // reads (never on stdin / in argv), mirroring the Codex adapter.
            .env(BEARER_ENV_VAR, task.token_str())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(model) = &self.model {
            command.env(MODEL_ENV_VAR, model);
        }

        let mut child = command.spawn().map_err(|source| HarnessError::Spawn {
            harness: NAME,
            path: self.bin_path.clone(),
            source,
        })?;

        // Stream the token-less task into the child's stdin, then close it so
        // the runner's read-to-end completes.
        {
            let mut stdin = child.stdin.take().expect("stdin was piped");
            stdin
                .write_all(&payload)
                .await
                .map_err(|source| HarnessError::Io {
                    harness: NAME,
                    source,
                })?;
            stdin.shutdown().await.map_err(|source| HarnessError::Io {
                harness: NAME,
                source,
            })?;
        }

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
    fn name_is_adk() {
        assert_eq!(AdkHarness::new("adk-runner").name(), "adk");
    }

    #[test]
    fn stdin_task_carries_instruction_endpoint_tools_but_not_token() {
        let json = serde_json::to_string(&AdkTask::from_context(&task())).expect("serialize");
        assert!(json.contains("INSTRUCTMARK skill body"), "{json}");
        assert!(json.contains("INPUTMARK fold this"), "{json}");
        assert!(json.contains("http://127.0.0.1:8080/mcp"), "{json}");
        assert!(json.contains("update_page"), "{json}");
        // The scoped bearer must NEVER be written to the stdin payload — it
        // rides out-of-band via ESCUREL_MCP_BEARER.
        assert!(
            !json.contains("scoped-bearer-XYZ"),
            "the stdin task must not carry the bearer: {json}"
        );
    }

    #[test]
    fn adk_task_round_trips() {
        let t = AdkTask::from_context(&task());
        let json = serde_json::to_string(&t).expect("serialize");
        let back: AdkTask = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, t);
    }

    #[test]
    fn model_is_filtered_when_empty() {
        let h = AdkHarness::new("adk-runner").with_model(Some(String::new()));
        assert_eq!(h.model, None);
        let h = AdkHarness::new("adk-runner").with_model(Some("gemini-3.5-flash".to_owned()));
        assert_eq!(h.model.as_deref(), Some("gemini-3.5-flash"));
    }
}
