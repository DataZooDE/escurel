//! Simulated external processing agent for the M7 event-sourcing demo.
//!
//! escurel models memory as a triad — **Events · Skills · Instances** —
//! where an instance's state **is the projection of its event sequence,
//! mediated by skills**. v1 keeps the gateway automation-free: the fold
//! event→state is performed by an **external** agent that reads the
//! inbox (notified by the capture webhook, or polling) and, using the
//! skill each event's `label_skill` points at as its context, folds the
//! event into the right instance's history.
//!
//! This crate makes that external processor concrete for the demo. The
//! "skill" reasoning is *simulated*: an event is folded into the
//! instance it was pre-flagged for (`instance_page_id`, gmail-label
//! style) — `assign_event` marks it `processed` and binds it to that
//! instance, so it joins the instance's `list_events` history (the
//! input the projection reads). A real agent would additionally
//! materialise new state via `update_page`; that richer step is out of
//! scope for the simulation.

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::{Value, json};

mod error;
pub use error::AgentError;

/// A minimal MCP-over-HTTP client scoped to the event/inbox tools the
/// agent needs. Talks the same `/mcp` JSON-RPC surface the gateway
/// exposes to every agent.
#[derive(Clone)]
pub struct McpClient {
    mcp_url: String,
    token: String,
    http: reqwest::Client,
}

/// One inbox event, as the agent cares about it.
#[derive(Debug, Clone, Deserialize)]
pub struct InboxEvent {
    pub event_id: String,
    #[serde(default)]
    pub label_skill: String,
    #[serde(default)]
    pub instance_page_id: Option<String>,
    #[serde(default)]
    pub title: String,
}

/// Outcome of one `process_inbox_once` pass.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ProcessReport {
    /// Events folded into an instance this pass.
    pub assigned: usize,
    /// Inbox events the agent left untouched (no routing target).
    pub skipped: usize,
}

impl McpClient {
    /// Build a client for the gateway's `/mcp` endpoint, authenticating
    /// with `token` (a bearer JWT).
    pub fn new(mcp_url: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            mcp_url: mcp_url.into(),
            token: token.into(),
            http: reqwest::Client::new(),
        }
    }

    /// Call one MCP tool, returning its `result` object.
    async fn call(&self, name: &str, args: Value) -> Result<Value, AgentError> {
        let resp = self
            .http
            .post(&self.mcp_url)
            .header("authorization", format!("Bearer {}", self.token))
            .json(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": { "name": name, "arguments": args },
            }))
            .send()
            .await?;
        let body: Value = resp.json().await?;
        if let Some(err) = body.get("error") {
            return Err(AgentError::Tool {
                tool: name.to_owned(),
                message: err.to_string(),
            });
        }
        // A `tools/call` result is an MCP `CallToolResult`
        // (`{content, structuredContent, isError}`). The raw tool payload
        // lives under `structuredContent`; fall back to `result` for any
        // non-tools/call response.
        let result = body.get("result").cloned().unwrap_or(Value::Null);
        Ok(result.get("structuredContent").cloned().unwrap_or(result))
    }

    /// Read the inbox (unprocessed events, newest first).
    pub async fn list_inbox(&self, limit: Option<usize>) -> Result<Vec<InboxEvent>, AgentError> {
        let mut args = json!({});
        if let Some(l) = limit {
            args["limit"] = json!(l);
        }
        let result = self.call("list_inbox", args).await?;
        let events = result
            .get("events")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(events
            .into_iter()
            .filter_map(|e| serde_json::from_value(e).ok())
            .collect())
    }

    /// Fold an event into an instance: mark it processed + bound.
    pub async fn assign_event(
        &self,
        event_id: &str,
        instance_page_id: &str,
    ) -> Result<(), AgentError> {
        self.call(
            "assign_event",
            json!({ "event_id": event_id, "instance_page_id": instance_page_id }),
        )
        .await?;
        Ok(())
    }
}

/// Decide which instance an inbox event folds into. The demo simulates
/// the skill-mediated routing an external agent would do: prefer the
/// event's pre-flagged `instance_page_id` (gmail-label style); failing
/// that, consult an optional `label_skill → instance` routing table.
/// Returns `None` when the event has no routing target (left in inbox).
pub fn route_event<'a>(
    event: &'a InboxEvent,
    routes: &'a HashMap<String, String>,
) -> Option<&'a str> {
    if let Some(id) = event.instance_page_id.as_deref()
        && !id.is_empty()
    {
        return Some(id);
    }
    routes.get(&event.label_skill).map(String::as_str)
}

/// Process the inbox once: route each event and fold the routable ones
/// into their instances. Pure orchestration over [`McpClient`] +
/// [`route_event`] so the routing policy is unit-testable in isolation.
pub async fn process_inbox_once(
    client: &McpClient,
    routes: &HashMap<String, String>,
) -> Result<ProcessReport, AgentError> {
    let inbox = client.list_inbox(None).await?;
    let mut report = ProcessReport::default();
    for event in &inbox {
        match route_event(event, routes) {
            Some(instance) => {
                client.assign_event(&event.event_id, instance).await?;
                report.assigned += 1;
                tracing::info!(
                    target: "escurel",
                    event_id = %event.event_id,
                    instance = %instance,
                    title = %event.title,
                    "agent: folded event into instance",
                );
            }
            None => report.skipped += 1,
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(label: &str, instance: Option<&str>) -> InboxEvent {
        InboxEvent {
            event_id: "e".into(),
            label_skill: label.into(),
            instance_page_id: instance.map(str::to_owned),
            title: String::new(),
        }
    }

    #[test]
    fn preflag_wins_over_route_table() {
        let routes = HashMap::from([("gmail".to_owned(), "from-route".to_owned())]);
        let e = ev("gmail", Some("from-preflag"));
        assert_eq!(route_event(&e, &routes), Some("from-preflag"));
    }

    #[test]
    fn falls_back_to_route_table_without_preflag() {
        let routes = HashMap::from([("gmail".to_owned(), "from-route".to_owned())]);
        assert_eq!(route_event(&ev("gmail", None), &routes), Some("from-route"));
    }

    #[test]
    fn empty_preflag_is_not_a_target() {
        let routes = HashMap::new();
        assert_eq!(route_event(&ev("gmail", Some("")), &routes), None);
    }

    #[test]
    fn unroutable_event_returns_none() {
        let routes = HashMap::from([("gmail".to_owned(), "x".to_owned())]);
        assert_eq!(route_event(&ev("mystery", None), &routes), None);
    }
}
