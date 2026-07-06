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

/// Build the `{name}` template variables for a path / URI: the overlay instance
/// id (`{id}`) plus every **scalar** leaf of the write `payload`, flattened to
/// dotted keys (`{order_id}`, `{customer.tier}`). Reads pass `payload = None`,
/// binding only `{id}`. The overlay id always wins over a payload field named
/// `id` (it is the instance identity, not caller-supplied data).
#[must_use]
pub fn template_vars(id: Option<&str>, payload: Option<&Value>) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    if let Some(p) = payload {
        flatten_scalars(p, "", &mut out);
    }
    if let Some(id) = id {
        out.insert("id".to_owned(), id.to_owned());
    }
    out
}

/// Flatten an object's scalar leaves into dotted keys, e.g.
/// `{a:{b:1}, c:"x"}` → `{"a.b":"1", "c":"x"}`. Arrays and `null` are skipped
/// (they cannot be substituted into a URL segment).
fn flatten_scalars(v: &Value, prefix: &str, out: &mut BTreeMap<String, String>) {
    match v {
        Value::Object(m) => {
            for (k, val) in m {
                let key = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                flatten_scalars(val, &key, out);
            }
        }
        _ if prefix.is_empty() => {}
        Value::String(s) => {
            out.insert(prefix.to_owned(), s.clone());
        }
        Value::Number(n) => {
            out.insert(prefix.to_owned(), n.to_string());
        }
        Value::Bool(b) => {
            out.insert(prefix.to_owned(), b.to_string());
        }
        Value::Null | Value::Array(_) => {}
    }
}

/// Render a write `body:` template against the overlay id + write payload.
///
/// - An **exact** `"{name}"` string leaf is replaced by the resolved JSON value
///   *type-preserving* (a number stays a number, an object stays an object).
/// - Any other string is textually interpolated (`{name}` → the scalar value's
///   string form); a placeholder resolving to a non-scalar is treated as
///   unresolved.
/// - Arrays / objects recurse; non-string scalars pass through unchanged.
///
/// Placeholders resolve against the overlay id (`{id}`) and the payload (dotted,
/// e.g. `{customer.id}`). Returns the rendered body and the placeholders that
/// could not be resolved (a non-empty list ⇒ the caller fails the write closed).
#[must_use]
pub fn render_body(template: &Value, id: Option<&str>, payload: &Value) -> (Value, Vec<String>) {
    let mut missing = Vec::new();
    let out = render_value(template, id, payload, &mut missing);
    (out, missing)
}

fn render_value(t: &Value, id: Option<&str>, payload: &Value, missing: &mut Vec<String>) -> Value {
    match t {
        Value::String(s) => render_string_leaf(s, id, payload, missing),
        Value::Array(a) => Value::Array(
            a.iter()
                .map(|x| render_value(x, id, payload, missing))
                .collect(),
        ),
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, v)| (k.clone(), render_value(v, id, payload, missing)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn render_string_leaf(
    s: &str,
    id: Option<&str>,
    payload: &Value,
    missing: &mut Vec<String>,
) -> Value {
    // Exact `"{name}"` → the resolved value, keeping its JSON type.
    if let Some(name) = exact_placeholder(s) {
        return match resolve_typed(name, id, payload) {
            Some(v) => v,
            None => {
                missing.push(name.to_owned());
                Value::String(s.to_owned())
            }
        };
    }
    // Otherwise interpolate `{name}` occurrences as strings.
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let Some(rel) = rest[open..].find('}') else {
            out.push_str(&rest[open..]);
            return Value::String(out);
        };
        let close = open + rel;
        let name = &rest[open + 1..close];
        match resolve_typed(name, id, payload)
            .as_ref()
            .and_then(scalar_str)
        {
            Some(val) => out.push_str(&val),
            None => {
                missing.push(name.to_owned());
                out.push_str(&rest[open..=close]);
            }
        }
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    Value::String(out)
}

/// `Some(name)` iff `s` is exactly `{name}` with no other braces.
fn exact_placeholder(s: &str) -> Option<&str> {
    s.strip_prefix('{')
        .and_then(|r| r.strip_suffix('}'))
        .filter(|name| !name.contains('{') && !name.contains('}'))
}

/// Resolve a placeholder to a JSON value: `{id}` from the overlay id, else a
/// (dotted) lookup into the payload.
fn resolve_typed(name: &str, id: Option<&str>, payload: &Value) -> Option<Value> {
    if name == "id" {
        return id.map(|s| Value::String(s.to_owned()));
    }
    json_path_get(payload, name).cloned()
}

/// A scalar rendered as its string form; non-scalars are not interpolatable.
fn scalar_str(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
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

    #[test]
    fn template_vars_binds_id_and_flattened_payload_scalars() {
        let payload = json!({
            "order_id": "o-9",
            "qty": 3,
            "customer": { "tier": "gold" },
            "tags": ["a", "b"],   // arrays skipped
            "note": null           // null skipped
        });
        let v = template_vars(Some("acme"), Some(&payload));
        assert_eq!(v.get("id").map(String::as_str), Some("acme"));
        assert_eq!(v.get("order_id").map(String::as_str), Some("o-9"));
        assert_eq!(v.get("qty").map(String::as_str), Some("3"));
        assert_eq!(v.get("customer.tier").map(String::as_str), Some("gold"));
        assert!(!v.contains_key("tags") && !v.contains_key("note"));

        // A multi-placeholder path resolves from the merged map.
        let filled = fill_template("/customers/{id}/orders/{order_id}", &v);
        assert_eq!(filled, "/customers/acme/orders/o-9");
        assert!(unfilled_placeholders(&filled).is_empty());
    }

    #[test]
    fn template_vars_overlay_id_wins_over_payload_id() {
        let payload = json!({ "id": "forged" });
        let v = template_vars(Some("acme"), Some(&payload));
        assert_eq!(v.get("id").map(String::as_str), Some("acme"));
    }

    #[test]
    fn render_body_exact_placeholder_is_type_preserving() {
        let template = json!({
            "order_id": "{order_id}",
            "qty": "{qty}",
            "customer": "{customer}",
            "label": "order {order_id} x{qty}",
            "source": "escurel"
        });
        let payload = json!({
            "order_id": "o-9",
            "qty": 3,
            "customer": { "tier": "gold" }
        });
        let (out, missing) = render_body(&template, Some("acme"), &payload);
        assert!(missing.is_empty(), "unexpected missing: {missing:?}");
        // exact "{qty}" keeps the number type; "{customer}" keeps the object.
        assert_eq!(out["order_id"], json!("o-9"));
        assert_eq!(out["qty"], json!(3));
        assert_eq!(out["customer"], json!({ "tier": "gold" }));
        // embedded placeholders interpolate as strings.
        assert_eq!(out["label"], json!("order o-9 x3"));
        assert_eq!(out["source"], json!("escurel"));
    }

    #[test]
    fn render_body_reports_unresolved_placeholders() {
        let template = json!({ "a": "{nope}", "b": "x-{alsonope}" });
        let (out, missing) = render_body(&template, Some("acme"), &json!({}));
        assert!(missing.contains(&"nope".to_owned()));
        assert!(missing.contains(&"alsonope".to_owned()));
        // unresolved placeholders are left literal (the caller fails closed).
        assert_eq!(out["a"], json!("{nope}"));
        assert_eq!(out["b"], json!("x-{alsonope}"));
    }

    #[test]
    fn render_body_binds_id_and_dotted_payload() {
        let template = json!({ "who": "{id}", "tier": "{customer.tier}" });
        let payload = json!({ "customer": { "tier": "gold" } });
        let (out, missing) = render_body(&template, Some("acme"), &payload);
        assert!(missing.is_empty());
        assert_eq!(out["who"], json!("acme"));
        assert_eq!(out["tier"], json!("gold"));
    }
}
