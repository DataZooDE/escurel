//! Consumer-skill ↔ tool-surface **parity guard** (the mechanical half).
//!
//! `.claude/skills/escurel-platform/references/02-tool-surface.md` is
//! shipped documentation that downstream agents act on as fact — a stale
//! tool name or argument name there is an agent confidently doing the
//! wrong thing (see CLAUDE.md §Keeping the consumer skill in sync; the
//! 2026-08-02 audit found exactly this class of drift, and the 2026-08-14
//! API review found it again on the same file).
//!
//! This test drives the **real** gateway, reads the live `tools/list`,
//! and reconciles the skill's tables against it:
//!
//! * every `` `tool` `` named in the first column of a table must exist
//!   on the wire;
//! * every backticked argument in a table's *inputs* column must be a
//!   property of that tool's live `inputSchema`;
//! * the "exposes N tools" count claim must equal the live count.
//!
//! It cannot check prose. The human half of the obligation stays human.

use escurel_test_support::{AuthMode, EscurelProcess, Opts};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const SKILL_REF_02: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../.claude/skills/escurel-platform/references/02-tool-surface.md"
);

/// Live `tools/list`: tool name → set of inputSchema property names.
async fn live_tools(base_url: &str) -> BTreeMap<String, Vec<String>> {
    let body: Value = reqwest::Client::new()
        .post(format!("{base_url}/mcp"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
        .send()
        .await
        .expect("POST tools/list")
        .json()
        .await
        .expect("decode tools/list");
    body["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .map(|t| {
            let name = t["name"].as_str().expect("tool name").to_owned();
            let props = t["inputSchema"]["properties"]
                .as_object()
                .map(|m| m.keys().cloned().collect())
                .unwrap_or_default();
            (name, props)
        })
        .collect()
}

/// The leading `[a-z_][a-z0-9_]*` identifier of a backtick span, if any.
/// `order_by='at asc'` → `order_by`; `{page_id, …}` → None; `—` → None.
fn leading_ident(span: &str) -> Option<&str> {
    let end = span
        .find(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'))
        .unwrap_or(span.len());
    (end > 0 && span.as_bytes()[0].is_ascii_lowercase()).then(|| &span[..end])
}

/// All backtick spans in a string.
fn backtick_spans(s: &str) -> Vec<&str> {
    let mut spans = Vec::new();
    let mut rest = s;
    while let Some(start) = rest.find('`') {
        let after = &rest[start + 1..];
        let Some(len) = after.find('`') else { break };
        spans.push(&after[..len]);
        rest = &after[len + 1..];
    }
    spans
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skill_tool_tables_match_the_live_surface() {
    let doc = std::fs::read_to_string(SKILL_REF_02)
        .unwrap_or_else(|e| panic!("read {SKILL_REF_02}: {e}"));
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::Disabled,
        fixtures: None,
        config_overrides: Default::default(),
    })
    .await;
    let live = live_tools(p.base_url()).await;

    let mut errors: Vec<String> = Vec::new();
    let mut checked_rows = 0usize;
    // Only tables whose header names an inputs column get their second
    // column arg-checked; two-column tables (`| tool | description |`)
    // carry prose there, and error codes in prose are not argument names.
    let mut table_has_inputs_col = false;

    for line in doc.lines() {
        if line.starts_with("| tool |") {
            table_has_inputs_col = line.split('|').nth(2).is_some_and(|h| h.contains("input"));
            continue;
        }
        // A tool-table row: `| `tool_name` | <inputs> | …`. Header and
        // separator rows don't start with a backticked identifier.
        let Some(row) = line.strip_prefix("| `") else {
            continue;
        };
        let Some(tool) = leading_ident(row) else {
            continue;
        };
        let Some(props) = live.get(tool) else {
            errors.push(format!(
                "row documents tool `{tool}` which the server does not \
                 advertise in tools/list"
            ));
            continue;
        };
        checked_rows += 1;
        if !table_has_inputs_col {
            continue;
        }
        // `\|` is an escaped pipe INSIDE a cell (enum alternatives), not a
        // column boundary.
        let unescaped = line.replace("\\|", "\u{A6}");
        // cells[0] is the empty prefix; cells[1] = tool, cells[2] = inputs.
        let cells: Vec<&str> = unescaped.split('|').collect();
        if cells.len() < 4 {
            continue;
        }
        for span in backtick_spans(cells[2]) {
            let Some(arg) = leading_ident(span) else {
                continue;
            };
            if !props.iter().any(|p| p == arg) {
                errors.push(format!(
                    "`{tool}` row documents input `{arg}` but the live \
                     inputSchema has only {props:?}"
                ));
            }
        }
    }

    assert!(
        checked_rows >= 15,
        "parsed only {checked_rows} tool rows from 02-tool-surface.md — \
         the table format changed and this guard went blind; fix the parser"
    );

    // The "exposes N tools" claim must track the live surface.
    let claimed: Option<usize> = doc
        .split("the server exposes ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|n| n.parse().ok());
    match claimed {
        Some(n) if n == live.len() => {}
        Some(n) => errors.push(format!(
            "02-tool-surface.md claims the server exposes {n} tools; the \
             live surface has {} — update the claim",
            live.len()
        )),
        None => errors.push(
            "02-tool-surface.md no longer contains a parseable \
             'the server exposes N tools' claim"
                .to_owned(),
        ),
    }

    assert!(
        errors.is_empty(),
        "skill doc ↔ tool surface drift ({} problems):\n  - {}",
        errors.len(),
        errors.join("\n  - ")
    );
}
