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
    let frontmatter = render_frontmatter(expanded.get("frontmatter"));

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
