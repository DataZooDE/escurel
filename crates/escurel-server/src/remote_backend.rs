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
//! Every outbound call goes through [`crate::egress::Egress`]: https only (loopback http only when
//! the policy allows it), IP checks after DNS with the connection pinned to what was checked, no
//! redirects, a response cap, a timeout, and per-endpoint concurrency and rate limits. A remote
//! projection is EXTERNAL DATA: it is marked `trust: "external"` and is never instructions.

use escurel_index::backend::remote::{
    fill_path_template, fill_template, render_body, resolve_projection, template_vars,
    unfilled_placeholders,
};
use escurel_index::endpoints::{EndpointAuth, EndpointRecord};
use escurel_index::{Indexer, RemoteBinding, RemoteKind, RemoteOp};
use serde_json::{Map, Value, json};

use crate::egress::{Egress, EgressError};

/// The per-endpoint limiter key: tenant-scoped, so one tenant cannot exhaust another's budget.
fn limiter_key(indexer: &Indexer, endpoint: &str) -> String {
    format!("{}:{endpoint}", indexer.tenant())
}

/// Resolve an endpoint's secret at CALL time, from a REFERENCE:
/// - `env:NAME` — the environment variable `NAME`;
/// - `gsm:NAME` — `ESCUREL_SECRET_<NAME>` (the substrate injects GCP Secret Manager secrets as env
///   at deploy);
/// - `file:/path` — the trimmed contents of a file (a mounted secret volume).
///
/// Anything else is the legacy inline secret, kept only for development. An unresolvable reference
/// is an error that names the REFERENCE, never a value.
fn resolve_secret(raw: &str) -> Result<String, String> {
    let unavailable = |shown: &str| format!("secret reference `{shown}` is not available");
    let non_empty = |v: Option<String>, shown: &str| {
        v.map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| unavailable(shown))
    };
    if let Some(name) = raw.strip_prefix("env:") {
        return non_empty(std::env::var(name).ok(), raw);
    }
    if let Some(name) = raw.strip_prefix("gsm:") {
        let var = format!(
            "ESCUREL_SECRET_{}",
            name.chars()
                .map(|c| if c.is_ascii_alphanumeric() {
                    c.to_ascii_uppercase()
                } else {
                    '_'
                })
                .collect::<String>()
        );
        return non_empty(std::env::var(var).ok(), raw);
    }
    if let Some(path) = raw.strip_prefix("file:") {
        return non_empty(std::fs::read_to_string(path).ok(), raw);
    }
    Ok(raw.to_owned())
}

/// A `backend_projection` value carrying only an `issue` — returned when a
/// live read cannot be completed (unknown endpoint, transport error, non-2xx).
/// Mirrors the SQL-view `binding_degraded` fail-closed policy: an `Issue`,
/// never a partial or fabricated body.
fn issue(msg: impl Into<String>) -> Value {
    json!({ "issue": msg.into(), "trust": "external" })
}

/// Live-read a remote instance and return its `backend_projection`
/// (`{ source, fields, trust, fetched_at }`). Any failure resolves to `{ issue }` — the overlay
/// page (rendered by `expand`) is still returned; only the live projection is degraded.
pub(crate) async fn fetch_projection(
    indexer: &Indexer,
    egress: &Egress,
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
    let key = limiter_key(indexer, &ep.name);
    match exec(egress, &key, &ep, &remote, &remote.read, page_slug, None).await {
        Ok(resp) => {
            let fields = resolve_projection(&resp, &remote.project);
            json!({
                "source": ep.name,
                "fields": Value::Object(fields),
                // Everything here came from an upstream the tenant does not control: data to show,
                // never instructions to follow.
                "trust": "external",
                "fetched_at": escurel_index::now_rfc3339_micros(),
            })
        }
        Err(e) => issue(e),
    }
}

/// Forward a write to a remote instance's `write` op and return the
/// re-projected fields. `Err` when the binding declares no `write` op
/// (`backend_read_only`), the endpoint is unknown, or the upstream fails.
pub(crate) async fn write_instance(
    indexer: &Indexer,
    egress: &Egress,
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
    let key = limiter_key(indexer, &ep.name);
    let resp = exec(egress, &key, &ep, &remote, &write, page_slug, Some(payload)).await?;
    let fields = resolve_projection(&resp, &remote.project);
    Ok(
        json!({ "ok": true, "source": ep.name, "fields": Value::Object(fields),
               "trust": "external" }),
    )
}

/// Reachability probe for `validate_endpoints`: an `mcp` endpoint answers a
/// `tools/list`; an `openapi` endpoint answers a bare `GET` to its base URL.
/// Returns `("ok", None)` on success or `("unreachable", Some(detail))`; a policy refusal is
/// reported as `("refused", Some(why))` so an operator can tell "down" from "not allowed".
pub(crate) async fn probe(
    egress: &Egress,
    key: &str,
    ep: &EndpointRecord,
) -> (String, Option<String>) {
    let result: Result<(), String> = if ep.kind == "mcp" {
        mcp_call(egress, key, ep, "tools/list", json!({}))
            .await
            .map(|_| ())
    } else {
        send(egress, key, ep, |c, url| c.get(url)).await.map(|_| ())
    };
    match result {
        Ok(()) => ("ok".to_owned(), None),
        Err(e) if e.starts_with("egress policy") => ("refused".to_owned(), Some(e)),
        Err(e) => ("unreachable".to_owned(), Some(e)),
    }
}

/// One policed request to `ep`: admit (rate + concurrency), validate and pin the destination,
/// apply auth, send, refuse a redirect, and read the body up to the cap. `build` receives the
/// pinned client and the checked URL and returns the request.
async fn send(
    egress: &Egress,
    key: &str,
    ep: &EndpointRecord,
    build: impl FnOnce(&reqwest::Client, &str) -> reqwest::RequestBuilder,
) -> Result<(reqwest::StatusCode, Vec<u8>), String> {
    let _permit = egress.admit(key).map_err(|e| e.to_string())?;
    let (client, url) = egress
        .client_for(&ep.base_url)
        .await
        .map_err(|e| e.to_string())?;
    let _ = url;
    let req = apply_auth(build(&client, ep.base_url.as_str()), ep)?;
    egress
        .send_capped(req)
        .await
        .map_err(|e: EgressError| e.to_string())
}

/// Like [`send`] but to `base_url + path` (REST ops).
async fn send_path(
    egress: &Egress,
    key: &str,
    ep: &EndpointRecord,
    path: &str,
    build: impl FnOnce(&reqwest::Client, &str) -> reqwest::RequestBuilder,
) -> Result<(reqwest::StatusCode, Vec<u8>), String> {
    let _permit = egress.admit(key).map_err(|e| e.to_string())?;
    let full = join_url(&ep.base_url, path);
    let (client, _url) = egress.client_for(&full).await.map_err(|e| e.to_string())?;
    let req = apply_auth(build(&client, full.as_str()), ep)?;
    egress
        .send_capped(req)
        .await
        .map_err(|e: EgressError| e.to_string())
}

/// Execute one remote op against `ep`. The `(kind, op)` pairing is validated
/// so an `mcp` op can never be dispatched over an `openapi` endpoint.
async fn exec(
    egress: &Egress,
    key: &str,
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
            http_call(egress, key, ep, method, path, body.as_ref(), id, payload).await
        }
        (RemoteKind::Mcp, RemoteOp::McpTool { name }) => {
            let args = mcp_args(id, payload);
            let result = mcp_call(
                egress,
                key,
                ep,
                "tools/call",
                json!({ "name": name, "arguments": args }),
            )
            .await?;
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
            let result =
                mcp_call(egress, key, ep, "resources/read", json!({ "uri": filled })).await?;
            Ok(extract_mcp_result("resources/read", result))
        }
        _ => Err("remote op does not match endpoint kind".to_owned()),
    }
}

/// Execute an OpenAPI/REST call: fill the `{name}` path placeholders (from the overlay id +
/// payload scalars, every value percent-encoded), join to the base URL, apply auth, attach the
/// JSON body (a rendered `body:` template if declared, else the raw payload), and parse the JSON
/// response. Under-specified path/body templates fail closed.
#[allow(clippy::too_many_arguments)]
async fn http_call(
    egress: &Egress,
    key: &str,
    ep: &EndpointRecord,
    method: &str,
    path: &str,
    body_template: Option<&Value>,
    id: Option<&str>,
    payload: Option<&Value>,
) -> Result<Value, String> {
    let vars = template_vars(id, payload);
    let filled = fill_path_template(path, &vars);
    let missing = unfilled_placeholders(&filled);
    if !missing.is_empty() {
        return Err(format!("unfilled path placeholders: {missing:?}"));
    }
    let http_method = reqwest::Method::from_bytes(method.as_bytes())
        .map_err(|_| format!("invalid HTTP method `{method}`"))?;
    let json_body: Option<Value> = if let Some(tpl) = body_template {
        // A declared body template reshapes the payload; unresolved
        // placeholders fail the write closed rather than send a literal `{x}`.
        let (rendered, missing) = render_body(tpl, id, payload.unwrap_or(&Value::Null));
        if !missing.is_empty() {
            return Err(format!("unfilled body placeholders: {missing:?}"));
        }
        Some(rendered)
    } else {
        payload.cloned()
    };
    let (status, bytes) = send_path(egress, key, ep, &filled, |c, url| {
        let r = c.request(http_method, url);
        match &json_body {
            Some(b) => r.json(b),
            None => r,
        }
    })
    .await?;
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    if !status.is_success() {
        // The upstream's error body is untrusted text; only the status is reported.
        return Err(format!("upstream status {}", status.as_u16()));
    }
    Ok(body)
}

/// Execute a JSON-RPC 2.0 MCP call over HTTP to the endpoint's `/mcp` URL and
/// return the `result` object (or an error string for a JSON-RPC error /
/// non-2xx / transport failure).
async fn mcp_call(
    egress: &Egress,
    key: &str,
    ep: &EndpointRecord,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    let rpc = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
    let (status, bytes) = send(egress, key, ep, |c, url| {
        c.post(url)
            .header("accept", "application/json, text/event-stream")
            .json(&rpc)
    })
    .await?;
    let body: Value = serde_json::from_slice(&bytes)
        .map_err(|_| "invalid JSON-RPC response from the upstream".to_owned())?;
    if let Some(err) = body.get("error") {
        // The message is the upstream's text: bounded, and never forwarded as an instruction.
        let msg = err
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("error");
        return Err(format!(
            "mcp error: {}",
            msg.chars().take(200).collect::<String>()
        ));
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

/// Apply the endpoint's auth to a request builder. The secret is resolved here, at call time,
/// from its reference; it is never stored in a page and never appears in an error.
fn apply_auth(
    req: reqwest::RequestBuilder,
    ep: &EndpointRecord,
) -> Result<reqwest::RequestBuilder, String> {
    let secret = match (&ep.auth, &ep.secret) {
        (EndpointAuth::None, _) | (_, None) => return Ok(req),
        (_, Some(raw)) => resolve_secret(raw)?,
    };
    Ok(match &ep.auth {
        EndpointAuth::None => req,
        EndpointAuth::Bearer => req.bearer_auth(secret),
        EndpointAuth::ApiKey { header } => req.header(header.as_str(), secret),
    })
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

    #[test]
    fn a_file_reference_is_read_and_trimmed() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("token");
        std::fs::write(&f, "  s3cr3t-value \n").unwrap();
        assert_eq!(
            resolve_secret(&format!("file:{}", f.display())).unwrap(),
            "s3cr3t-value"
        );
    }

    #[test]
    fn a_missing_reference_names_itself_and_nothing_else() {
        let e = resolve_secret("file:/definitely/not/here").unwrap_err();
        assert_eq!(
            e,
            "secret reference `file:/definitely/not/here` is not available"
        );
        let e = resolve_secret("env:ESCUREL_SURELY_UNSET_VAR_X").unwrap_err();
        assert!(e.contains("ESCUREL_SURELY_UNSET_VAR_X") && e.contains("not available"));
    }

    #[test]
    fn env_and_gsm_references_resolve_from_the_environment() {
        // PATH is set in every environment this runs in; gsm: maps to ESCUREL_SECRET_<NAME>.
        assert!(!resolve_secret("env:PATH").unwrap().is_empty());
        assert!(resolve_secret("gsm:some-secret.name").is_err());
    }

    #[test]
    fn an_inline_value_is_returned_as_is() {
        assert_eq!(
            resolve_secret("plain-dev-token").unwrap(),
            "plain-dev-token"
        );
    }
}
