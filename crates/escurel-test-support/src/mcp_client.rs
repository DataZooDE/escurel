//! Minimal raw JSON-RPC client for the MCP-over-HTTP transport,
//! used by gateway tests that want to assert on the *wire bytes* of a
//! tool call (the error envelope, a raw field) rather than the typed
//! surface.
//!
//! For typed end-to-end calls, use [`crate::EscurelProcess::client`] —
//! it hands out an `escurel_client::Client`, the same typed client
//! downstream apps use. This façade exists only for the low-level
//! `call(tool, args) -> raw result Value` path, so it deliberately
//! carries no per-tool decoders (those live in `escurel-client`).

use std::sync::atomic::{AtomicI64, Ordering};

use serde_json::{Value, json};

/// #569: `reqwest` has no default request timeout. This client only ever
/// talks to a same-process, localhost `EscurelProcess`, so a real hang is
/// unlikely — but a bare `Client::new()` on a genuine hang would still turn
/// into "the test suite never finishes" rather than a clean failure. Kept
/// consistent with the fix everywhere else this pattern was found.
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Error variants returned by [`McpTestClient::call`]. Mirrors the
/// wire-failure modes a test cares about: transport, HTTP status,
/// JSON-RPC error envelope, JSON decode.
#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("transport error: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("http {status}: {body}")]
    Http { status: u16, body: String },
    #[error("jsonrpc error: code={code} message={message}")]
    JsonRpc { code: i64, message: String },
    #[error("response missing `result` field: {body}")]
    MissingResult { body: String },
    /// [`McpTestClient::call_ok`] only: the tool REFUSED the call (`isError` / `ok: false`).
    #[error("refused: {0}")]
    Refused(escurel_types::call_result::Refusal),
    #[error("response decode failed: {source}")]
    Decode {
        #[source]
        source: serde_json::Error,
    },
}

/// JSON-RPC client over `POST /mcp`. Cheap to clone — wraps a
/// `reqwest::Client` (already arc-internal) and a string URL + bearer.
#[derive(Clone)]
pub struct McpTestClient {
    http: reqwest::Client,
    mcp_url: String,
    bearer: Option<String>,
    next_id: std::sync::Arc<AtomicI64>,
}

impl std::fmt::Debug for McpTestClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Deliberately do not print `bearer` — it carries a JWT.
        f.debug_struct("McpTestClient")
            .field("mcp_url", &self.mcp_url)
            .finish_non_exhaustive()
    }
}

impl McpTestClient {
    pub(crate) fn new(mcp_url: String, bearer: Option<String>) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .build()
                .unwrap_or_default(),
            mcp_url,
            bearer,
            next_id: std::sync::Arc::new(AtomicI64::new(1)),
        }
    }

    /// Low-level JSON-RPC `tools/call` driver. Returns the inner
    /// `result` JSON value, or maps the JSON-RPC error envelope to
    /// [`McpError::JsonRpc`] and a non-success HTTP status to
    /// [`McpError::Http`].
    ///
    /// NOTE for tests: this returns the payload WHETHER OR NOT the tool refused (a refusal is the
    /// payload `{ok: false, issues}`), because many tests assert on refusals. A test that wants a
    /// refusal to fail loudly uses [`Self::call_ok`].
    pub async fn call(&self, tool: &str, arguments: Value) -> Result<Value, McpError> {
        let result = self.call_envelope(tool, arguments).await?;
        Ok(result.get("structuredContent").cloned().unwrap_or(result))
    }

    /// Like [`Self::call`], but a refused call (`isError` / `ok: false`) is
    /// [`McpError::Refused`], never a payload.
    pub async fn call_ok(&self, tool: &str, arguments: Value) -> Result<Value, McpError> {
        let result = self.call_envelope(tool, arguments).await?;
        escurel_types::call_result::unwrap_call_result(result).map_err(McpError::Refused)
    }

    /// The whole `CallToolResult`, unopened.
    async fn call_envelope(&self, tool: &str, arguments: Value) -> Result<Value, McpError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let envelope = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": tool, "arguments": arguments },
        });
        let mut req = self.http.post(&self.mcp_url).json(&envelope);
        if let Some(b) = &self.bearer {
            req = req.header("authorization", format!("Bearer {b}"));
        }
        let resp = req.send().await?;
        let status = resp.status();
        let body_text = resp.text().await?;
        if !status.is_success() {
            return Err(McpError::Http {
                status: status.as_u16(),
                body: body_text,
            });
        }
        let body: Value =
            serde_json::from_str(&body_text).map_err(|source| McpError::Decode { source })?;
        if let Some(err) = body.get("error") {
            let code = err.get("code").and_then(Value::as_i64).unwrap_or(0);
            let message = err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            return Err(McpError::JsonRpc { code, message });
        }
        let result = body
            .get("result")
            .cloned()
            .ok_or(McpError::MissingResult { body: body_text })?;
        Ok(result)
    }
}
