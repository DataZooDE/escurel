//! Live remote (proxy) backend execution at the gateway (`openapi` / `mcp`).
//!
//! `escurel-index` owns the binding model ([`RemoteBinding`]), the endpoint
//! registry (`external_endpoints`), and the pure projection / templating
//! helpers ([`escurel_index::backend::remote`]). The actual **outbound**
//! HTTP / MCP call lives here — the gateway already carries `reqwest` for the
//! capture webhook, and keeping the network out of the DuckDB-linked index
//! crate preserves its offline test loop.
//!
//! Two entry points:
//! - [`fetch_projection`] — `expand`'s live read; returns the
//!   `backend_projection` object (`{ source, fields }`, or `{ issue }` on any
//!   failure — the read path never fabricates a body).
//! - [`write_instance`] — the `write_instance` tool's write-back; forwards the
//!   payload to the binding's `write` op and returns the re-projected fields.
//!
//! The outbound `reqwest::Client` honours `HTTPS_PROXY` from the environment
//! (reqwest reads it by default), so calls traverse the same egress path as
//! the rest of the gateway.

use std::time::Duration;

use escurel_index::backend::remote::{
    fill_template, render_body, resolve_projection, template_vars, unfilled_placeholders,
};
use escurel_index::endpoints::{EndpointAuth, EndpointRecord};
use escurel_index::{Indexer, RemoteBinding, RemoteKind, RemoteOp};
use serde_json::{Map, Value, json};

/// Outbound timeout for a single remote read/write.
const REMOTE_TIMEOUT: Duration = Duration::from_secs(10);

/// Build the outbound client. `reqwest` picks up `HTTPS_PROXY` from the env by
/// default, so this traverses the gateway's egress proxy.
fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(REMOTE_TIMEOUT)
        .build()
        .unwrap_or_default()
}

/// A `backend_projection` value carrying only an `issue` — returned when a
/// live read cannot be completed (unknown endpoint, transport error, non-2xx).
/// Mirrors the SQL-view `binding_degraded` fail-closed policy: an `Issue`,
/// never a partial or fabricated body.
fn issue(msg: impl Into<String>) -> Value {
    json!({ "issue": msg.into() })
}

/// Live-read a remote instance and return its `backend_projection`
/// (`{ source, fields }`). Any failure resolves to `{ issue }` — the overlay
/// page (rendered by `expand`) is still returned; only the live projection is
/// degraded.
pub(crate) async fn fetch_projection(
    indexer: &Indexer,
    skill: &str,
    page_slug: Option<&str>,
) -> Value {
    let binding = match indexer.skill_backend(skill).await {
        Ok(b) => b,
        Err(e) => return issue(format!("binding load failed: {e}")),
    };
    let Some(remote) = binding.remote else {
        return issue("skill declares no remote backend binding");
    };
    let ep = match indexer.lookup_endpoint(&remote.endpoint).await {
        Ok(Some(ep)) => ep,
        Ok(None) => return issue(format!("endpoint `{}` is not registered", remote.endpoint)),
        Err(e) => return issue(format!("endpoint lookup failed: {e}")),
    };
    match exec(&ep, &remote, &remote.read, page_slug, None).await {
        Ok(resp) => {
            let fields = resolve_projection(&resp, &remote.project);
            json!({ "source": ep.name, "fields": Value::Object(fields) })
        }
        Err(e) => issue(e),
    }
}

/// Forward a write to a remote instance's `write` op and return the
/// re-projected fields. `Err` when the binding declares no `write` op
/// (`backend_read_only`), the endpoint is unknown, or the upstream fails.
pub(crate) async fn write_instance(
    indexer: &Indexer,
    skill: &str,
    page_slug: Option<&str>,
    payload: &Value,
) -> Result<Value, String> {
    let binding = indexer
        .skill_backend(skill)
        .await
        .map_err(|e| e.to_string())?;
    let remote = binding
        .remote
        .ok_or_else(|| "skill declares no remote backend binding".to_owned())?;
    let write = remote
        .write
        .clone()
        .ok_or_else(|| "backend_read_only: remote binding declares no write op".to_owned())?;
    let ep = indexer
        .lookup_endpoint(&remote.endpoint)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("endpoint `{}` is not registered", remote.endpoint))?;
    let resp = exec(&ep, &remote, &write, page_slug, Some(payload)).await?;
    let fields = resolve_projection(&resp, &remote.project);
    Ok(json!({ "ok": true, "source": ep.name, "fields": Value::Object(fields) }))
}

/// Reachability probe for `validate_endpoints`: an `mcp` endpoint answers a
/// `tools/list`; an `openapi` endpoint answers a bare `GET` to its base URL.
/// Returns `("ok", None)` on success or `("unreachable", Some(detail))`.
pub(crate) async fn probe(ep: &EndpointRecord) -> (String, Option<String>) {
    let result: Result<(), String> = if ep.kind == "mcp" {
        mcp_call(ep, "tools/list", json!({})).await.map(|_| ())
    } else {
        apply_auth(client().get(ep.base_url.as_str()), ep)
            .send()
            .await
            .map(|_| ())
            .map_err(|e| format!("transport error: {e}"))
    };
    match result {
        Ok(()) => ("ok".to_owned(), None),
        Err(e) => ("unreachable".to_owned(), Some(e)),
    }
}

/// Execute one remote op against `ep`. The `(kind, op)` pairing is validated
/// so an `mcp` op can never be dispatched over an `openapi` endpoint.
async fn exec(
    ep: &EndpointRecord,
    remote: &RemoteBinding,
    op: &RemoteOp,
    id: Option<&str>,
    payload: Option<&Value>,
) -> Result<Value, String> {
    // Fail closed on a protocol mismatch: the skill's backend kind must match
    // the kind the endpoint was registered under. `create_remote_instance`
    // only checks the endpoint *name* exists, so without this an `openapi`
    // skill pointing at an endpoint registered as `mcp` (or vice-versa) would
    // dispatch the wrong transport at a URL that speaks the other protocol.
    if ep.kind != remote.kind.as_str() {
        return Err(format!(
            "endpoint `{}` is registered as `{}` but the skill's backend is `{}`",
            ep.name,
            ep.kind,
            remote.kind.as_str()
        ));
    }
    match (remote.kind, op) {
        (RemoteKind::OpenApi, RemoteOp::Http { method, path, body }) => {
            http_call(ep, method, path, body.as_ref(), id, payload).await
        }
        (RemoteKind::Mcp, RemoteOp::McpTool { name }) => {
            let args = mcp_args(id, payload);
            let result =
                mcp_call(ep, "tools/call", json!({ "name": name, "arguments": args })).await?;
            Ok(extract_mcp_result("tools/call", result))
        }
        (RemoteKind::Mcp, RemoteOp::McpResource { uri }) => {
            let filled = fill_template(uri, &template_vars(id, payload));
            // Fail closed, matching the openapi path/body: never send a literal
            // `{placeholder}` upstream when a template var did not resolve.
            let missing = unfilled_placeholders(&filled);
            if !missing.is_empty() {
                return Err(format!("unfilled resource placeholders: {missing:?}"));
            }
            let result = mcp_call(ep, "resources/read", json!({ "uri": filled })).await?;
            Ok(extract_mcp_result("resources/read", result))
        }
        _ => Err("remote op does not match endpoint kind".to_owned()),
    }
}

/// Execute an OpenAPI/REST call: fill the `{name}` path placeholders (from the
/// overlay id + payload scalars), join to the base URL, apply auth, attach the
/// JSON body (a rendered `body:` template if declared, else the raw payload),
/// and parse the JSON response. Under-specified path/body templates fail closed.
async fn http_call(
    ep: &EndpointRecord,
    method: &str,
    path: &str,
    body_template: Option<&Value>,
    id: Option<&str>,
    payload: Option<&Value>,
) -> Result<Value, String> {
    let vars = template_vars(id, payload);
    let filled = fill_template(path, &vars);
    let missing = unfilled_placeholders(&filled);
    if !missing.is_empty() {
        return Err(format!("unfilled path placeholders: {missing:?}"));
    }
    let url = join_url(&ep.base_url, &filled);
    let http_method = reqwest::Method::from_bytes(method.as_bytes())
        .map_err(|_| format!("invalid HTTP method `{method}`"))?;
    let mut req = apply_auth(client().request(http_method, url.as_str()), ep);
    if let Some(tpl) = body_template {
        // A declared body template reshapes the payload; unresolved
        // placeholders fail the write closed rather than send a literal `{x}`.
        let (rendered, missing) = render_body(tpl, id, payload.unwrap_or(&Value::Null));
        if !missing.is_empty() {
            return Err(format!("unfilled body placeholders: {missing:?}"));
        }
        req = req.json(&rendered);
    } else if let Some(p) = payload {
        req = req.json(p);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| format!("transport error: {e}"))?;
    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        return Err(format!("upstream status {}: {body}", status.as_u16()));
    }
    Ok(body)
}

/// Execute a JSON-RPC 2.0 MCP call over HTTP to the endpoint's `/mcp` URL and
/// return the `result` object (or an error string for a JSON-RPC error /
/// non-2xx / transport failure).
async fn mcp_call(ep: &EndpointRecord, method: &str, params: Value) -> Result<Value, String> {
    let rpc = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
    let req = apply_auth(
        client()
            .post(ep.base_url.as_str())
            .header("accept", "application/json, text/event-stream")
            .json(&rpc),
        ep,
    );
    let resp = req
        .send()
        .await
        .map_err(|e| format!("transport error: {e}"))?;
    let status = resp.status();
    let body: Value = resp
        .json()
        .await
        .map_err(|e| format!("invalid JSON-RPC response: {e}"))?;
    if let Some(err) = body.get("error") {
        return Err(format!("mcp error: {err}"));
    }
    if !status.is_success() {
        return Err(format!("upstream status {}", status.as_u16()));
    }
    Ok(body.get("result").cloned().unwrap_or(Value::Null))
}

/// Normalise an MCP `result` into a plain JSON value the projection can read:
/// prefer `structuredContent`; else the first text content parsed as JSON (or
/// wrapped as `{ text }`); resources use `contents[0].text`.
fn extract_mcp_result(method: &str, result: Value) -> Value {
    let first_text = |key: &str| -> Option<String> {
        result
            .get(key)
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(|first| first.get("text"))
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    if method == "resources/read" {
        if let Some(text) = first_text("contents") {
            return serde_json::from_str::<Value>(&text)
                .unwrap_or_else(|_| json!({ "text": text }));
        }
        return result;
    }
    if let Some(sc) = result.get("structuredContent") {
        return sc.clone();
    }
    if let Some(text) = first_text("content") {
        return serde_json::from_str::<Value>(&text).unwrap_or_else(|_| json!({ "text": text }));
    }
    result
}

/// Arguments for an MCP tool call: the overlay id (`{id}`) merged with the
/// write payload's object fields (payload wins on key collision).
fn mcp_args(id: Option<&str>, payload: Option<&Value>) -> Value {
    let mut m = Map::new();
    if let Some(id) = id {
        m.insert("id".to_owned(), Value::String(id.to_owned()));
    }
    if let Some(Value::Object(p)) = payload {
        for (k, v) in p {
            m.insert(k.clone(), v.clone());
        }
    }
    Value::Object(m)
}

/// Apply the endpoint's auth to a request builder.
fn apply_auth(req: reqwest::RequestBuilder, ep: &EndpointRecord) -> reqwest::RequestBuilder {
    match &ep.auth {
        EndpointAuth::None => req,
        EndpointAuth::Bearer => match &ep.secret {
            Some(s) => req.bearer_auth(s),
            None => req,
        },
        EndpointAuth::ApiKey { header } => match &ep.secret {
            Some(s) => req.header(header.as_str(), s),
            None => req,
        },
    }
}

/// Join a base URL and a (possibly leading-slash) path without doubling `/`.
fn join_url(base: &str, path: &str) -> String {
    let b = base.trim_end_matches('/');
    if let Some(rest) = path.strip_prefix('/') {
        format!("{b}/{rest}")
    } else {
        format!("{b}/{path}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_url_normalises_slashes() {
        assert_eq!(
            join_url("https://h/api", "/customers/1"),
            "https://h/api/customers/1"
        );
        assert_eq!(
            join_url("https://h/api/", "/customers/1"),
            "https://h/api/customers/1"
        );
        assert_eq!(
            join_url("https://h/api", "customers/1"),
            "https://h/api/customers/1"
        );
    }

    #[test]
    fn extract_mcp_result_prefers_structured_content() {
        let r = json!({ "structuredContent": { "title": "x" }, "content": [] });
        assert_eq!(extract_mcp_result("tools/call", r), json!({ "title": "x" }));
    }

    #[test]
    fn extract_mcp_result_parses_text_content_json() {
        let r = json!({ "content": [{ "type": "text", "text": "{\"title\":\"y\"}" }] });
        assert_eq!(extract_mcp_result("tools/call", r), json!({ "title": "y" }));
    }

    #[test]
    fn extract_mcp_resource_reads_contents_text() {
        let r = json!({ "contents": [{ "uri": "kb://a", "text": "{\"title\":\"z\"}" }] });
        assert_eq!(
            extract_mcp_result("resources/read", r),
            json!({ "title": "z" })
        );
    }

    #[test]
    fn mcp_args_merges_id_and_payload() {
        let payload = json!({ "tier": "gold" });
        assert_eq!(
            mcp_args(Some("acme"), Some(&payload)),
            json!({ "id": "acme", "tier": "gold" })
        );
    }
}
