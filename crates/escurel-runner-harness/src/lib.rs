//! Harness adapters for the escurel agent runner.
//!
//! This crate holds the single async [`Harness`] adapter trait
//! (`name()` + `run(&self, task: &TaskContext) -> Result<HarnessOutcome,
//! HarnessError>`) and — as the harness work-items land — its concrete
//! adapters (**Claude Code CLI**, **Codex CLI**, **Google ADK**) per
//! [`docs/contract/agent-orchestration.md`] §"Harness-adapter trait".
//!
//! Most adapters are a thin process-management shell: they spawn the chosen
//! harness as an isolated, timed, kill-on-drop subprocess, injects the
//! `label_skill` page as instructions, points the harness at the gateway
//! `/mcp` endpoint with a scoped bearer token, and captures a structured
//! [`HarnessOutcome`]. Adapters deliberately do **not** write to escurel
//! themselves — writes flow through the harness's own MCP tool calls.
//!
//! [`GeminiHarness`] is the exception to the subprocess shape, for a
//! deployment reason: a container has no interactive auth and no node
//! runtime, so a CLI harness cannot run where the runner actually runs. It
//! drives the model over HTTP and runs the tool loop in process — and it
//! keeps the same rule, more visibly: every escurel effect is a tool the
//! MODEL chose, from the narrowed surface the packager allowed, under the
//! scoped token.
//!
//! #151 lands the trait + the first concrete adapter, [`EchoHarness`]: a
//! real subprocess (the `escurel-echo-harness` binary) that performs a
//! deterministic `update_page` + `assign_event` over the real `/mcp`. It
//! is the test stand-in for an LLM, but its escurel effects are 100% real
//! — the first true trigger→agent→instance loop.
//!
//! Per the epic's constraint this crate depends only on
//! `escurel-runner-core` (+ `escurel-client`/`escurel-types` transitively)
//! — never on `escurel-server` / `escurel-index`.
//!
//! [`docs/contract/agent-orchestration.md`]: https://github.com/DataZooDE/escurel/blob/main/docs/contract/agent-orchestration.md

mod adk;
mod agy;
mod claude;
mod codex;
mod echo;
mod gemini;
mod harness;
mod task;

pub use adk::{AdkHarness, AdkTask};
pub use agy::AgyHarness;
pub use claude::ClaudeHarness;
pub use codex::CodexHarness;
pub use echo::EchoHarness;
pub use gemini::{
    DEFAULT_BASE_URL as GEMINI_DEFAULT_BASE_URL, DEFAULT_MODEL as GEMINI_DEFAULT_MODEL,
    GeminiHarness,
};
pub use harness::{Harness, HarnessError, HarnessOutcome, HarnessStatus};
pub use task::HarnessTask;
