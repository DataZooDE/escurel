//! Frontmatter post-filter clauses.
//!
//! The `search` tool accepts an optional `filter` object applied to
//! each hit's frontmatter *after* retrieval (`docs/spec/protocol.md`
//! §search). A clause is one of:
//!
//! - **equality** — `{"tier": "gold"}` (scalar match)
//! - **null** — `{"prev_review": null}` (field absent or JSON null)
//! - **operators** — `{"at": {">=": "2026-04-01"}}`, also `<=`, `>`,
//!   `<`, `in` (membership in an array), `not` (inequality)
//!
//! All clauses are ANDed. An empty / non-object filter matches
//! everything. Numbers compare numerically; other scalars compare by
//! their string form (ISO-8601 dates sort lexically).

use std::cmp::Ordering;

use serde_json::Value;

/// True iff `frontmatter` satisfies every clause in `filter`.
#[must_use]
pub fn matches_filter(filter: &Value, frontmatter: &Value) -> bool {
    let Some(clauses) = filter.as_object() else {
        return true;
    };
    clauses
        .iter()
        .all(|(key, pred)| clause_matches(frontmatter.get(key), pred))
}

fn clause_matches(field: Option<&Value>, pred: &Value) -> bool {
    match pred {
        // A `null` clause asserts the field is absent or JSON null.
        Value::Null => field.is_none_or(Value::is_null),
        // An object is a set of ANDed operator clauses.
        Value::Object(ops) => ops
            .iter()
            .all(|(op, operand)| op_matches(field, op, operand)),
        // Any other scalar is an equality assertion.
        scalar => field == Some(scalar),
    }
}

fn op_matches(field: Option<&Value>, op: &str, operand: &Value) -> bool {
    // Every operator except an explicit null-equality needs a value.
    let Some(field) = field else {
        return false;
    };
    match op {
        ">=" => cmp(field, operand).is_some_and(Ordering::is_ge),
        "<=" => cmp(field, operand).is_some_and(Ordering::is_le),
        ">" => cmp(field, operand).is_some_and(Ordering::is_gt),
        "<" => cmp(field, operand).is_some_and(Ordering::is_lt),
        "in" => operand
            .as_array()
            .is_some_and(|arr| arr.iter().any(|v| v == field)),
        "not" => field != operand,
        // Unknown operator: don't exclude on a clause we can't read.
        _ => true,
    }
}

/// Order two JSON scalars: numerically when both are numbers, else by
/// string form. Returns `None` for incomparable shapes.
fn cmp(a: &Value, b: &Value) -> Option<Ordering> {
    if let (Some(x), Some(y)) = (a.as_f64(), b.as_f64()) {
        return x.partial_cmp(&y);
    }
    match (a.as_str(), b.as_str()) {
        (Some(x), Some(y)) => Some(x.cmp(y)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fm() -> Value {
        json!({"tier": "gold", "at": "2026-04-12", "score": 7, "region": "emea"})
    }

    #[test]
    fn equality_matches_and_misses() {
        assert!(matches_filter(&json!({"tier": "gold"}), &fm()));
        assert!(!matches_filter(&json!({"tier": "silver"}), &fm()));
    }

    #[test]
    fn date_range_operators() {
        assert!(matches_filter(&json!({"at": {">=": "2026-04-01"}}), &fm()));
        assert!(!matches_filter(&json!({"at": {">=": "2026-05-01"}}), &fm()));
        assert!(matches_filter(&json!({"at": {"<": "2026-05-01"}}), &fm()));
    }

    #[test]
    fn numeric_operators_compare_numerically() {
        assert!(matches_filter(&json!({"score": {">=": 5}}), &fm()));
        assert!(!matches_filter(&json!({"score": {">": 7}}), &fm()));
    }

    #[test]
    fn in_and_not_and_null() {
        assert!(matches_filter(
            &json!({"region": {"in": ["emea", "apac"]}}),
            &fm()
        ));
        assert!(!matches_filter(&json!({"region": {"in": ["apac"]}}), &fm()));
        assert!(matches_filter(&json!({"region": {"not": "apac"}}), &fm()));
        assert!(matches_filter(&json!({"prev_review": null}), &fm()));
        assert!(!matches_filter(&json!({"tier": null}), &fm()));
    }

    #[test]
    fn empty_filter_matches_everything() {
        assert!(matches_filter(&json!({}), &fm()));
    }

    #[test]
    fn multiple_clauses_are_anded() {
        assert!(matches_filter(
            &json!({"tier": "gold", "score": {">=": 7}}),
            &fm()
        ));
        assert!(!matches_filter(
            &json!({"tier": "gold", "score": {">": 7}}),
            &fm()
        ));
    }
}
