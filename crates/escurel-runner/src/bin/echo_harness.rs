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

/// The coordinates a barrier (`verify`) step must stamp into the
/// `verify-vote` instance it produces, recovered from the step event's
/// `provenance.workflow` (`§3.5`). The barrier tally counts `COUNT(DISTINCT
/// vote_index)` grouped by `claim`, so a harness that omits these collapses
/// every skeptic onto one slot and wedges the barrier open forever.
struct VoteStamp {
    /// The element the vote is about — `provenance.workflow.over`, the
    /// upstream instance page id this skeptic reviews. The barrier groups on
    /// its [`element_slug`], not the raw page id (see `derive_instance_frontmatter`).
    over: String,
    /// The distinct skeptic slot — `provenance.workflow.vote_index`.
    vote_index: u64,
    /// The run board this vote belongs to — `provenance.workflow.run` (scopes
    /// the operator verify-tally inspection query).
    run: String,
}

/// Read a [`VoteStamp`] out of an inbox event's `provenance.workflow`, but
/// only for a **barrier** step — one carrying a `vote_index`. A non-barrier
/// step (no `vote_index`) yields `None`, leaving its produced instance's
/// minimal frontmatter untouched.
fn vote_stamp(event: &Value) -> Option<VoteStamp> {
    let wf = event.get("provenance")?.get("workflow")?;
    let vote_index = wf.get("vote_index")?.as_u64()?;
    Some(VoteStamp {
        over: wf
            .get("over")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        vote_index,
        run: wf
            .get("run")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    })
}

/// The barrier's `claim` grouping key for an upstream page id — its last path
/// segment sans `.md`. Mirrors `escurel_runner_workflow::reduce::element_slug`
/// (and the Gemini runner's `element_slug`), so an echo-authored vote's
/// `claim` matches exactly what the reducer's tally compares against.
fn element_slug(page_id: &str) -> String {
    let seg = page_id.rsplit('/').next().unwrap_or(page_id);
    seg.strip_suffix(".md").unwrap_or(seg).to_owned()
}

/// Emit a YAML value double-quoted (escaping `"` and `\`), so a page-id ref
/// carrying no YAML-special quoting still round-trips as a plain scalar.
fn yaml_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Derive minimal instance frontmatter from a
/// `markdown/instances/<skill>/<id>.md` page id, so a missing target can be
/// *created* rather than rejected. Returns `None` when the path is not an
/// instance path (leaving the existing "no frontmatter → update_page
/// rejects" behaviour for a genuinely malformed target).
///
/// When `vote` is set (a barrier `verify` step), the vote coordinates the
/// tally reads are stamped alongside the type fields — `claim`, `vote_index`,
/// a non-refuting `verdict: valid` (the echo harness is a deterministic
/// stand-in, never a real skeptic), and the `workflow_run` scope.
fn derive_instance_frontmatter(page_id: &str, vote: Option<&VoteStamp>) -> Option<String> {
    let rest = page_id.strip_prefix("markdown/instances/")?;
    let (skill, file) = rest.split_once('/')?;
    let id = file.strip_suffix(".md").unwrap_or(file);
    if skill.is_empty() || id.is_empty() {
        return None;
    }
    let mut fm = format!("---\ntype: instance\nskill: {skill}\nid: {id}\n");
    if let Some(v) = vote {
        // `claim` is the upstream item's slug (the tally's grouping key), or
        // the vote's own id when no `over` was routed — mirrors the Gemini
        // runner's `claim = element_slug(over) if over else inst_id`.
        let claim = if v.over.is_empty() {
            id.to_owned()
        } else {
            element_slug(&v.over)
        };
        fm.push_str(&format!("claim: {}\n", yaml_quote(&claim)));
        fm.push_str(&format!("vote_index: {}\n", v.vote_index));
        fm.push_str("verdict: valid\n");
        if !v.run.is_empty() {
            fm.push_str(&format!("workflow_run: {}\n", yaml_quote(&v.run)));
        }
    }
    fm.push_str("---\n");
    Some(fm)
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
    let workflow = event.get("provenance").and_then(|p| p.get("workflow"));
    let is_workflow_step = workflow.is_some_and(|w| w.is_object());

    // The `lint` workflow's scan step (compile-first-wiki G2): perform a real,
    // deterministic structural health pass over the pages named by the run
    // board and record each finding as an `issue` — reading the scanned pages
    // only, never rewriting them. A semantic LLM harness would additionally
    // find nuanced contradictions; the echo does the structural tier.
    let wf_skill = workflow
        .and_then(|w| w.get("wf_skill"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    // Route only the lint *scan step* (which targets an `issue` page) here; the
    // lint *invocation* (which targets the run board) folds normally so the
    // reducer then emits the scan step.
    if wf_skill == "lint" && instance_page_id.starts_with("markdown/instances/issue/") {
        let run = workflow
            .and_then(|w| w.get("run"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        return lint_scan(&mcp, &event_id, &instance_page_id, &run, tool_calls);
    }
    // The `curate` workflow's curate step regenerates the by-category index
    // (compile-first-wiki G3) — a pure function of the corpus.
    if wf_skill == "curate" && instance_page_id.starts_with("markdown/instances/index/") {
        let run = workflow
            .and_then(|w| w.get("run"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let at = event
            .get("at")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map_or_else(now_rfc3339, str::to_owned);
        return curate_index(&mcp, &event_id, &instance_page_id, &run, &at, tool_calls);
    }
    // The `eval` workflow (compile-first-wiki G4): the `score` step (targets an
    // eval-result) checks one task; the `apply` step (a durable-target weave,
    // targets the implicated page/skill) merges the fix.
    if wf_skill == "eval" && instance_page_id.starts_with("markdown/instances/eval-result/") {
        let task_page = workflow
            .and_then(|w| w.get("over"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        return eval_score(&mcp, &event_id, &instance_page_id, &task_page, tool_calls);
    }
    if wf_skill == "eval" && !instance_page_id.contains("/workflow-run/") {
        let run = workflow
            .and_then(|w| w.get("run"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let at = now_rfc3339();
        return improve_apply(&mcp, &event_id, &instance_page_id, &run, &at, tool_calls);
    }

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
        // A missing target is *created*. If the driving step is a barrier
        // vote, stamp the tally's coordinates (`§3.5`) so distinct skeptics
        // count as distinct votes rather than collapsing onto one slot.
        let stamp = vote_stamp(event);
        frontmatter =
            derive_instance_frontmatter(&instance_page_id, stamp.as_ref()).ok_or_else(|| {
                format!("target {instance_page_id} is missing and is not an instance path")
            })?;
    }
    if is_workflow_step {
        frontmatter = stamp_frontmatter(&frontmatter, "source_event", &event_id);
        // Freshness (compile-first-wiki G3): stamp when this fact was last
        // (re-)verified. Reducer-emitted step events carry no `at`, so fall back
        // to now (RFC 3339, so lint's lexical staleness compare works). Lint's
        // `stale` check reads this field.
        let verified_at = event
            .get("at")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map_or_else(now_rfc3339, str::to_owned);
        frontmatter = stamp_frontmatter(&frontmatter, "last_verified", &verified_at);
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

/// The current time as an RFC 3339 string (UTC, second precision) — the
/// freshness/generation stamp when the event carries no timestamp of its own.
fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// The `run_slug`: the last path segment of a page id, sans `.md`.
fn run_slug(page_id: &str) -> &str {
    page_id
        .rsplit('/')
        .next()
        .unwrap_or(page_id)
        .strip_suffix(".md")
        .unwrap_or(page_id)
}

/// Read `scan_skills` off the run board frontmatter — a YAML list (JSON array)
/// or a comma-separated string.
fn scan_skills(fm: &Value) -> Vec<String> {
    match fm.get("scan_skills") {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        Some(Value::String(s)) => s.split(',').map(|p| p.trim().to_owned()).collect(),
        _ => Vec::new(),
    }
}

/// Build one `issue` instance's markdown.
fn issue_md(kind: &str, severity: &str, subject: &str, message: &str, run: &str, id: &str) -> String {
    format!(
        "---\ntype: instance\nskill: issue\nid: {id}\nkind: {kind}\nseverity: {severity}\n\
         subject_page: {subject}\nmessage: {message}\nsource_run: {run}\n---\n# {kind} issue\n\n{message}\n"
    )
}

/// The deterministic **structural** lint scan (compile-first-wiki G2). Reads the
/// pages named by the run board's `scan_skills`, flags `orphan` / `stale` /
/// `contradiction`, and writes one `issue` per finding. The scanned pages are
/// only *read* — lint proposes, it never rewrites.
fn lint_scan(
    mcp: &Mcp,
    event_id: &str,
    summary_page: &str,
    run: &str,
    mut tool_calls: u32,
) -> Result<HarnessOutcome, String> {
    let board = mcp.call("expand", json!({ "page_id": run }))?;
    tool_calls += 1;
    let fm = board.get("frontmatter").cloned().unwrap_or(Value::Null);
    let mut skills = scan_skills(&fm);
    // Fallback (tick-driven runs whose board carries no scan config): scan every
    // content skill, minus the system/workflow skills that would only produce
    // noise.
    if skills.is_empty() {
        let listed = mcp.call("list_skills", json!({}))?;
        tool_calls += 1;
        const DENY: &[&str] = &[
            "issue",
            "workflow-run",
            "lint",
            "distill",
            "distill-claim",
            "weave",
            "distill-report",
            "deep-research",
            "escurel",
        ];
        skills = listed
            .get("skills")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.get("id").and_then(Value::as_str))
                    .filter(|id| !DENY.contains(id))
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
    }
    let stale_before = fm
        .get("stale_before")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let slug = run_slug(run);

    // (kind, subject_page, message) findings, plus fact rows for contradiction.
    let mut findings: Vec<(String, String, String)> = Vec::new();
    // fact_key -> list of (page_id, value)
    let mut facts: std::collections::BTreeMap<String, Vec<(String, String)>> =
        std::collections::BTreeMap::new();

    for skill in &skills {
        let listed = mcp.call("list_instances", json!({ "skill_id": skill }))?;
        tool_calls += 1;
        let instances = listed
            .get("instances")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for inst in &instances {
            let Some(page_id) = inst.get("page_id").and_then(Value::as_str) else {
                continue;
            };
            let ifm = inst.get("frontmatter").cloned().unwrap_or(Value::Null);

            // orphan: no inbound links.
            let nbrs = mcp.call("neighbours", json!({ "page_id": page_id, "direction": "in" }))?;
            tool_calls += 1;
            let inbound = nbrs
                .get("edges")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            if inbound == 0 {
                findings.push((
                    "orphan".to_owned(),
                    page_id.to_owned(),
                    format!("{page_id} has no inbound links — it is unreachable by navigation"),
                ));
            }

            // stale: last_verified older than the review cutoff (RFC 3339
            // sorts lexically = chronologically).
            if let Some(lv) = ifm.get("last_verified").and_then(Value::as_str)
                && !stale_before.is_empty()
                && lv < stale_before.as_str()
            {
                findings.push((
                    "stale".to_owned(),
                    page_id.to_owned(),
                    format!("last_verified {lv} predates the review cutoff {stale_before}"),
                ));
            }

            // contradiction inputs: a (fact_key, fact_value) pair.
            if let (Some(k), Some(v)) = (
                ifm.get("fact_key").and_then(Value::as_str),
                ifm.get("fact_value").and_then(Value::as_str),
            ) {
                facts
                    .entry(k.to_owned())
                    .or_default()
                    .push((page_id.to_owned(), v.to_owned()));
            }
        }
    }

    // contradiction: a fact_key asserted with two different values.
    for (key, rows) in &facts {
        let distinct: std::collections::BTreeSet<&str> =
            rows.iter().map(|(_, v)| v.as_str()).collect();
        if distinct.len() > 1 {
            for (page_id, value) in rows {
                findings.push((
                    "contradiction".to_owned(),
                    page_id.to_owned(),
                    format!("fact `{key}` = `{value}` conflicts with another page's value"),
                ));
            }
        }
    }

    // Write one issue instance per finding (create-only; never touches the
    // scanned pages).
    for (i, (kind, subject, message)) in findings.iter().enumerate() {
        let id = format!("{slug}-{kind}-{i}");
        let page_id = format!("markdown/instances/issue/{id}.md");
        let severity = if kind == "contradiction" { "error" } else { "warning" };
        let content = issue_md(kind, severity, subject, message, run, &id);
        mcp.call("update_page", json!({ "page_id": page_id, "content": content }))?;
        tool_calls += 1;
    }

    // Write the pre-flagged summary issue so the scan phase completes, and
    // assign the triggering event to it.
    let summary_id = run_slug(summary_page);
    let summary = issue_md(
        "lint_summary",
        "info",
        run,
        &format!("lint scan recorded {} issue(s)", findings.len()),
        run,
        summary_id,
    );
    mcp.call("update_page", json!({ "page_id": summary_page, "content": summary }))?;
    tool_calls += 1;
    mcp.call(
        "assign_event",
        json!({ "event_id": event_id, "instance_page_id": summary_page }),
    )?;
    tool_calls += 1;

    Ok(HarnessOutcome {
        ok: true,
        status: HarnessStatus::Ok,
        summary: format!("lint scan recorded {} issue(s)", findings.len()),
        tool_calls,
        produced_instance: Some(summary_page.to_owned()),
    })
}

/// Regenerate the curated by-category **index** (compile-first-wiki G3). Reads
/// every content skill and its instances and writes a single `index` instance
/// grouping the corpus by category. Deterministic: sorted skills and instances
/// ⇒ the same corpus yields byte-identical output (derivable).
fn curate_index(
    mcp: &Mcp,
    event_id: &str,
    index_page: &str,
    run: &str,
    at: &str,
    mut tool_calls: u32,
) -> Result<HarnessOutcome, String> {
    const DENY: &[&str] = &[
        "index",
        "issue",
        "workflow-run",
        "curate",
        "lint",
        "distill",
        "distill-claim",
        "weave",
        "distill-report",
        "escurel",
    ];
    let listed = mcp.call("list_skills", json!({}))?;
    tool_calls += 1;
    let mut skills: Vec<(String, String)> = listed
        .get("skills")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|s| {
                    let id = s.get("id").and_then(Value::as_str)?;
                    if DENY.contains(&id) {
                        return None;
                    }
                    let desc = s
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    Some((id.to_owned(), desc.to_owned()))
                })
                .collect()
        })
        .unwrap_or_default();
    skills.sort();

    let mut body = String::from("# Knowledge Base Index\n\nA map of the territory by category.\n");
    for (skill, desc) in &skills {
        let listed = mcp.call("list_instances", json!({ "skill_id": skill }))?;
        tool_calls += 1;
        let mut ids: Vec<String> = listed
            .get("instances")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|i| i.get("page_id").and_then(Value::as_str))
                    .map(|p| run_slug(p).to_owned())
                    .collect()
            })
            .unwrap_or_default();
        ids.sort();
        if desc.is_empty() {
            body.push_str(&format!("\n## {skill}\n"));
        } else {
            body.push_str(&format!("\n## {skill} — {desc}\n"));
        }
        for id in &ids {
            body.push_str(&format!("- [[{skill}::{id}]]\n"));
        }
    }

    let index_id = run_slug(index_page);
    let content = format!(
        "---\ntype: instance\nskill: index\nid: {index_id}\ngenerated_at: {at}\nsource_run: {run}\n---\n{body}"
    );
    mcp.call("update_page", json!({ "page_id": index_page, "content": content }))?;
    tool_calls += 1;
    mcp.call(
        "assign_event",
        json!({ "event_id": event_id, "instance_page_id": index_page }),
    )?;
    tool_calls += 1;

    Ok(HarnessOutcome {
        ok: true,
        status: HarnessStatus::Ok,
        summary: format!("curated index over {} categories", skills.len()),
        tool_calls,
        produced_instance: Some(index_page.to_owned()),
    })
}

/// The `eval` score step (compile-first-wiki G4): check whether the task's
/// implicated page contains the expected content, and record an `eval-result`.
/// A failure names `target_page` + `fix` so `improve` can act.
fn eval_score(
    mcp: &Mcp,
    event_id: &str,
    result_page: &str,
    task_page: &str,
    mut tool_calls: u32,
) -> Result<HarnessOutcome, String> {
    let task = mcp.call("expand", json!({ "page_id": task_page }))?;
    tool_calls += 1;
    let tfm = task.get("frontmatter").cloned().unwrap_or(Value::Null);
    let implicated = tfm
        .get("implicated_page")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let expect = tfm
        .get("expect")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let fix = tfm
        .get("fix")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let task_id = run_slug(task_page).to_owned();

    // Answer the task from the KB: does the implicated page contain the
    // expected content?
    let page = mcp.call("expand", json!({ "page_id": implicated }))?;
    tool_calls += 1;
    let body = page.get("body").and_then(Value::as_str).unwrap_or_default();
    let passed = body.contains(&expect);

    // Regression guard (bounded loop): a task that fails on a page that was
    // ALREADY improved (carries `source_event`) means the fix did not hold —
    // raise an `eval_regression` issue for human review rather than looping.
    let already_improved = page
        .get("frontmatter")
        .and_then(|f| f.get("source_event"))
        .is_some();
    if !passed && already_improved {
        let issue_id = format!("{task_id}-eval-regression");
        let issue_page = format!("markdown/instances/issue/{issue_id}.md");
        let content = issue_md(
            "eval_regression",
            "error",
            &implicated,
            &format!("task `{task_id}` still fails after an improvement to {implicated}"),
            "eval",
            &issue_id,
        );
        mcp.call("update_page", json!({ "page_id": issue_page, "content": content }))?;
        tool_calls += 1;
    }

    let result_id = run_slug(result_page);
    let content = if passed {
        format!(
            "---\ntype: instance\nskill: eval-result\nid: {result_id}\ntask: {task_id}\nverdict: pass\n---\n# eval-result\n\nPASS: {implicated} answers the task.\n"
        )
    } else {
        format!(
            "---\ntype: instance\nskill: eval-result\nid: {result_id}\ntask: {task_id}\nverdict: fail\n\
             target_page: {implicated}\nfix: {fix}\n---\n# eval-result\n\nFAIL: {implicated} is missing the expected content.\n"
        )
    };
    mcp.call("update_page", json!({ "page_id": result_page, "content": content }))?;
    tool_calls += 1;
    mcp.call(
        "assign_event",
        json!({ "event_id": event_id, "instance_page_id": result_page }),
    )?;
    tool_calls += 1;

    Ok(HarnessOutcome {
        ok: true,
        status: HarnessStatus::Ok,
        summary: format!("eval {}: {}", task_id, if passed { "pass" } else { "fail" }),
        tool_calls,
        produced_instance: Some(result_page.to_owned()),
    })
}

/// The `improve` apply step (compile-first-wiki G4): weave the fix from the
/// failing eval-result into the implicated durable page (a document *or a
/// skill*), stamping freshness. Reuses the G1 durable-target path — the event
/// is pre-flagged onto `target_page`, so reconciliation confirms on it.
fn improve_apply(
    mcp: &Mcp,
    event_id: &str,
    target_page: &str,
    run: &str,
    at: &str,
    mut tool_calls: u32,
) -> Result<HarnessOutcome, String> {
    // Find THIS run's failing eval-result for this target and its fix. Results
    // accumulate across runs, so scope to the current run (run-scoped page-id
    // prefix) — never apply a stale fix from an earlier run.
    let run_prefix = format!("markdown/instances/eval-result/{}-", run_slug(run));
    let listed = mcp.call("list_instances", json!({ "skill_id": "eval-result" }))?;
    tool_calls += 1;
    let fix = listed
        .get("instances")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|r| {
            r["page_id"].as_str().is_some_and(|p| p.starts_with(&run_prefix))
                && r["frontmatter"]["verdict"].as_str() == Some("fail")
                && r["frontmatter"]["target_page"].as_str() == Some(target_page)
        })
        .and_then(|r| r["frontmatter"]["fix"].as_str())
        .unwrap_or_default()
        .to_owned();

    let expanded = mcp.call("expand", json!({ "page_id": target_page }))?;
    tool_calls += 1;
    let current_body = expanded
        .get("body")
        .and_then(Value::as_str)
        .unwrap_or_default();
    // Reconstruct the target's frontmatter (it already exists) so update_page
    // gets the full markdown; stamp provenance + freshness onto it.
    let mut frontmatter = render_frontmatter(expanded.get("frontmatter"));
    frontmatter = stamp_frontmatter(&frontmatter, "source_event", event_id);
    frontmatter = stamp_frontmatter(&frontmatter, "last_verified", at);

    let note = if fix.is_empty() {
        format!("\n- improved via {event_id}\n")
    } else {
        format!("\n{fix}\n")
    };
    let new_body = format!("{}{}", current_body.trim_end_matches('\n'), note);
    let content = format!("{frontmatter}{new_body}");
    mcp.call("update_page", json!({ "page_id": target_page, "content": content }))?;
    tool_calls += 1;
    mcp.call(
        "assign_event",
        json!({ "event_id": event_id, "instance_page_id": target_page }),
    )?;
    tool_calls += 1;

    Ok(HarnessOutcome {
        ok: true,
        status: HarnessStatus::Ok,
        summary: format!("improved {target_page}"),
        tool_calls,
        produced_instance: Some(target_page.to_owned()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn element_slug_is_the_basename_sans_md() {
        // Must mirror reduce::element_slug exactly — the barrier's grouping key.
        assert_eq!(
            element_slug("markdown/instances/claims/r1-extract-abc123.md"),
            "r1-extract-abc123"
        );
        assert_eq!(element_slug("bare-slug"), "bare-slug");
        assert_eq!(element_slug("no/dir/x.md"), "x");
    }

    #[test]
    fn non_barrier_step_stamps_only_type_fields() {
        // No vote_index in provenance ⇒ no VoteStamp ⇒ minimal frontmatter,
        // unchanged from the pre-barrier behaviour.
        let event = json!({ "provenance": { "workflow": { "over": "x", "run": "r" } } });
        assert!(vote_stamp(&event).is_none());
        let fm =
            derive_instance_frontmatter("markdown/instances/risk-signal/r1-signals-9.md", None)
                .expect("instance path");
        assert_eq!(
            fm,
            "---\ntype: instance\nskill: risk-signal\nid: r1-signals-9\n---\n"
        );
    }

    #[test]
    fn barrier_step_stamps_claim_as_upstream_slug_plus_slot() {
        // A verify (barrier) step: claim = element_slug(over), the distinct
        // vote_index slot, a non-refuting verdict, and the run scope — exactly
        // the fields `vote_from_instance` + the tally read.
        let event = json!({ "provenance": { "workflow": {
            "over": "markdown/instances/claims/r1-extract-abc.md",
            "vote_index": 2,
            "run": "markdown/instances/workflow-run/r1.md",
        }}});
        let stamp = vote_stamp(&event).expect("barrier step yields a stamp");
        let fm = derive_instance_frontmatter(
            "markdown/instances/verify-vote/r1-verify-ff.md",
            Some(&stamp),
        )
        .expect("instance path");
        assert!(
            fm.contains("claim: \"r1-extract-abc\"\n"),
            "claim is the upstream slug: {fm}"
        );
        assert!(fm.contains("vote_index: 2\n"), "distinct slot: {fm}");
        assert!(
            fm.contains("verdict: valid\n"),
            "non-refuting stand-in: {fm}"
        );
        assert!(
            fm.contains("workflow_run: \"markdown/instances/workflow-run/r1.md\"\n"),
            "run scope: {fm}"
        );
    }

    #[test]
    fn barrier_step_without_over_falls_back_to_instance_id() {
        let event = json!({ "provenance": { "workflow": { "vote_index": 0 } } });
        let stamp = vote_stamp(&event).expect("vote_index present");
        let fm = derive_instance_frontmatter(
            "markdown/instances/verify-vote/r1-verify-01.md",
            Some(&stamp),
        )
        .expect("instance path");
        assert!(
            fm.contains("claim: \"r1-verify-01\"\n"),
            "fallback to id: {fm}"
        );
    }
}
