//! Live collaborative-editing (CRDT) session frames.
//!
//! Proto `LiveOp` / `LiveAck` — the bidi-stream frames for the
//! `open_session` / `apply_op` / `close_session` MCP tools.

use serde::{Deserialize, Serialize};

use crate::agent::ValidationIssue;

/// A live op frame (proto `LiveOp`). `op` is the raw CRDT op bytes;
/// on the MCP `apply_op` wire these arrive base64-encoded under the
/// `op` key — base64 (de)serialization is the consumer's job in a
/// later task.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct LiveOp {
    pub session: String,
    pub op: Vec<u8>,
}

/// A live ack frame (proto `LiveAck`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct LiveAck {
    pub session: String,
    pub merged_version: String,
    pub content: String,
    pub issues: Vec<ValidationIssue>,
}
