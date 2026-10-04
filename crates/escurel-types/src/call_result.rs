//! Reading an MCP `tools/call` result — ONE place, so no consumer can read the payload and ignore the
//! refusal flag.
//!
//! The gateway answers a `tools/call` with a spec `CallToolResult`:
//! `{content: [{type: "text", text: <short summary>}], structuredContent: <full payload>, isError}`.
//! A tool that REFUSES (a domain refusal: ACL denial, `invalid_limit`, `query_not_found`,
//! `endpoint_not_registered`, a write that fails validation, ...) sets `isError: true` and a payload of
//! `{ok: false, issues: [{code, location, message, suggestion?}]}`.
//!
//! Reading `structuredContent` blindly turns that refusal into data: a response type whose fields all
//! default (a page of instances, a row set) decodes `{ok: false, ...}` into an EMPTY SUCCESS, i.e. a
//! silent partial read after an access denial. Every Rust consumer of `tools/call` results goes through
//! [`unwrap_call_result`] (a guard test in `escurel-client` fails if a new one reads the field itself).

use serde_json::Value;

use crate::ValidationIssue;

/// A tool's refusal, as the gateway reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct Refusal {
    /// The issues the tool named (code, location, message, suggestion). Never empty: a refusal with no
    /// issue is given one built from the result's text, so the caller always has a reason.
    pub issues: Vec<ValidationIssue>,
    /// The full refused payload, for callers that want the rest of it.
    pub payload: Value,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut first = true;
        for i in &self.issues {
            if !first {
                write!(f, "; ")?;
            }
            first = false;
            write!(f, "{}", i.code)?;
            if !i.location.is_empty() {
                write!(f, " at {}", i.location)?;
            }
            if !i.message.is_empty() {
                write!(f, ": {}", i.message)?;
            }
            if let Some(s) = &i.suggestion {
                write!(f, " ({s})")?;
            }
        }
        Ok(())
    }
}

/// The payload of a `CallToolResult`: `structuredContent` (what every current gateway sends, with a
/// short summary in `content[0].text`); for a LEGACY gateway that put the payload in the text block as
/// JSON, that text parsed; else the result as it is.
#[must_use]
pub fn payload_of(result: &Value) -> Value {
    if let Some(sc) = result.get("structuredContent") {
        return sc.clone();
    }
    if let Some(text) = result["content"][0]["text"].as_str()
        && let Ok(parsed @ Value::Object(_)) = serde_json::from_str(text)
    {
        return parsed;
    }
    result.clone()
}

/// `Some` when the tool REFUSED: `isError: true` (the gateway's flag for `ok: false`), or an explicit
/// `ok: false` with issues. `None` for a success.
#[must_use]
pub fn refusal_of(result: &Value) -> Option<Refusal> {
    let payload = payload_of(result);
    let flagged = result.get("isError").and_then(Value::as_bool) == Some(true);
    let ok_false = payload.get("ok").and_then(Value::as_bool) == Some(false);
    if !flagged && !ok_false {
        return None;
    }
    let mut issues: Vec<ValidationIssue> = payload
        .get("issues")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|i| ValidationIssue {
                    severity: text(i, "severity").unwrap_or_else(|| "error".to_owned()),
                    code: text(i, "code").unwrap_or_default(),
                    location: text(i, "location").unwrap_or_default(),
                    message: text(i, "message").unwrap_or_default(),
                    suggestion: text(i, "suggestion"),
                })
                .collect()
        })
        .unwrap_or_default();
    if issues.is_empty() {
        // A refusal always carries a reason: the text block, else a generic code.
        let message = result["content"][0]["text"]
            .as_str()
            .unwrap_or("the tool refused the call")
            .to_owned();
        issues.push(ValidationIssue {
            severity: "error".to_owned(),
            code: "tool_error".to_owned(),
            location: String::new(),
            message,
            suggestion: None,
        });
    }
    Some(Refusal { issues, payload })
}

/// The payload of a SUCCESSFUL result, or the [`Refusal`]. The only way a read consumer should open a
/// `tools/call` result.
///
/// # Errors
/// The tool refused the call (`isError`, or `ok: false` with issues).
pub fn unwrap_call_result(result: Value) -> Result<Value, Refusal> {
    match refusal_of(&result) {
        Some(r) => Err(r),
        None => Ok(payload_of(&result)),
    }
}

fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_success_is_its_structured_payload() {
        let r = json!({ "content": [{"type":"text","text":"3 events."}], "structuredContent": {"events":[1,2,3]}, "isError": false });
        assert_eq!(unwrap_call_result(r).unwrap(), json!({"events":[1,2,3]}));
    }

    #[test]
    fn an_is_error_result_is_a_refusal_with_its_issues() {
        let r = json!({
            "isError": true,
            "content": [{"type":"text","text":"refused: invalid_limit"}],
            "structuredContent": {"ok": false, "issues": [
                {"severity":"error","code":"invalid_limit","location":"arguments.limit","message":"limit is an integer from 1 to 10000; got 0","suggestion":"pass limit: 100"}
            ]}
        });
        let e = unwrap_call_result(r).unwrap_err();
        assert_eq!(e.issues.len(), 1);
        assert_eq!(e.issues[0].code, "invalid_limit");
        assert_eq!(e.issues[0].suggestion.as_deref(), Some("pass limit: 100"));
        assert!(e.to_string().contains("invalid_limit at arguments.limit"));
    }

    #[test]
    fn ok_false_without_the_flag_is_still_a_refusal() {
        let r = json!({ "structuredContent": {"ok": false, "issues": [{"code":"conflict","message":"x"}]} });
        assert_eq!(
            unwrap_call_result(r).unwrap_err().issues[0].code,
            "conflict"
        );
    }

    #[test]
    fn a_refusal_with_no_issue_still_carries_a_reason() {
        let r = json!({ "isError": true, "content": [{"type":"text","text":"boom"}] });
        let e = unwrap_call_result(r).unwrap_err();
        assert_eq!(e.issues[0].code, "tool_error");
        assert_eq!(e.issues[0].message, "boom");
    }

    #[test]
    fn a_legacy_gateway_text_payload_is_parsed_and_a_summary_is_not_mistaken_for_one() {
        let legacy = json!({ "content": [{"type":"text","text":"{\"rows\":[1]}"}] });
        assert_eq!(unwrap_call_result(legacy).unwrap(), json!({"rows":[1]}));
        let summary = json!({ "content": [{"type":"text","text":"3 events. Full result in structuredContent."}] });
        assert_eq!(unwrap_call_result(summary.clone()).unwrap(), summary);
    }

    #[test]
    fn a_validate_style_ok_false_payload_that_is_not_flagged_is_still_surfaced_by_refusal_of() {
        // `validate` reports problems with ok:false but is not an error; the typed `validate` method
        // reads it through `payload_of`, never through `unwrap_call_result`.
        let r = json!({ "isError": false, "structuredContent": {"ok": false, "issues": [{"code":"x"}]} });
        assert!(refusal_of(&r).is_some());
        assert_eq!(payload_of(&r)["issues"][0]["code"], "x");
    }
}
