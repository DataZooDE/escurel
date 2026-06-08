//! Shared MCP-over-HTTP + WebSocket transport for [`crate::Client`]
//! and [`crate::AdminClient`].
//!
//! Holds the `reqwest` client, the `<base>/mcp` URL, and the bearer
//! token (in `SecretString` custody). Every typed method on the public
//! clients funnels through [`McpTransport::call`] (raw JSON-RPC) or
//! [`McpTransport::call_typed`] (decode the `result` into an
//! `escurel-types` struct). The live channel is [`McpTransport::live_session`].

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use futures_util::{SinkExt as _, Stream, StreamExt as _};
use secrecy::{ExposeSecret as _, SecretString};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

use crate::error::Error;
use escurel_types::{LiveAck, LiveOp};

/// Cheap-to-clone transport handle. Wraps an arc-internal
/// `reqwest::Client`, the resolved URLs, and the bearer token.
#[derive(Clone)]
pub(crate) struct McpTransport {
    http: reqwest::Client,
    /// HTTP base, e.g. `http://127.0.0.1:8080` (no trailing slash).
    base: String,
    /// `<base>/mcp`.
    mcp_url: String,
    /// Pre-formatted `Bearer <jwt>` header value; empty when the token
    /// was empty (dev / unauthenticated gateways ignore it).
    bearer: String,
    // Kept in `SecretString` custody for the lifetime of the transport
    // so the token honours the zeroisation contract on drop.
    _token: SecretString,
    next_id: Arc<AtomicI64>,
}

impl McpTransport {
    pub(crate) fn new(endpoint: &str, token: SecretString) -> Result<Self, Error> {
        let base = endpoint.trim_end_matches('/').to_owned();
        if !(base.starts_with("http://") || base.starts_with("https://")) {
            return Err(Error::InvalidEndpoint(endpoint.to_owned()));
        }
        let secret = token.expose_secret();
        let bearer = if secret.is_empty() {
            String::new()
        } else {
            let value = format!("Bearer {secret}");
            // Reject tokens that can't be a legal HTTP header value, to
            // match the old client's `Error::InvalidToken` surface.
            if reqwest::header::HeaderValue::from_str(&value).is_err() {
                return Err(Error::InvalidToken);
            }
            value
        };
        let mcp_url = format!("{base}/mcp");
        Ok(Self {
            http: reqwest::Client::new(),
            base,
            mcp_url,
            bearer,
            _token: token,
            next_id: Arc::new(AtomicI64::new(1)),
        })
    }

    /// Call an MCP tool and decode the `result` JSON into `T`.
    pub(crate) async fn call_typed<T: serde::de::DeserializeOwned>(
        &self,
        tool: &str,
        arguments: Value,
    ) -> Result<T, Error> {
        let result = self.call(tool, arguments).await?;
        serde_json::from_value(result).map_err(|e| Error::Decode(format!("{tool}: {e}")))
    }

    /// Low-level JSON-RPC `tools/call` driver. Returns the tool's
    /// payload, or maps the JSON-RPC error envelope to
    /// [`Error::JsonRpc`] and a non-success HTTP status to
    /// [`Error::Http`].
    ///
    /// The gateway MCP-shapes a `tools/call` success into a spec
    /// `CallToolResult` (`{content, structuredContent, isError}`); we
    /// unwrap `structuredContent` (the raw payload) when present so
    /// every typed/raw consumer sees the payload directly, falling back
    /// to the whole `result` for back-compat with any non-wrapped shape.
    pub(crate) async fn call(&self, tool: &str, arguments: Value) -> Result<Value, Error> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let envelope = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": tool, "arguments": arguments },
        });
        let mut req = self.http.post(&self.mcp_url).json(&envelope);
        if !self.bearer.is_empty() {
            req = req.header("authorization", &self.bearer);
        }
        let resp = req.send().await?;
        let status = resp.status();
        let body_text = resp.text().await?;
        if !status.is_success() {
            return Err(Error::Http {
                status: status.as_u16(),
                body: body_text,
            });
        }
        let body: Value =
            serde_json::from_str(&body_text).map_err(|e| Error::Decode(e.to_string()))?;
        if let Some(err) = body.get("error") {
            let code = err.get("code").and_then(Value::as_i64).unwrap_or(0);
            let message = err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            return Err(Error::JsonRpc { code, message });
        }
        let result = body.get("result").cloned().ok_or_else(|| {
            Error::Decode(format!("response missing `result` field: {body_text}"))
        })?;
        // Unwrap the MCP `CallToolResult.structuredContent` payload when
        // the gateway wrapped it; otherwise return the result as-is.
        Ok(result.get("structuredContent").cloned().unwrap_or(result))
    }

    /// GET a plain-text endpoint relative to the base (e.g.
    /// `/version`, `/healthz`). Carries the bearer if set. Used by the
    /// admin `health` synthesis, since the MCP surface has no `health`
    /// tool.
    pub(crate) async fn get_text(&self, path: &str) -> Result<String, Error> {
        let url = format!("{}{}", self.base, path);
        let mut req = self.http.get(&url);
        if !self.bearer.is_empty() {
            req = req.header("authorization", &self.bearer);
        }
        let resp = req.send().await?;
        let status = resp.status();
        let body = resp.text().await?;
        if !status.is_success() {
            return Err(Error::Http {
                status: status.as_u16(),
                body,
            });
        }
        Ok(body)
    }

    /// Open the `/ws` live-session channel and drive it with `ops`.
    /// Sends a `hello` for the first op's session, forwards each op as
    /// `{type:"op", op:<base64>}`, and yields a [`LiveAck`] per
    /// `op_ack` frame.
    pub(crate) async fn live_session<S>(
        &self,
        ops: S,
    ) -> Result<impl Stream<Item = Result<LiveAck, Error>>, Error>
    where
        S: Stream<Item = LiveOp> + Send + 'static,
    {
        let ws_url = self.ws_url();
        let mut request = ws_url
            .into_client_request()
            .map_err(|e| Error::LiveSession(format!("build ws request: {e}")))?;
        if !self.bearer.is_empty() {
            let value = reqwest::header::HeaderValue::from_str(&self.bearer)
                .map_err(|_| Error::InvalidToken)?;
            request.headers_mut().insert(
                "authorization",
                http_header_value(value.as_bytes())
                    .map_err(|e| Error::LiveSession(format!("auth header: {e}")))?,
            );
        }
        let (stream, _resp) = tokio_tungstenite::connect_async(request)
            .await
            .map_err(|e| Error::LiveSession(format!("ws connect: {e}")))?;
        let (mut writer, mut reader) = stream.split();

        // The output channel the caller's stream reads from.
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Result<LiveAck, Error>>();

        tokio::spawn(async move {
            futures_util::pin_mut!(ops);
            let mut said_hello = false;
            while let Some(op) = ops.next().await {
                // First op carries the session id we attach to.
                if !said_hello {
                    let hello = json!({ "type": "hello", "session": op.session });
                    if writer.send(Message::Text(hello.to_string())).await.is_err() {
                        let _ = tx.send(Err(Error::LiveSession("ws send hello failed".to_owned())));
                        return;
                    }
                    said_hello = true;
                    // The first LiveOp may be a bare attach (empty op);
                    // skip forwarding it as an op frame in that case.
                    if op.op.is_empty() {
                        continue;
                    }
                }
                let frame = json!({ "type": "op", "op": B64.encode(&op.op) });
                if writer.send(Message::Text(frame.to_string())).await.is_err() {
                    let _ = tx.send(Err(Error::LiveSession("ws send op failed".to_owned())));
                    return;
                }
                // Read frames until we get the matching op_ack (or error).
                match read_ack(&mut reader).await {
                    Ok(Some(ack)) => {
                        if tx.send(Ok(ack)).is_err() {
                            return;
                        }
                    }
                    Ok(None) => return,
                    Err(e) => {
                        let _ = tx.send(Err(e));
                        return;
                    }
                }
            }
            // Caller's op stream ended — close politely.
            let close = json!({ "type": "close", "commit": true });
            let _ = writer.send(Message::Text(close.to_string())).await;
        });

        Ok(tokio_stream_from(rx))
    }

    fn ws_url(&self) -> String {
        let trimmed = self
            .base
            .strip_prefix("http://")
            .or_else(|| self.base.strip_prefix("https://"))
            .unwrap_or(&self.base);
        let scheme = if self.base.starts_with("https://") {
            "wss"
        } else {
            "ws"
        };
        format!("{scheme}://{trimmed}/ws")
    }
}

/// Read WS frames until an `op_ack` (→ `Some(ack)`), a `closed`
/// (→ `None`), or an `error` frame (→ `Err`). Other frame types
/// (presence, etc.) are skipped.
async fn read_ack<R>(reader: &mut R) -> Result<Option<LiveAck>, Error>
where
    R: Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    while let Some(msg) = reader.next().await {
        let msg = msg.map_err(|e| Error::LiveSession(format!("ws recv: {e}")))?;
        let text = match msg {
            Message::Text(t) => t,
            Message::Binary(b) => String::from_utf8_lossy(&b).into_owned(),
            Message::Close(_) => return Ok(None),
            _ => continue,
        };
        let v: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => continue,
        };
        match v.get("type").and_then(Value::as_str) {
            Some("op_ack") => {
                return Ok(Some(LiveAck {
                    session: str_field(&v, "session"),
                    merged_version: str_field(&v, "merged_version"),
                    content: str_field(&v, "content"),
                    issues: Vec::new(),
                }));
            }
            Some("closed") => return Ok(None),
            Some("error") => {
                let code = v.get("code").and_then(Value::as_str).unwrap_or_default();
                let message = v.get("message").and_then(Value::as_str).unwrap_or_default();
                return Err(Error::LiveSession(format!("{code}: {message}")));
            }
            _ => continue,
        }
    }
    Ok(None)
}

fn str_field(v: &Value, k: &str) -> String {
    v.get(k)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_default()
}

fn http_header_value(
    bytes: &[u8],
) -> Result<tokio_tungstenite::tungstenite::http::HeaderValue, String> {
    tokio_tungstenite::tungstenite::http::HeaderValue::from_bytes(bytes).map_err(|e| e.to_string())
}

/// Wrap an mpsc receiver as a `Stream`. Avoids depending on
/// `tokio-stream` for the one wrapper we need.
fn tokio_stream_from<T>(mut rx: tokio::sync::mpsc::UnboundedReceiver<T>) -> impl Stream<Item = T> {
    futures_util::stream::poll_fn(move |cx| rx.poll_recv(cx))
}
