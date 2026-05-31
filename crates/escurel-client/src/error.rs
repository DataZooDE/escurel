//! Error type surfaced by [`crate::Client`] and [`crate::AdminClient`].
//!
//! The client speaks MCP-over-HTTP (JSON-RPC 2.0 on `POST /mcp`) plus a
//! WebSocket live channel. This enum maps the wire-failure modes a
//! downstream app cares about — transport, HTTP status, JSON-RPC error
//! envelope, decode — into a small, semver-tracked surface so callers
//! don't pin `reqwest` / `tokio-tungstenite` directly.
//!
//! See `docs/spec/dx.md` §"Stability and versioning" — additions to
//! this enum are *breaking* (per the spec), so keep the surface
//! intentionally minimal.

use thiserror::Error;

/// JSON-RPC error code the gateway returns when an admin-gated tool is
/// called without an admin-role bearer (`require_admin` in the server's
/// MCP dispatcher). Re-exported so callers can branch on the
/// permission-denied case without hard-coding the literal.
pub const JSONRPC_ADMIN_REQUIRED: i64 = -32001;

/// Failures surfaced by [`crate::Client`] / [`crate::AdminClient`]
/// methods.
#[derive(Debug, Error)]
pub enum Error {
    /// The endpoint URL passed to `connect` failed to parse, or the
    /// `/ws` URL could not be derived from it. The inner string is the
    /// offending URL.
    #[error("invalid endpoint URL `{0}`")]
    InvalidEndpoint(String),

    /// The bearer token contains characters that are not legal in an
    /// HTTP header value (control bytes, non-ASCII, etc.).
    #[error("invalid bearer token: not a valid HTTP header value")]
    InvalidToken,

    /// The HTTP request never produced a response — connection
    /// refused, DNS failure, TLS error, timeout, …
    #[error("transport error: {0}")]
    Transport(#[source] reqwest::Error),

    /// The gateway returned a non-success HTTP status (e.g. 401 for a
    /// rejected bearer, 429 for quota exhaustion). The JSON-RPC error
    /// envelope is only used for *protocol*-level errors; transport /
    /// auth failures stay at the HTTP layer.
    #[error("http {status}: {body}")]
    Http { status: u16, body: String },

    /// The gateway returned a JSON-RPC 2.0 error envelope. `code`
    /// `-32001` ([`JSONRPC_ADMIN_REQUIRED`]) means the caller lacked the
    /// admin role; `-32000` is quota exhaustion; the rest follow the
    /// JSON-RPC spec.
    #[error("jsonrpc error: code={code} message={message}")]
    JsonRpc { code: i64, message: String },

    /// The response body could not be decoded into the expected typed
    /// shape (malformed JSON, missing `result`, or a field-type
    /// mismatch against the wire contract).
    #[error("response decode failed: {0}")]
    Decode(String),

    /// The WebSocket live-session channel failed (handshake rejected,
    /// frame protocol violation, or the socket closed mid-stream).
    #[error("live session error: {0}")]
    LiveSession(String),
}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        Self::Transport(e)
    }
}
