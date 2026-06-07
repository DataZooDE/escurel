//! Harness adapters for the escurel agent runner.
//!
//! This crate holds the single async [`Harness`] adapter trait
//! (`name()` + `run(&self, task: &TaskContext) -> Result<HarnessOutcome,
//! HarnessError>`) and — as the harness work-items land — its concrete
//! adapters (**Claude Code CLI**, **Codex CLI**, **Google ADK**) per
//! [`docs/contract/agent-orchestration.md`] §"Harness-adapter trait".
//!
//! Each adapter is a thin process-management shell: it spawns the chosen
//! harness as an isolated, timed, kill-on-drop subprocess, injects the
//! `label_skill` page as instructions, points the harness at the gateway
//! `/mcp` endpoint with a scoped bearer token, and captures a structured
//! [`HarnessOutcome`]. Adapters deliberately do **not** write to escurel
//! themselves — writes flow through the harness's own MCP tool calls.
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
mod claude;
mod codex;
mod echo;
mod harness;
mod task;

pub use adk::{AdkHarness, AdkTask};
pub use claude::ClaudeHarness;
pub use codex::CodexHarness;
pub use echo::EchoHarness;
pub use harness::{Harness, HarnessError, HarnessOutcome, HarnessStatus};
pub use task::HarnessTask;
