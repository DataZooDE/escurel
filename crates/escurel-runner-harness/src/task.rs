//! The serializable task payload the adapter hands its harness subprocess.
//!
//! A [`crate::Harness`] adapter cannot pass an in-process `TaskContext`
//! across a process boundary, so it projects the parts a harness needs into
//! this JSON-serializable [`HarnessTask`] and writes it to the child's
//! stdin. The scoped bearer rides in `token` so the harness authenticates
//! to `/mcp` as the agent — the adapter never makes escurel writes itself.

use escurel_runner_core::TaskContext;
use serde::{Deserialize, Serialize};

/// The task a harness subprocess reads on stdin: instructions, input,
/// the `/mcp` endpoint, the allowed-tool surface, and the scoped bearer.
///
/// This is the harness wire contract — the same JSON shape every adapter
/// hands every harness, so a harness binary stays adapter-agnostic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessTask {
    /// The agent's instructions (skill body + task framing + event).
    pub instructions: String,
    /// The agent's input (event + instance state + history).
    pub input: String,
    /// The gateway `/mcp` endpoint the harness calls.
    pub mcp_endpoint: String,
    /// The narrowed tool surface the run may call.
    pub allowed_tools: Vec<String>,
    /// Tenant-scoped bearer for the `/mcp` toolset.
    pub token: String,
}

impl HarnessTask {
    /// Project a [`TaskContext`] into the serializable wire payload. This is
    /// the single point where the packaged scoped token is read out of the
    /// `TaskContext`'s opaque holder for transport to the subprocess.
    pub fn from_context(ctx: &TaskContext) -> Self {
        Self {
            instructions: ctx.instructions.clone(),
            input: ctx.input.clone(),
            mcp_endpoint: ctx.mcp_endpoint.clone(),
            allowed_tools: ctx.allowed_tools.clone(),
            token: ctx.token_str().to_owned(),
        }
    }
}
