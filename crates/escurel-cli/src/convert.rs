//! Proto → JSON projection helpers shared by every command.
//!
//! The CLI commits to a stable JSON shape (the contract an LLM or a
//! script parses), so these mappers are the single place that shape is
//! defined. Empty proto strings become JSON `null` rather than `""` so
//! "absent" is unambiguous downstream.

use escurel_client::{Event, PageRef};
use serde_json::{Value, json};

/// Empty string → `null`, otherwise the string.
pub fn opt(s: &str) -> Value {
    if s.is_empty() {
        Value::Null
    } else {
        Value::String(s.to_owned())
    }
}

/// Normalise a JSON value for the CLI's contract: `null` / empty object
/// / empty array collapse to `null`, everything else passes through. The
/// wire types now carry real JSON values, not strings.
pub fn json_or_null(v: &Value) -> Value {
    match v {
        Value::Null => Value::Null,
        Value::Object(m) if m.is_empty() => Value::Null,
        Value::Array(a) if a.is_empty() => Value::Null,
        other => other.clone(),
    }
}

pub fn page_ref(p: PageRef) -> Value {
    json!({
        "page_id": p.page_id,
        "slug": opt(&p.slug),
        "skill": p.skill,
        "page_type": p.page_type,
    })
}

pub fn event(e: Event) -> Value {
    json!({
        "event_id": e.event_id,
        "at": opt(&e.at),
        "source": e.source,
        "mime": e.mime,
        "label_skill": e.label_skill,
        "instance_page_id": opt(&e.instance_page_id),
        "status": e.status,
        "title": e.title,
        "body": e.body,
        "provenance": json_or_null(&e.provenance),
    })
}
