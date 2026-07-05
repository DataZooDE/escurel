//! Live remote (proxy) backends: `openapi` + `mcp`.
//!
//! These two backends share one model — an instance is a **live window onto a
//! remote object**. Its identity, links, ACL, and history are ordinary page
//! machinery (the markdown overlay page), but its body/data is fetched **live
//! on `expand`** (nothing materialised in DuckDB) and edits are forwarded
//! upstream via the explicit `write_instance` tool. `openapi` proxies a
//! REST/HTTP endpoint; `mcp` proxies an upstream MCP server (escurel is the
//! client). The base URL + auth live in the admin [`endpoint
//! registry`](crate::endpoints), never in tenant markdown (the SSRF /
//! secrets-in-markdown guard).
//!
//! This module holds the **deterministic core** both transports share —
//! projection resolution and path/URI templating — kept pure so it is
//! unit-testable without a network. The live transport ([`RemoteClient`]) is
//! implemented over the pure request/response DTOs below.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

/// Failure modes of a live remote read/write. Surfaced on `expand` as a
/// `backend_projection.issue` (the read path never returns a partial or
/// fabricated body — a hard failure is an `Issue`, mirroring the SQL-view
/// `binding_degraded` policy) and on `write_instance` as an error.
#[derive(Debug, thiserror::Error)]
pub enum RemoteError {
    /// The skill's `backend.endpoint` names no registered endpoint.
    #[error("remote endpoint `{0}` is not registered")]
    UnknownEndpoint(String),
    /// The binding is missing / malformed (fail-closed at create/read).
    #[error("remote binding is missing or invalid: {0}")]
    BadBinding(String),
    /// The op declared in the binding does not match the endpoint's protocol
    /// (e.g. an `mcp` op against an `openapi` endpoint).
    #[error("remote op does not match endpoint kind: {0}")]
    OpMismatch(String),
    /// The upstream returned a non-success status or an MCP error.
    #[error("remote call failed: {0}")]
    Upstream(String),
    /// The transport (DNS/TLS/timeout) failed.
    #[error("remote transport error: {0}")]
    Transport(String),
}

/// Resolve a `project:` map against a remote response: for each
/// `(overlay_field, path)`, extract `path` from `resp` and bind it to
/// `overlay_field`. A path that does not resolve is skipped (the overlay
/// field is simply absent), never an error — a shape drift narrows the
/// projection rather than failing the read.
#[must_use]
pub fn resolve_projection(resp: &Value, project: &BTreeMap<String, String>) -> Map<String, Value> {
    let mut out = Map::new();
    for (field, path) in project {
        if let Some(v) = json_path_get(resp, path) {
            out.insert(field.clone(), v.clone());
        }
    }
    out
}

/// Read a value out of `v` by a dotted JSON path. Accepts a leading `$` /
/// `$.` (JSONPath-lite) or a bare dotted key; `$` alone (or an empty path)
/// returns the whole value. Only object member access is supported (no array
/// indexing / filters) — enough for the projection maps a binding declares.
#[must_use]
pub fn json_path_get<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    let trimmed = path
        .strip_prefix("$.")
        .or_else(|| path.strip_prefix('$'))
        .unwrap_or(path);
    if trimmed.is_empty() {
        return Some(v);
    }
    let mut cur = v;
    for seg in trimmed.split('.') {
        cur = cur.get(seg)?;
    }
    Some(cur)
}

/// Fill `{name}` placeholders in a path/URI template from `vars`. A
/// placeholder with no matching var is left intact so the caller can detect
/// an under-specified template ([`unfilled_placeholders`]). No escaping is
/// performed here — the transport is responsible for URL-encoding the
/// substituted values.
#[must_use]
pub fn fill_template(template: &str, vars: &BTreeMap<String, String>) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        if let Some(rel_close) = rest[open..].find('}') {
            let close = open + rel_close;
            let name = &rest[open + 1..close];
            match vars.get(name) {
                Some(val) => out.push_str(val),
                None => out.push_str(&rest[open..=close]),
            }
            rest = &rest[close + 1..];
        } else {
            // Unbalanced `{` — emit the remainder verbatim.
            out.push_str(&rest[open..]);
            return out;
        }
    }
    out.push_str(rest);
    out
}

/// The `{name}` placeholders still present in a filled template — non-empty
/// means the template was under-specified (a `BadBinding` at call time).
#[must_use]
pub fn unfilled_placeholders(filled: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = filled;
    while let Some(open) = rest.find('{') {
        if let Some(rel_close) = rest[open..].find('}') {
            let close = open + rel_close;
            out.push(rest[open + 1..close].to_owned());
            rest = &rest[close + 1..];
        } else {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn vars(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn json_path_get_dotted_and_bare_and_root() {
        let v = json!({ "a": { "b": 7 }, "name": "acme" });
        assert_eq!(json_path_get(&v, "$.a.b"), Some(&json!(7)));
        assert_eq!(json_path_get(&v, "a.b"), Some(&json!(7)));
        assert_eq!(json_path_get(&v, "name"), Some(&json!("acme")));
        assert_eq!(json_path_get(&v, "$"), Some(&v));
        assert_eq!(json_path_get(&v, "$.missing"), None);
        assert_eq!(json_path_get(&v, "a.b.c"), None);
    }

    #[test]
    fn resolve_projection_maps_fields_and_skips_missing() {
        let resp = json!({ "name": "Acme", "account_tier": "gold", "nested": { "x": 1 } });
        let project = vars(&[
            ("display_name", "$.name"),
            ("tier", "$.account_tier"),
            ("deep", "$.nested.x"),
            ("absent", "$.nope"),
        ]);
        let out = resolve_projection(&resp, &project);
        assert_eq!(out.get("display_name"), Some(&json!("Acme")));
        assert_eq!(out.get("tier"), Some(&json!("gold")));
        assert_eq!(out.get("deep"), Some(&json!(1)));
        assert!(!out.contains_key("absent"), "unresolved path is skipped");
    }

    #[test]
    fn fill_template_substitutes_known_and_keeps_unknown() {
        assert_eq!(
            fill_template("/customers/{id}", &vars(&[("id", "acme-corp")])),
            "/customers/acme-corp"
        );
        assert_eq!(
            fill_template("/a/{x}/b/{y}", &vars(&[("x", "1"), ("y", "2")])),
            "/a/1/b/2"
        );
        // unknown placeholder is preserved so the caller can detect it
        let filled = fill_template("/customers/{id}", &vars(&[]));
        assert_eq!(filled, "/customers/{id}");
        assert_eq!(unfilled_placeholders(&filled), vec!["id".to_owned()]);
    }

    #[test]
    fn fill_template_handles_no_placeholders_and_unbalanced() {
        assert_eq!(fill_template("/plain", &vars(&[])), "/plain");
        assert_eq!(
            fill_template("/oops/{id", &vars(&[("id", "x")])),
            "/oops/{id"
        );
    }

    #[test]
    fn unfilled_placeholders_empty_when_all_bound() {
        let filled = fill_template("/c/{id}", &vars(&[("id", "z")]));
        assert!(unfilled_placeholders(&filled).is_empty());
    }
}
