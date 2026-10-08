//! The bounded, redacted summary a run's tool-call row keeps of a call's arguments and result
//! (`ESCUREL_TOOLCALL_DETAIL=summary`).
//!
//! The point is a trace that can answer "what did the agent ask, and what came back" without
//! becoming a second copy of the tenant's data or a place secrets collect. So the summary keeps the
//! SHAPE and the identifying values (page ids, skills, filters, short strings) and drops or marks
//! the rest:
//!
//! - a key that names a credential (`*token*`, `*secret*`, `*password*`, `authorization`, …) keeps
//!   its key and loses its value (`"[redacted]"`);
//! - a string that looks like a credential (`Bearer …`, a JWT, `token=…`) is scrubbed in place;
//! - a content-carrying string (`content`, `body`, `markdown`, base64 blobs) becomes its size;
//! - any other long string, long array, wide object or deep nesting is cut with a count;
//! - the whole summary is capped at [`CAP`] bytes, cut on a character boundary and marked.

use serde_json::{Map, Value};

/// The most bytes one summary holds, marker included.
pub(crate) const CAP: usize = 2048;
const MARK: &str = "…[truncated]";
const REDACTED: &str = "[redacted]";
const MAX_STR_CHARS: usize = 160;
const MAX_ITEMS: usize = 8;
const MAX_KEYS: usize = 40;
const MAX_DEPTH: usize = 4;

/// Keys whose value is a credential, whatever the string looks like.
const SECRET_KEY_PARTS: &[&str] = &[
    "token",
    "secret",
    "password",
    "passwd",
    "authorization",
    "api_key",
    "apikey",
    "api-key",
    "credential",
    "bearer",
    "cookie",
    "private_key",
];

/// Keys whose string value is content, not an identifier: kept as a size only.
const CONTENT_KEYS: &[&str] = &[
    "content",
    "body",
    "markdown",
    "tarball_b64",
    "data_b64",
    "bytes_b64",
    "text",
];

fn is_secret_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    SECRET_KEY_PARTS.iter().any(|p| k.contains(p))
}

fn is_content_key(key: &str) -> bool {
    CONTENT_KEYS.contains(&key.to_ascii_lowercase().as_str())
}

/// A summary of `value`: reduced, then serialised compactly and capped.
pub(crate) fn summarise(value: &Value) -> String {
    cap(&reduce(value, 0).to_string())
}

/// A summary of a failure's message (`code: message`), scrubbed and capped.
pub(crate) fn summarise_message(message: &str) -> String {
    cap(&scrub_str(message))
}

fn reduce(value: &Value, depth: usize) -> Value {
    match value {
        Value::String(s) => Value::String(shorten(&scrub_str(s))),
        Value::Array(items) => {
            if depth >= MAX_DEPTH {
                return Value::String(format!("[{} items]", items.len()));
            }
            let mut out: Vec<Value> = items
                .iter()
                .take(MAX_ITEMS)
                .map(|v| reduce(v, depth + 1))
                .collect();
            if items.len() > MAX_ITEMS {
                out.push(Value::String(format!(
                    "[+{} more]",
                    items.len() - MAX_ITEMS
                )));
            }
            Value::Array(out)
        }
        Value::Object(map) => {
            if depth >= MAX_DEPTH {
                return Value::String(format!("{{{} keys}}", map.len()));
            }
            let mut out = Map::new();
            for (k, v) in map.iter().take(MAX_KEYS) {
                let reduced = if is_secret_key(k) {
                    Value::String(REDACTED.to_owned())
                } else if let (true, Value::String(s)) = (is_content_key(k), v) {
                    Value::String(format!("[{} bytes]", s.len()))
                } else {
                    reduce(v, depth + 1)
                };
                out.insert(k.clone(), reduced);
            }
            if map.len() > MAX_KEYS {
                out.insert(
                    "…".to_owned(),
                    Value::String(format!("+{} more keys", map.len() - MAX_KEYS)),
                );
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

/// A long string cut to [`MAX_STR_CHARS`] characters, with how much was dropped.
fn shorten(s: &str) -> String {
    let n = s.chars().count();
    if n <= MAX_STR_CHARS {
        return s.to_owned();
    }
    let head: String = s.chars().take(MAX_STR_CHARS).collect();
    format!("{head}…({n} chars)")
}

/// Credentials inside free text: `Bearer <x>`, a JWT, `token=<x>`. Whitespace is kept as it was.
fn scrub_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    // The previous word named a credential (`Bearer`, `token:`): this one is its value.
    let mut value_next = false;
    for piece in s.split_inclusive(char::is_whitespace) {
        let word = piece.trim_end_matches(char::is_whitespace);
        let space = &piece[word.len()..];
        if word.is_empty() {
            out.push_str(space);
            continue;
        }
        let at_sep = word.find(['=', ':']);
        let scrubbed = if value_next || looks_like_jwt(word) {
            REDACTED.to_owned()
        } else if let Some(i) = at_sep
            && i > 0
            && is_secret_key(&word[..i])
            && i + 1 < word.len()
        {
            format!("{}{REDACTED}", &word[..=i])
        } else {
            word.to_owned()
        };
        let label = word.trim_end_matches([':', '=']);
        value_next = matches!(label.to_ascii_lowercase().as_str(), "bearer" | "basic")
            || (label.len() < word.len() && is_secret_key(label));
        out.push_str(&scrubbed);
        out.push_str(space);
    }
    out
}

fn looks_like_jwt(word: &str) -> bool {
    word.starts_with("eyJ") && word.matches('.').count() >= 2
}

/// `s` capped at [`CAP`] bytes, cut on a character boundary, with the marker inside the cap.
fn cap(s: &str) -> String {
    if s.len() <= CAP {
        return s.to_owned();
    }
    let mut end = CAP - MARK.len();
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{MARK}", &s[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn credential_keys_and_strings_are_redacted_ids_stay() {
        let s = summarise(&json!({
            "page_id": "markdown/instances/note/n1.md",
            "api_token": "T0P",
            "nested": { "Authorization": "Bearer X", "password": "p" },
            "note": "call with Bearer abc.def and token=hunter2 please",
        }));
        assert!(s.contains("markdown/instances/note/n1.md"), "{s}");
        for leaked in ["T0P", "Bearer X", "abc.def", "hunter2", "\"p\""] {
            assert!(!s.contains(leaked), "{leaked} in {s}");
        }
    }

    #[test]
    fn content_becomes_a_size_and_long_things_are_cut() {
        let s =
            summarise(&json!({ "body": "x".repeat(5000), "list": (0..20).collect::<Vec<_>>() }));
        assert!(s.contains("[5000 bytes]"), "{s}");
        assert!(s.contains("[+12 more]"), "{s}");
    }

    #[test]
    fn the_cap_cuts_on_a_character_boundary() {
        let big = json!({ "k": "é".repeat(5000) });
        let reduced = cap(&format!("{{\"k\":\"{}\"}}", "é".repeat(5000)));
        assert!(reduced.len() <= CAP && reduced.ends_with(MARK));
        assert!(summarise(&big).len() <= CAP);
    }
}
