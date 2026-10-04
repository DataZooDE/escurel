//! Golden wire files, decoded by the typed Rust structs.
//!
//! `tests/golden/*.json` are captured from a REAL gateway and runner by `scripts/refresh-golden.sh`.
//! The same files are decoded by the TypeScript extension's vitest suite
//! (`editors/vscode/test/unit/golden.test.ts`) and the Dart kit's tests, so the three hand-copied
//! implementations of the wire cannot drift apart without a test going red.
//!
//! The check is "nothing on the wire is silently dropped": the golden file must decode into the typed
//! struct, and every key the gateway sent must come back out when the struct is serialised again.
//! (`#[serde(default)]` structs ignore unknown keys, which is exactly how a new wire field goes missing.)

use escurel_types::*;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::path::PathBuf;

fn golden(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(format!("{name}.json"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e} (run scripts/refresh-golden.sh)", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Every key the golden has, with the same value, is in `got` (a `null` may be absent; numbers compare by value).
fn missing(path: &str, want: &Value, got: &Value, out: &mut Vec<String>) {
    match (want, got) {
        (Value::Null, _) => {}
        (Value::Object(w), Value::Object(g)) => {
            for (k, v) in w {
                match g.get(k) {
                    Some(gv) => missing(&format!("{path}.{k}"), v, gv, out),
                    None if v.is_null() => {}
                    None => out.push(format!("{path}.{k}")),
                }
            }
        }
        (Value::Array(w), Value::Array(g)) => {
            if w.len() != g.len() {
                out.push(format!("{path}[len {} != {}]", w.len(), g.len()));
            }
            for (i, (a, b)) in w.iter().zip(g).enumerate() {
                missing(&format!("{path}[{i}]"), a, b, out);
            }
        }
        (Value::Number(a), Value::Number(b)) => {
            if a.as_f64() != b.as_f64() {
                out.push(format!("{path} ({a} != {b})"));
            }
        }
        (a, b) => {
            if a != b {
                out.push(format!("{path} ({a} != {b})"));
            }
        }
    }
}

/// Decode `value` as `T` and report the golden paths that did not survive.
fn dropped<T: DeserializeOwned + Serialize>(value: &Value) -> Vec<String> {
    let typed: T = serde_json::from_value(value.clone())
        .unwrap_or_else(|e| panic!("does not decode as {}: {e}", std::any::type_name::<T>()));
    let back = serde_json::to_value(&typed).unwrap();
    let mut out = Vec::new();
    missing("$", value, &back, &mut out);
    out
}

fn structured(result: &Value) -> &Value {
    &result["structuredContent"]
}

#[test]
fn list_skills_is_decoded_without_loss() {
    let v = golden("list_skills");
    assert_eq!(dropped::<ListSkillsResponse>(&v), Vec::<String>::new());
    assert!(!v["skills"].as_array().unwrap().is_empty());
}

#[test]
fn expand_instance_is_decoded_without_loss() {
    let v = golden("expand_instance");
    assert_eq!(dropped::<ExpandResponse>(&v), Vec::<String>::new());
}

#[test]
fn list_instances_is_decoded_without_loss() {
    let v = golden("list_instances");
    assert_eq!(dropped::<ListInstancesResponse>(&v), Vec::<String>::new());
}

#[test]
fn a_refusal_is_an_error_result_that_carries_issues() {
    let r = golden("refusal_result");
    assert_eq!(
        r["isError"],
        Value::Bool(true),
        "a rejected write is an error result"
    );
    assert!(
        r["content"][0]["text"].is_string(),
        "with a short text summary"
    );
    let issues = structured(&r)["issues"].as_array().expect("and the issues");
    assert!(!issues.is_empty());
    assert_eq!(
        dropped::<ValidateResponse>(structured(&r)),
        Vec::<String>::new()
    );
}

#[test]
fn validate_reports_ok_false_with_issues_as_data() {
    let r = golden("validate_result");
    assert_ne!(
        r["isError"],
        Value::Bool(true),
        "validate problems are data, not an error result"
    );
    assert_eq!(structured(&r)["ok"], Value::Bool(false));
    assert_eq!(
        dropped::<ValidateResponse>(structured(&r)),
        Vec::<String>::new()
    );
}

/// Wire fields a Rust struct does not carry yet. The test FAILS when a gap closes, so the entry is
/// removed rather than forgotten.
const KNOWN_GAPS: &[(&str, &[&str])] = &[(
    "update_page_held",
    // `held_for_review` and `draft` are typed now (a typed client no longer reads a held write as
    // landed). Still not carried: the human-readable `message` and the draft's `base_version`.
    &["$.draft.base_version", "$.message"],
)];

#[test]
fn a_held_write_says_so() {
    let v = golden("update_page_held");
    assert_eq!(
        v["held_for_review"],
        Value::Bool(true),
        "the gateway reports the write was held"
    );
    assert_eq!(v["draft"]["status"], "open", "and names the open draft");
    let lost = dropped::<UpdatePageResponse>(&v);
    let known = KNOWN_GAPS
        .iter()
        .find(|(n, _)| *n == "update_page_held")
        .unwrap()
        .1;
    assert_eq!(
        lost.iter().map(String::as_str).collect::<Vec<_>>(),
        known,
        "update_page_held loses {lost:?}; if the typed response now carries them, delete the KNOWN_GAPS entry"
    );
}

#[test]
fn run_events_are_decoded_without_loss() {
    let v = golden("events_run");
    assert_eq!(dropped::<ListEventsResponse>(&v), Vec::<String>::new());
    let titles: Vec<&str> = v["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["title"].as_str())
        .collect();
    assert!(
        titles.contains(&"run-started") && titles.contains(&"run-finished"),
        "{titles:?}"
    );
}

#[test]
fn an_inbox_event_is_decoded_without_loss() {
    let v = golden("inbox_event");
    assert_eq!(dropped::<Event>(&v["event"]), Vec::<String>::new());
}
