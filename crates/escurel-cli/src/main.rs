//! `escurel` — operator + agent-style CLI for the Escurel gateway.
//!
//! Pure presentation over [`escurel_client`]: every subcommand maps to
//! one typed RPC, renders the response as JSON (the default, stable
//! contract for scripts and LLM agents) or as a human table
//! (`--format table`). Errors are emitted as JSON on **stderr** with a
//! non-zero exit so a calling agent can branch on them.
//!
//! Commands are grouped gh/aws-style by resource noun:
//!   escurel skill list
//!   escurel instance list --skill customer
//!   escurel page expand|validate|update <page_id>
//!   escurel link neighbours <page_id>
//!   escurel event capture|inbox|list|assign
//!   escurel query run <query_id> --params '{…}'
//!   escurel chat append|list
//!   escurel admin <tenant|audit|quota|…>
//! with two natural top-level verbs (`search`, `resolve`).
//!
//! `--server` / `ESCUREL_SERVER` and `--token` / `ESCUREL_TOKEN` are
//! global. When no token is set the CLI sends an empty bearer; a server
//! without a verifier (dev mode) ignores it, a server with one rejects
//! it as `Unauthenticated`.

mod admin;
mod agent;
mod convert;
mod output;

use anyhow::Result;
use clap::{Parser, Subcommand};
use escurel_client::{AdminClient, Client, SecretString};
use output::Format;
use serde_json::json;

#[derive(Parser, Debug)]
#[command(name = "escurel", about = "CLI for the Escurel gateway", version)]
struct Cli {
    /// HTTP endpoint URL, e.g. `http://127.0.0.1:8080`.
    #[arg(long, env = "ESCUREL_SERVER", default_value = "http://127.0.0.1:8080")]
    server: String,
    /// OIDC bearer token. Required unless the server runs
    /// unauthenticated (dev only).
    #[arg(long, env = "ESCUREL_TOKEN", hide_env_values = true)]
    token: Option<String>,
    /// Output format.
    #[arg(long, value_enum, default_value_t = Format::Json, global = true)]
    format: Format,
    #[command(subcommand)]
    cmd: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Hybrid vector + FTS search.
    Search(agent::SearchArgs),
    /// Parse a `[[wikilink]]` and look up its target page.
    Resolve { wikilink: String },
    /// Tier-1 skill catalogue.
    #[command(subcommand)]
    Skill(agent::SkillCmd),
    /// Instances of a skill.
    #[command(subcommand)]
    Instance(agent::InstanceCmd),
    /// Page read / validate / write.
    #[command(subcommand)]
    Page(agent::PageCmd),
    /// Typed link-graph traversal.
    #[command(subcommand)]
    Link(agent::LinkCmd),
    /// Event-sourcing surface: inbox, history, capture, assign.
    #[command(subcommand)]
    Event(agent::EventCmd),
    /// Stored queries.
    #[command(subcommand)]
    Query(agent::QueryCmd),
    /// Per-chat-group conversation log.
    #[command(subcommand)]
    Chat(agent::ChatCmd),
    /// Operator surface (admin-role token required, except `health`).
    #[command(subcommand)]
    Admin(admin::AdminCmd),
    /// Launch the interactive k9s-style terminal browser.
    Ui,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let fmt = cli.format;
    if let Err(e) = run(cli).await {
        // JSON-on-stderr error contract: an agent parses this; a human
        // still reads it. Always non-zero exit.
        let body = json!({ "error": e.to_string() });
        match fmt {
            Format::Json => eprintln!("{}", serde_json::to_string_pretty(&body).unwrap()),
            Format::Table => eprintln!("error: {e}"),
        }
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<()> {
    let token = SecretString::from(cli.token.unwrap_or_default());
    let fmt = cli.format;

    // The admin group dials the admin service; the `ui` subcommand takes
    // over the terminal (raw mode + alternate screen) and never emits
    // JSON; everything else dials the agent service and renders a value.
    // Dial lazily so a bad URL surfaces the same way for every path.
    let value = match cli.cmd {
        // `ui` returns directly: it owns the terminal and produces no value.
        Command::Ui => return escurel_tui::run(&cli.server, token).await,
        Command::Admin(cmd) => {
            let client = AdminClient::connect(&cli.server, token).await?;
            admin::run(&client, cmd).await?
        }
        other => {
            let client = Client::connect(&cli.server, token).await?;
            agent::run(&client, other).await?
        }
    };
    output::emit(&value, fmt)?;
    Ok(())
}
