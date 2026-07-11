//! Dynamic-workflow operator surface: `escurel workflow run|status|stop`.
//!
//! `run` invokes a `kind: workflow` plan — it creates the run board (a
//! `workflow-run` instance recording the plan) and captures the initiating
//! event, whose `provenance.workflow` block routes the runner's reducer.
//! `status` renders the board's per-phase progress; `stop` marks the board so
//! the runner's recovery pass leaves it alone.

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use escurel_client::{
    CaptureEventRequest, Client, ExpandRequest, ListInstancesRequest, UpdatePageRequest,
};
use serde_json::{Value, json};

#[derive(Subcommand, Debug)]
pub enum WorkflowCmd {
    /// Invoke a workflow: create its run board and capture the run event.
    Run(RunArgs),
    /// Show a run's per-phase progress (produced instances per phase).
    Status {
        /// The run id (as printed by `workflow run`).
        run: String,
    },
    /// Request a run stop (marks its board `status: stopped`).
    Stop {
        /// The run id to stop.
        run: String,
    },
}

#[derive(Args, Debug)]
pub struct RunArgs {
    /// The `kind: workflow` plan skill id (e.g. `deep-research`).
    pub skill: String,
    /// Explicit run id (default: a fresh ULID).
    #[arg(long)]
    pub run: Option<String>,
    /// Free-text / JSON params folded into the invocation event body.
    #[arg(long)]
    pub params: Option<String>,
}

/// Dispatch a `workflow` subcommand.
pub async fn run(client: &Client, cmd: WorkflowCmd) -> Result<Value> {
    match cmd {
        WorkflowCmd::Run(a) => invoke(client, a).await,
        WorkflowCmd::Status { run } => status(client, &run).await,
        WorkflowCmd::Stop { run } => stop(client, &run).await,
    }
}

fn run_page(run_id: &str) -> String {
    format!("markdown/instances/workflow-run/{run_id}.md")
}

/// The run-board markdown, recording which plan the run belongs to (the
/// recovery pass and `status` read `wf_skill` from here).
fn board_markdown(run_id: &str, wf_skill: &str, status: &str) -> String {
    format!(
        "---\ntype: instance\nskill: workflow-run\nid: {run_id}\n\
         wf_skill: {wf_skill}\nstatus: {status}\n---\n# workflow run {run_id}\n\n\
         Plan: [[{wf_skill}]].\n"
    )
}

async fn invoke(client: &Client, a: RunArgs) -> Result<Value> {
    let run_id = a
        .run
        .unwrap_or_else(|| ulid::Ulid::new().to_string().to_lowercase());
    let page = run_page(&run_id);

    // Create the board first so it carries `wf_skill` for status/recovery
    // (the invocation event only pre-flags it).
    client
        .update_page(UpdatePageRequest {
            page_id: page.clone(),
            content: board_markdown(&run_id, &a.skill, "running"),
        })
        .await
        .context("create the run board")?;

    let stored = client
        .capture_event(CaptureEventRequest {
            source: "escurel-cli".to_owned(),
            mime: "text/plain".to_owned(),
            label_skill: a.skill.clone(),
            instance_page_id: page.clone(),
            title: format!("invoke {}", a.skill),
            body: a.params.unwrap_or_default(),
            provenance: json!({
                "workflow": { "run": page, "wf_skill": a.skill, "phase": "invoke" }
            }),
            ..Default::default()
        })
        .await
        .context("capture the invocation event")?;

    Ok(json!({
        "run": run_id,
        "run_page": page,
        "wf_skill": a.skill,
        "event_id": stored.event_id,
    }))
}

async fn status(client: &Client, run_id: &str) -> Result<Value> {
    let page = run_page(run_id);
    let board = client
        .expand(ExpandRequest {
            page_id: page.clone(),
            ..Default::default()
        })
        .await
        .context("read the run board")?;
    let wf_skill = board
        .frontmatter
        .get("wf_skill")
        .and_then(|v| v.as_str())
        .context("run board has no wf_skill")?
        .to_owned();
    let run_status = board
        .frontmatter
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_owned();

    // The plan's phases drive the progress board.
    let plan = client
        .expand(ExpandRequest {
            page_id: format!("markdown/skills/{wf_skill}.md"),
            ..Default::default()
        })
        .await
        .context("read the workflow plan")?;
    let phases = plan
        .frontmatter
        .get("phases")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut phase_rows = Vec::new();
    for phase in &phases {
        let Some(id) = phase.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(produces) = phase.get("produces").and_then(|v| v.as_str()) else {
            continue;
        };
        let prefix = format!("markdown/instances/{produces}/{run_id}-{id}-");
        let resp = client
            .list_instances(ListInstancesRequest {
                skill: produces.to_owned(),
                ..Default::default()
            })
            .await
            .context("count produced instances")?;
        let produced = resp
            .instances
            .iter()
            .filter(|i| i.page_id.starts_with(&prefix))
            .count();
        phase_rows.push(json!({ "phase": id, "produces": produces, "produced": produced }));
    }

    Ok(json!({
        "run": run_id,
        "wf_skill": wf_skill,
        "status": run_status,
        "phases": phase_rows,
    }))
}

async fn stop(client: &Client, run_id: &str) -> Result<Value> {
    let page = run_page(run_id);
    let board = client
        .expand(ExpandRequest {
            page_id: page.clone(),
            ..Default::default()
        })
        .await
        .context("read the run board")?;
    let wf_skill = board
        .frontmatter
        .get("wf_skill")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();
    client
        .update_page(UpdatePageRequest {
            page_id: page.clone(),
            content: board_markdown(run_id, &wf_skill, "stopped"),
        })
        .await
        .context("mark the run stopped")?;
    Ok(json!({ "run": run_id, "status": "stopped" }))
}
