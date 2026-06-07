//! Harness adapters for the escurel agent runner.
//!
//! This crate will hold the single async `Harness` adapter trait
//! (`name()` + `run(&self, task: &TaskContext) -> Result<HarnessOutcome,
//! HarnessError>`) and its three concrete adapters — **Claude Code
//! CLI**, **Codex CLI**, and **Google ADK** — per
//! [`docs/contract/agent-orchestration.md`] §"Harness-adapter trait".
//!
//! Each adapter is a thin process-management shell: it spawns the chosen
//! harness as an isolated, timed, kill-on-drop subprocess, injects the
//! `label_skill` page as instructions, points the harness at the
//! gateway `/mcp` endpoint with a scoped bearer token, and captures a
//! structured `HarnessOutcome`. Adapters deliberately do **not** write
//! to escurel themselves — writes flow through the harness's own MCP
//! tool calls.
//!
//! The trait and adapters land in work-item #151; this crate exists now
//! so the workspace dependency graph is in place. Per the epic's
//! constraint it depends **only** on `escurel-client` + `escurel-types`
//! (never on `escurel-server` / `escurel-index`).
//!
//! [`docs/contract/agent-orchestration.md`]: https://github.com/DataZooDE/escurel/blob/main/docs/contract/agent-orchestration.md

// Intentionally empty for the skeleton issue (#145). The `Harness`
// trait and adapters arrive in #151.
