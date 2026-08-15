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

// ── HTTP session tools (open_session / apply_op / close_session) ──

/// `open_session` arguments. MCP wire key: `page_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct OpenSessionRequest {
    pub page_id: String,
}

/// MCP wire keys: `session`, `head_version`, `ws_url`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct OpenSessionResponse {
    /// The minted session id — the value every subsequent
    /// `apply_op`/`close_session` (and WS `hello`) names.
    pub session: String,
    /// The page's monotonic head version (`v<hlc>`) at open time. NOT a
    /// substitute for `expand`'s `version` in the `update_page`
    /// `base_version` guard — a live session does not observe whole-page
    /// writes.
    pub head_version: String,
    /// Advisory relative WS upgrade path (canonically `/ws`): clients
    /// with WS support should switch to the socket after this call.
    pub ws_url: String,
}

/// `apply_op` arguments. MCP wire keys: `session`, `op` — the op is the
/// **base64-encoded** Loro op bytes exactly as the wire carries them
/// (unlike [`LiveOp::op`], which holds the raw bytes for the WS framing).
/// The op author is always the verified token subject; there is no field
/// to name one, by design (#357).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ApplyOpRequest {
    pub session: String,
    /// base64-encoded Loro op blob.
    pub op: String,
}

/// MCP wire keys: `ok`, `merged_version`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ApplyOpResponse {
    pub ok: bool,
    pub merged_version: String,
}

/// `close_session` arguments. MCP wire keys: `session`, `commit`.
///
/// `commit` **defaults to `true`** — the wire default: closing a session
/// commits it (snapshot + indexer write-through of the merged body);
/// `commit: false` is an explicit discard. `Default::default()` matches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CloseSessionRequest {
    pub session: String,
    pub commit: bool,
}

impl Default for CloseSessionRequest {
    fn default() -> Self {
        Self {
            session: String::new(),
            // The wire default: a defaulted close is a commit, never a
            // silent discard.
            commit: true,
        }
    }
}

/// MCP wire keys: `ok`, `final_version`, `issues`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct CloseSessionResponse {
    pub ok: bool,
    pub final_version: String,
    pub issues: Vec<ValidationIssue>,
}
