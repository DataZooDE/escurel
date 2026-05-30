//! Thin binary around the [`escurel_demo_agent`] fold logic: polls the
//! gateway's inbox on an interval and folds routable events into their
//! instances. The agent is external to the gateway by design — this is
//! the "simulated external processor" of the M7 demo (it can later be
//! replaced by a real agent subscribing to the capture webhook).
//!
//! Config (env):
//! - `ESCUREL_AGENT_MCP_URL`   — gateway `/mcp` URL (required)
//! - `ESCUREL_AGENT_TOKEN`     — bearer JWT for the agent (required)
//! - `ESCUREL_AGENT_INTERVAL`  — poll interval seconds (default 5)
//! - `ESCUREL_AGENT_ROUTES`    — optional `label_skill=instance_page_id`
//!   pairs, comma-separated, for events without a pre-flagged instance.
//! - `ESCUREL_AGENT_ONCE`      — when set (`1`/`true`), process the inbox
//!   exactly once and exit (scriptable; used by `verify-demo.sh`).

use std::collections::HashMap;
use std::time::Duration;

use escurel_demo_agent::{McpClient, process_inbox_once};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().json().init();

    let mcp_url = env_required("ESCUREL_AGENT_MCP_URL");
    let token = env_required("ESCUREL_AGENT_TOKEN");
    let interval = std::env::var("ESCUREL_AGENT_INTERVAL")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(5);
    let routes = parse_routes(std::env::var("ESCUREL_AGENT_ROUTES").unwrap_or_default());
    let once = matches!(
        std::env::var("ESCUREL_AGENT_ONCE").ok().as_deref(),
        Some("1") | Some("true")
    );

    let client = McpClient::new(mcp_url, token);
    tracing::info!(target: "escurel", interval, once, routes = routes.len(), "demo agent started");

    // Single-pass mode: process the inbox once and exit (scriptable).
    if once {
        match process_inbox_once(&client, &routes).await {
            Ok(report) => {
                tracing::info!(
                    target: "escurel",
                    assigned = report.assigned,
                    skipped = report.skipped,
                    "agent: single pass complete",
                );
            }
            Err(e) => {
                tracing::error!(target: "escurel", error = %e, "agent: single pass failed");
                std::process::exit(1);
            }
        }
        return;
    }

    loop {
        match process_inbox_once(&client, &routes).await {
            Ok(report) if report.assigned > 0 => tracing::info!(
                target: "escurel",
                assigned = report.assigned,
                skipped = report.skipped,
                "agent: inbox pass",
            ),
            Ok(_) => {}
            Err(e) => tracing::warn!(target: "escurel", error = %e, "agent: inbox pass failed"),
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(interval)) => {}
            _ = tokio::signal::ctrl_c() => {
                tracing::info!(target: "escurel", "demo agent shutting down");
                break;
            }
        }
    }
}

fn env_required(key: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| {
        eprintln!("escurel-demo-agent: missing required env var {key}");
        std::process::exit(2);
    })
}

/// Parse `a=b,c=d` into a `label_skill → instance_page_id` map.
fn parse_routes(raw: String) -> HashMap<String, String> {
    raw.split(',')
        .filter_map(|pair| {
            let (k, v) = pair.split_once('=')?;
            let (k, v) = (k.trim(), v.trim());
            (!k.is_empty() && !v.is_empty()).then(|| (k.to_owned(), v.to_owned()))
        })
        .collect()
}
