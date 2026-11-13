//! The run transcript (#822): what the agent did, as a replayable record.
//!
//! Each harness adapter normalises its own structured stream (claude
//! stream-json, codex `--json` events, the echo harness's scripted lines,
//! gemini's in-process turns) into [`TranscriptEntry`]s and hands them to
//! the attempt's [`TranscriptSink`] through
//! [`crate::TaskContext::transcribe`]. The sink turns an entry into the
//! stored [`TranscriptRecord`]:
//!
//! - arguments never leave the runner raw: a record keeps `args_digest`, the
//!   SHA-256 of the canonical (key-sorted, compact) args JSON, so two calls
//!   with the same arguments are recognisably the same and nothing else is
//!   disclosed;
//! - `summary` is a redacted preview, at most [`SUMMARY_MAX_CHARS`]
//!   characters, scrubbed of credential shapes and of the run's own bearer
//!   (`escurel_types::redact::scrub`) BEFORE it is cut, so a token cut in
//!   half cannot slip past the scrubber;
//! - `name` goes through the same scrubber.
//!
//! The runner collects every attempt's records (each carries its
//! `attempt`), uploads them as JSONL with `put_run_transcript`, and names the
//! blob on `run-finished.transcript`.

use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The longest `summary` a record keeps, in characters.
pub const SUMMARY_MAX_CHARS: usize = 200;

/// The most records one run keeps; past it the transcript says how many it
/// dropped (bounded by the gateway's size cap: a record is a few hundred
/// bytes).
pub const MAX_RECORDS_PER_RUN: usize = 4000;

/// What a record describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptKind {
    /// The agent called a tool (`name`, `args_digest`).
    ToolCall,
    /// A tool's answer came back (`name` when the stream names it).
    ToolResult,
    /// The agent said something (assistant text).
    Message,
    /// Something went wrong (a refused call, an error event, a failed result).
    Error,
}

/// One normalised stream item, as an adapter reads it — before digesting
/// and redaction.
#[derive(Debug, Clone, PartialEq)]
pub struct TranscriptEntry {
    pub kind: TranscriptKind,
    pub name: Option<String>,
    /// The tool call's arguments; only their digest is kept.
    pub args: Option<Value>,
    /// Free text: the message, the result preview, the error.
    pub text: String,
    /// When it happened, when the stream says (`None` = when the line was read).
    pub at: Option<SystemTime>,
}

impl TranscriptEntry {
    #[must_use]
    pub fn new(kind: TranscriptKind, text: impl Into<String>) -> Self {
        Self {
            kind,
            name: None,
            args: None,
            text: text.into(),
            at: None,
        }
    }

    #[must_use]
    pub fn tool_call(name: impl Into<String>, args: Value) -> Self {
        Self {
            name: Some(name.into()),
            args: Some(args),
            ..Self::new(TranscriptKind::ToolCall, String::new())
        }
    }

    #[must_use]
    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    #[must_use]
    pub fn at(mut self, at: SystemTime) -> Self {
        self.at = Some(at);
        self
    }
}

/// One stored transcript line (the JSONL wire format).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscriptRecord {
    /// RFC 3339, microseconds, UTC.
    pub ts: String,
    /// The run attempt it belongs to (1-based).
    pub attempt: u32,
    pub kind: TranscriptKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// `sha256:<hex>` of the canonical args JSON; tool calls only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args_digest: Option<String>,
    pub summary: String,
}

/// One attempt's records. Cheap to clone (shared); the runner keeps one
/// handle and gives the other to the harness via the task.
#[derive(Clone, Debug)]
pub struct TranscriptSink {
    attempt: u32,
    records: Arc<Mutex<Vec<TranscriptRecord>>>,
}

impl TranscriptSink {
    #[must_use]
    pub fn new(attempt: u32) -> Self {
        Self {
            attempt,
            records: Arc::default(),
        }
    }

    /// Normalise `entry` into a record, scrubbing `secrets` (the run's own
    /// bearer) along with every credential shape.
    pub fn record(&self, entry: TranscriptEntry, secrets: &[&str]) {
        let record = TranscriptRecord {
            ts: rfc3339(entry.at.unwrap_or_else(SystemTime::now)),
            attempt: self.attempt,
            kind: entry.kind,
            name: entry.name.map(|n| {
                cut(
                    &escurel_types::redact::scrub(&n, secrets),
                    SUMMARY_MAX_CHARS,
                )
            }),
            args_digest: entry.args.as_ref().map(args_digest),
            summary: cut(
                &escurel_types::redact::scrub(&entry.text, secrets),
                SUMMARY_MAX_CHARS,
            ),
        };
        if let Ok(mut records) = self.records.lock() {
            records.push(record);
        }
    }

    /// Take the records collected so far.
    #[must_use]
    pub fn take(&self) -> Vec<TranscriptRecord> {
        self.records
            .lock()
            .map(|mut r| std::mem::take(&mut *r))
            .unwrap_or_default()
    }
}

/// The run's records as JSONL, at most [`MAX_RECORDS_PER_RUN`] of them; a
/// final `error` record says how many were dropped past the cap. `None`
/// when there is nothing to store.
#[must_use]
pub fn to_jsonl(records: &[TranscriptRecord]) -> Option<(String, usize)> {
    if records.is_empty() {
        return None;
    }
    let mut kept: Vec<TranscriptRecord> = records
        .iter()
        .take(MAX_RECORDS_PER_RUN.saturating_sub(1))
        .cloned()
        .collect();
    let dropped = records.len() - kept.len();
    if dropped == 1 {
        kept.push(records[records.len() - 1].clone());
    } else if dropped > 1 {
        let last = &records[records.len() - 1];
        kept.push(TranscriptRecord {
            ts: last.ts.clone(),
            attempt: last.attempt,
            kind: TranscriptKind::Error,
            name: None,
            args_digest: None,
            summary: format!("transcript truncated: {dropped} records dropped past the cap"),
        });
    }
    let mut out = String::new();
    for r in &kept {
        out.push_str(&serde_json::to_string(r).ok()?);
        out.push('\n');
    }
    Some((out, kept.len()))
}

/// `sha256:<hex>` over the canonical JSON of `args`: object keys sorted at
/// every level, no whitespace — the same arguments digest the same however
/// the harness ordered them.
#[must_use]
pub fn args_digest(args: &Value) -> String {
    use sha2::{Digest, Sha256};
    let mut canonical = String::new();
    write_canonical(args, &mut canonical);
    format!("sha256:{:x}", Sha256::digest(canonical.as_bytes()))
}

fn write_canonical(v: &Value, out: &mut String) {
    match v {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(k.clone()).to_string());
                out.push(':');
                write_canonical(&map[k], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

/// `s` cut to `max` characters, with an ellipsis when it was longer.
fn cut(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut head: String = s.chars().take(max - 1).collect();
    head.push('…');
    head
}

/// An RFC 3339 timestamp from a harness stream, as a [`SystemTime`].
#[must_use]
pub fn parse_rfc3339(s: &str) -> Option<SystemTime> {
    let t = chrono::DateTime::parse_from_rfc3339(s).ok()?;
    Some(SystemTime::from(t.with_timezone(&chrono::Utc)))
}

fn rfc3339(t: SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn args_are_digested_canonically_and_never_kept() {
        let sink = TranscriptSink::new(2);
        sink.record(
            TranscriptEntry::tool_call(
                "update_page",
                json!({"b": 1, "a": {"y": 2, "x": "SECRETARG"}}),
            ),
            &[],
        );
        let r = &sink.take()[0];
        assert_eq!(r.attempt, 2);
        assert_eq!(r.kind, TranscriptKind::ToolCall);
        assert_eq!(r.name.as_deref(), Some("update_page"));
        assert_eq!(
            r.args_digest.as_deref(),
            Some(args_digest(&json!({"a": {"x": "SECRETARG", "y": 2}, "b": 1})).as_str()),
            "key order does not change the digest"
        );
        assert!(r.args_digest.as_deref().unwrap().starts_with("sha256:"));
        assert!(!serde_json::to_string(r).unwrap().contains("SECRETARG"));
    }

    #[test]
    fn the_summary_is_scrubbed_before_it_is_cut() {
        let sink = TranscriptSink::new(1);
        let token = "run-bearer-abcdef";
        let long_tail = "x".repeat(400);
        sink.record(
            TranscriptEntry::new(
                TranscriptKind::Error,
                format!(
                    "401 with Bearer eyJhbGciOiJSUzI1NiJ9.eyJzdWIi.sig and {token} {long_tail}"
                ),
            ),
            &[token],
        );
        let r = &sink.take()[0];
        assert!(
            r.summary.chars().count() <= SUMMARY_MAX_CHARS,
            "{}",
            r.summary
        );
        assert!(
            r.summary
                .starts_with("401 with Bearer [redacted] and [redacted] x"),
            "{}",
            r.summary
        );
        assert!(r.summary.ends_with('…'));
    }

    #[test]
    fn jsonl_is_one_record_per_line_and_says_what_the_cap_dropped() {
        let sink = TranscriptSink::new(1);
        for i in 0..(MAX_RECORDS_PER_RUN + 5) {
            sink.record(
                TranscriptEntry::new(TranscriptKind::Message, format!("m{i}")),
                &[],
            );
        }
        let records = sink.take();
        let (jsonl, n) = to_jsonl(&records).expect("some");
        assert_eq!(n, MAX_RECORDS_PER_RUN);
        let lines: Vec<&str> = jsonl.lines().collect();
        assert_eq!(lines.len(), MAX_RECORDS_PER_RUN);
        let last: TranscriptRecord = serde_json::from_str(lines[lines.len() - 1]).unwrap();
        assert_eq!(last.kind, TranscriptKind::Error);
        assert!(
            last.summary.contains("6 records dropped"),
            "{}",
            last.summary
        );
        assert_eq!(to_jsonl(&[]), None);
    }
}
