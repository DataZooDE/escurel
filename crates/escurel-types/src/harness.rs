//! The harness adapter names: what a skill's or an agent's `harness:` may say,
//! what `ESCUREL_RUNNER_HARNESS` selects, and what a run reports.
//!
//! One list, shared by `validate` (escurel-index) and the runner, so a harness
//! the runner can build is never a lint finding — the list had drifted when it
//! lived in validate alone and the OpenAI-compatible presets were added to the
//! runner (#823 hardening).

/// The deterministic test harness.
pub const ECHO: &str = "echo";
/// The A2A delegate harness (async-ops Phase 4).
pub const DELEGATE: &str = "delegate";
/// Alibaba Cloud Model Studio, an OpenAI-compatible preset.
pub const ALIBABA: &str = "alibaba";
/// Hetzner Inference API, an OpenAI-compatible preset.
pub const HETZNER: &str = "hetzner";
/// Any other OpenAI-compatible endpoint.
pub const OPENAI_COMPATIBLE: &str = "openai-compatible";

/// Every adapter the runner's `build_harness_named` builds.
pub const KNOWN_HARNESSES: [&str; 11] = [
    ECHO,
    "claude",
    "codex",
    "agy",
    "muse",
    "gemini",
    "gemini-vertex",
    DELEGATE,
    ALIBABA,
    HETZNER,
    OPENAI_COMPATIBLE,
];

/// Whether `name` is a harness the runner can build.
#[must_use]
pub fn is_known_harness(name: &str) -> bool {
    KNOWN_HARNESSES.contains(&name)
}
