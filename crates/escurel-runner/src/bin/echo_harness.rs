//! `escurel-echo-harness` — the deterministic real harness for #151.
//!
//! This is a **real harness subprocess**, not a mock: it is the test
//! stand-in for an LLM agent, but every escurel effect it has is 100% real,
//! performed through the gateway's own `/mcp` tool calls under the scoped
//! agent token the runner packaged.
//!
//! Wire contract (the same JSON every [`escurel_runner_harness::Harness`]
//! adapter hands every harness):
//!
//! - **stdin**: a JSON [`escurel_runner_harness::HarnessTask`]
//!   (`instructions`, `input`, `mcp_endpoint`, `allowed_tools`, `token`).
//! - **stdout**: a JSON [`escurel_runner_harness::HarnessOutcome`]
//!   (`{ ok, status, summary, tool_calls, produced_instance? }`).
//! - exit `0` on a clean run; non-zero with a stderr message on failure.
//!
//! What it does — the minimal real "fold the event into the instance":
//! 1. `list_inbox` over `/mcp` → take the oldest unprocessed event.
//! 2. Resolve its target instance (`instance_page_id`; the packaged trigger
//!    pre-flagged it). `expand` that instance's current body.
//! 3. `update_page` appending a short event note under the existing body
//!    (append, never clobber — the baseline content survives).
//! 4. `assign_event` to mark the event `processed` + bound to the instance.
//!
//! A real LLM harness would read `instructions`/`input` to *decide* these
//! steps; the echo harness performs them deterministically. The escurel
//! writes are identical real `/mcp` calls either way.

use std::io::Read;
use std::process::ExitCode;

use escurel_runner_harness::{HarnessOutcome, HarnessStatus, HarnessTask};
use serde_json::{Value, json};

fn main() -> ExitCode {
    // Read the packaged task from stdin.
    let mut buf = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut buf) {
        eprintln!("echo-harness: could not read task from stdin: {e}");
        return ExitCode::FAILURE;
    }
    let task: HarnessTask = match serde_json::from_str(&buf) {
        Ok(task) => task,
        Err(e) => {
            eprintln!("echo-harness: malformed task on stdin: {e}");
            return ExitCode::FAILURE;
        }
    };

    match run(&task) {
        Ok(outcome) => match serde_json::to_string(&outcome) {
            Ok(s) => {
                println!("{s}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("echo-harness: could not serialize outcome: {e}");
                ExitCode::FAILURE
            }
        },
        Err(e) => {
            eprintln!("echo-harness: {e}");
            ExitCode::FAILURE
        }
    }
}

/// A blocking MCP-over-HTTP tool call against the packaged endpoint.
struct Mcp {
    endpoint: String,
    token: String,
    http: reqwest::blocking::Client,
}

impl Mcp {
    fn new(endpoint: &str, token: &str) -> Self {
        Self {
            endpoint: endpoint.to_owned(),
            token: token.to_owned(),
            http: reqwest::blocking::Client::new(),
        }
    }

    /// Call one tool, returning its `result` object (or a flattened error).
    fn call(&self, name: &str, args: Value) -> Result<Value, String> {
        let resp = self
            .http
            .post(&self.endpoint)
            .header("authorization", format!("Bearer {}", self.token))
            .json(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": { "name": name, "arguments": args },
            }))
            .send()
            .map_err(|e| format!("/mcp {name} transport error: {e}"))?;
        let status = resp.status();
        let body: Value = resp
            .json()
            .map_err(|e| format!("/mcp {name} bad JSON (http {status}): {e}"))?;
        if let Some(err) = body.get("error") {
            return Err(format!("/mcp {name} tool error: {err}"));
        }
        let result = body.get("result").cloned().unwrap_or(Value::Null);
        // The gateway MCP-shapes a `tools/call` success into a
        // `CallToolResult` (`{content, structuredContent, isError}`);
        // unwrap `structuredContent` (the raw payload) so the fold below
        // reads `events` / `body` / `frontmatter` directly.
        Ok(result.get("structuredContent").cloned().unwrap_or(result))
    }
}

/// Reconstruct a YAML frontmatter block (`---\n…\n---\n`) from the JSON
/// object `expand` returned. `update_page` requires the full markdown
/// (frontmatter + body); `expand` hands them apart, so the harness re-emits
/// the frontmatter. Scalar values are emitted as-is; the instance pages the
/// echo harness folds into carry only scalar frontmatter (`type`, `id`,
/// `skill`). A missing/empty object yields an empty string (the page then has
/// no frontmatter — and `update_page` will reject it, surfacing as a failed
/// run rather than silent corruption).
fn render_frontmatter(value: Option<&Value>) -> String {
    let Some(obj) = value.and_then(Value::as_object) else {
        return String::new();
    };
    if obj.is_empty() {
        return String::new();
    }
    let mut out = String::from("---\n");
    for (key, val) in obj {
        let rendered = match val {
            Value::String(s) => s.clone(),
            Value::Bool(b) => b.to_string(),
            Value::Number(n) => n.to_string(),
            Value::Null => String::new(),
            // Nested structures are out of scope for the echo harness; emit
            // their compact JSON so the value at least round-trips.
            other => other.to_string(),
        };
        out.push_str(&format!("{key}: {rendered}\n"));
    }
    out.push_str("---\n");
    out
}

/// Upsert a scalar `key: value` into a rendered frontmatter block
/// (`---\n…\n---\n`). Replaces the line if the key is already present,
/// otherwise inserts it just before the closing `---`. A block with no
/// closing fence (or an empty string) is returned unchanged — the caller only
/// stamps blocks it just rendered, which always have the fence.
fn stamp_frontmatter(frontmatter: &str, key: &str, value: &str) -> String {
    let line = format!("{key}: {value}");
    let mut lines: Vec<String> = frontmatter.lines().map(str::to_owned).collect();
    if let Some(existing) = lines
        .iter_mut()
        .find(|l| l.trim_start().starts_with(&format!("{key}:")))
    {
        *existing = line;
        return format!("{}\n", lines.join("\n"));
    }
    // Insert before the closing fence (the last `---`).
    if let Some(close) = lines.iter().rposition(|l| l.trim_end() == "---") {
        lines.insert(close, line);
        return format!("{}\n", lines.join("\n"));
    }
    frontmatter.to_owned()
}

/// Derive minimal instance frontmatter from a
/// `markdown/instances/<skill>/<id>.md` page id, so a missing target can be
/// *created* rather than rejected. Returns `None` when the path is not an
/// instance path (leaving the existing "no frontmatter → update_page
/// rejects" behaviour for a genuinely malformed target).
fn derive_instance_frontmatter(page_id: &str) -> Option<String> {
    let rest = page_id.strip_prefix("markdown/instances/")?;
    let (skill, file) = rest.split_once('/')?;
    let id = file.strip_suffix(".md").unwrap_or(file);
    if skill.is_empty() || id.is_empty() {
        return None;
    }
    Some(format!(
        "---\ntype: instance\nskill: {skill}\nid: {id}\n---\n"
    ))
}

/// Perform the deterministic fold; returns the structured outcome.
fn run(task: &HarnessTask) -> Result<HarnessOutcome, String> {
    let mcp = Mcp::new(&task.mcp_endpoint, &task.token);
    let mut tool_calls = 0u32;

    // 1. Read the inbox; take the oldest unprocessed event that carries a
    //    target instance (the packaged trigger pre-flagged it).
    let inbox = mcp.call("list_inbox", json!({}))?;
    tool_calls += 1;
    let events = inbox
        .get("events")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let target = events
        .iter()
        .rev() // oldest first (list_inbox returns newest-first)
        .find(|e| {
            e.get("instance_page_id")
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty())
        });
    let event = match target {
        Some(e) => e,
        None => {
            // Nothing to fold — a clean no-op pass.
            return Ok(HarnessOutcome {
                ok: true,
                status: HarnessStatus::Ok,
                summary: "no unassigned inbox event with a target instance".to_owned(),
                tool_calls,
                produced_instance: None,
            });
        }
    };
    let event_id = event
        .get("event_id")
        .and_then(Value::as_str)
        .ok_or("inbox event missing event_id")?
        .to_owned();
    let instance_page_id = event
        .get("instance_page_id")
        .and_then(Value::as_str)
        .unwrap()
        .to_owned();
    let title = event
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    // A workflow step carries a `provenance.workflow` block. For those, the
    // harness stamps `source_event: <event_id>` on the instance it writes — the
    // freshness/provenance field (compile-first-wiki G3) and, for a durable
    // `writes: existing` weave, the reducer's completion signal (a durable page
    // already exists, so the reducer detects the weave landed by this stamp).
    // A real agent stamps the same field; the echo does it deterministically.
    let is_workflow_step = event
        .get("provenance")
        .and_then(|p| p.get("workflow"))
        .is_some_and(|w| w.is_object());

    // 2. Read the instance's current state so we append rather than clobber.
    //    `expand` splits the page into a `frontmatter` object + a `body`;
    //    `update_page` needs the full markdown (frontmatter block + body), so
    //    we reconstruct it.
    let expanded = mcp.call("expand", json!({ "page_id": instance_page_id }))?;
    tool_calls += 1;
    let current_body = expanded
        .get("body")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    // An existing instance's frontmatter is folded verbatim; a MISSING target
    // (empty frontmatter) is *created* with minimal frontmatter derived from
    // its `markdown/instances/<skill>/<id>.md` path. This lets the echo
    // harness stand in for a phase that produces a fresh typed instance (a
    // dynamic-workflow step), not only one that appends to an existing page.
    let mut frontmatter = render_frontmatter(expanded.get("frontmatter"));
    if frontmatter.is_empty() {
        frontmatter = derive_instance_frontmatter(&instance_page_id).ok_or_else(|| {
            format!("target {instance_page_id} is missing and is not an instance path")
        })?;
    }
    if is_workflow_step {
        frontmatter = stamp_frontmatter(&frontmatter, "source_event", &event_id);
    }

    // 3. Append a short event note and write the full page back.
    let note = format!("\n- folded event `{event_id}`: {title}\n");
    let new_body = format!("{}{}", current_body.trim_end_matches('\n'), note);
    let new_content = format!("{frontmatter}{new_body}");
    let updated = mcp.call(
        "update_page",
        json!({ "page_id": instance_page_id, "content": new_content }),
    )?;
    tool_calls += 1;
    let new_version = updated
        .get("new_version")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();

    // 4. Mark the event processed + bound to the instance.
    mcp.call(
        "assign_event",
        json!({ "event_id": event_id, "instance_page_id": instance_page_id }),
    )?;
    tool_calls += 1;

    Ok(HarnessOutcome {
        ok: true,
        status: HarnessStatus::Ok,
        summary: format!(
            "folded event {event_id} into {instance_page_id} (new_version {new_version})"
        ),
        tool_calls,
        produced_instance: Some(instance_page_id),
    })
}

#[cfg(test)]
mod tests {
    use super::stamp_frontmatter;

    #[test]
    fn stamp_inserts_a_new_key_before_the_closing_fence() {
        let fm = "---\ntype: instance\nskill: entity\nid: acme\n---\n";
        let out = stamp_frontmatter(fm, "source_event", "EV1");
        assert!(out.contains("source_event: EV1\n"));
        assert!(out.contains("skill: entity\n"));
        // Still a well-formed block: last line before body is the closing ---.
        assert!(out.trim_end().ends_with("---"));
    }

    #[test]
    fn stamp_replaces_an_existing_key() {
        let fm = "---\ntype: instance\nsource_event: OLD\nid: acme\n---\n";
        let out = stamp_frontmatter(fm, "source_event", "NEW");
        assert!(out.contains("source_event: NEW\n"));
        assert!(!out.contains("OLD"));
        assert_eq!(out.matches("source_event:").count(), 1, "no duplicate key");
    }
}
