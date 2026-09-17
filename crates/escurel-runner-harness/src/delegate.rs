//! The [`DelegateHarness`] — the A2A control channel for a `harness: delegate`
//! step (async-ops Phase 4 slice 3c).
//!
//! escurel is the orchestrator; the domain implementation binaries live next to
//! the agent (the fleet), not in escurel. A delegate step therefore does not run
//! a model or a subprocess — it hands the work to the AGENT over A2A and waits
//! for the agent to name a result:
//!
//! 1. `message/send` (JSON-RPC 2.0) to the agent's A2A endpoint, carrying the
//!    requested capability + the step input, authenticated with the
//!    runner→agent delegation token (aud=agent, empty roles,
//!    `purpose=internal_delegation`; minted by
//!    [`escurel_runner_core::auth::Signer::mint_delegation`]).
//! 2. Poll `tasks/get` until the task reaches a terminal state, bounded by an
//!    overall timeout.
//! 3. On `completed`, read the result reference the agent placed in
//!    `task.metadata.result_ref` (an opaque [`escurel_types::ResultRef`] JSON,
//!    which the runner validates before stamping) and return it on the
//!    [`HarnessOutcome`]. The agent produced + published the tabular result
//!    itself; escurel resolves the reference and reads it back through its own
//!    validated ingest boundary (the "seal") — the data never flows over this
//!    control channel.
//!
//! The harness holds no per-task state: the endpoint, capability and token all
//! ride on the [`TaskContext`] (the token is minted fresh per requester), so a
//! task routed here without a [`Delegation`] fails closed with
//! [`HarnessError::Unsupported`] rather than delegating with no authority.

use std::time::Duration;

use async_trait::async_trait;
use escurel_runner_core::{Delegation, TaskContext};
use serde_json::{Value, json};

use crate::harness::{Harness, HarnessError, HarnessOutcome, HarnessStatus};

/// The adapter name, used for selection (`ESCUREL_RUNNER_HARNESS=delegate` /
/// a per-phase `harness: delegate`) and logging. Shared with the packager
/// (which builds the [`Delegation`]) via `escurel-runner-core`.
pub const NAME: &str = escurel_runner_core::DELEGATE_HARNESS;

/// How often to poll `tasks/get` while the delegated task is in flight.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// The overall wall-clock budget for a delegated task before the harness gives
/// up and reports [`HarnessError::Timeout`]. Domain work (a scenario what-if, a
/// forecast) can be slow, so this is generous; the workflow's own retry/timeout
/// policy governs above it.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);

/// Per-HTTP-REQUEST timeout on a single `message/send` / `tasks/get` call.
///
/// Distinct from [`DEFAULT_TIMEOUT`], which bounds the whole poll LOOP but is
/// only checked *between* polls: without a per-request timeout a hung
/// `send().await` inside [`DelegateHarness::rpc`] blocks forever (the #569 class
/// — an unbounded `Client::new()` on an LLM path hung a live pod until the
/// kubelet killed it). Generous enough for a slow agent turn, finite so a dead
/// endpoint fails instead of wedging the run.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// A harness that delegates a step to the agent over A2A and waits for a result
/// reference. See the module docs.
pub struct DelegateHarness {
    http: reqwest::Client,
    poll_interval: Duration,
    timeout: Duration,
}

impl Default for DelegateHarness {
    fn default() -> Self {
        Self::new()
    }
}

impl DelegateHarness {
    /// A delegate harness with the default poll interval + timeouts.
    #[must_use]
    pub fn new() -> Self {
        Self {
            http: build_client(DEFAULT_REQUEST_TIMEOUT),
            poll_interval: DEFAULT_POLL_INTERVAL,
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// Override the poll interval + overall timeout (tests use a short budget so
    /// a stuck agent does not stall the suite).
    #[must_use]
    pub fn with_timeouts(mut self, poll_interval: Duration, timeout: Duration) -> Self {
        self.poll_interval = poll_interval;
        self.timeout = timeout;
        self
    }

    /// Override the per-request HTTP timeout (rebuilds the client). A test points
    /// this at a hung endpoint with a short value to prove a stalled request
    /// degrades to an error instead of blocking forever.
    #[must_use]
    pub fn with_request_timeout(mut self, request_timeout: Duration) -> Self {
        self.http = build_client(request_timeout);
        self
    }

    /// One JSON-RPC 2.0 round-trip to the agent's A2A endpoint. Returns the
    /// `result` object (an A2A Task) or an [`HarnessError::Upstream`] naming
    /// what failed — a transport error, a non-2xx status, or a JSON-RPC `error`.
    async fn rpc(
        &self,
        d: &Delegation,
        method: &str,
        params: Value,
    ) -> Result<Value, HarnessError> {
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });
        let resp = self
            .http
            .post(&d.agent_a2a_url)
            .bearer_auth(d.token_str())
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                // A per-request timeout must surface as a clean Timeout, never a
                // silent hang — the point of the bounded client.
                if e.is_timeout() {
                    HarnessError::Timeout {
                        harness: NAME,
                        timeout_ms: 0,
                    }
                } else {
                    HarnessError::Upstream {
                        harness: NAME,
                        message: format!("A2A {method}: transport error: {e}"),
                    }
                }
            })?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| HarnessError::Upstream {
            harness: NAME,
            message: format!("A2A {method}: reading response body: {e}"),
        })?;
        if !status.is_success() {
            return Err(HarnessError::Upstream {
                harness: NAME,
                message: format!("A2A {method}: agent returned {status}: {}", truncate(&text)),
            });
        }
        let envelope: Value = serde_json::from_str(&text).map_err(|e| HarnessError::Upstream {
            harness: NAME,
            message: format!(
                "A2A {method}: response was not JSON: {e}: {}",
                truncate(&text)
            ),
        })?;
        if let Some(err) = envelope.get("error").filter(|e| !e.is_null()) {
            return Err(HarnessError::Upstream {
                harness: NAME,
                message: format!("A2A {method}: JSON-RPC error: {err}"),
            });
        }
        envelope
            .get("result")
            .cloned()
            .ok_or_else(|| HarnessError::Upstream {
                harness: NAME,
                message: format!(
                    "A2A {method}: response had no `result`: {}",
                    truncate(&text)
                ),
            })
    }

    /// Send the step to the agent and poll to a terminal task state.
    async fn send_and_poll(
        &self,
        d: &Delegation,
        input: &str,
    ) -> Result<HarnessOutcome, HarnessError> {
        // A unique-enough message id for the send; the agent assigns the task id.
        let message_id = format!(
            "esc-delegate-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let send_params = json!({
            "message": {
                "role": "user",
                "parts": [{ "kind": "text", "text": input }],
                "messageId": message_id,
            },
            "metadata": { "capability": d.capability },
        });

        let mut task = self.rpc(d, "message/send", send_params).await?;

        let started = std::time::Instant::now();
        loop {
            match task_state(&task) {
                // Terminal states.
                Some(TaskState::Completed) => return Ok(self.completed_outcome(&task)),
                Some(TaskState::Failed) | Some(TaskState::Canceled) | Some(TaskState::Rejected) => {
                    return Ok(HarnessOutcome {
                        ok: false,
                        status: HarnessStatus::Failed,
                        summary: format!(
                            "delegated task for capability {:?} ended {}",
                            d.capability,
                            task_state_str(&task).unwrap_or("(unknown)")
                        ),
                        tool_calls: 0,
                        produced_instance: None,
                        result_ref: None,
                    });
                }
                // Non-terminal (submitted / working / input-required / unknown):
                // keep polling until the budget is spent.
                _ => {}
            }
            if started.elapsed() >= self.timeout {
                return Err(HarnessError::Timeout {
                    harness: NAME,
                    timeout_ms: self.timeout.as_millis() as u64,
                });
            }
            tokio::time::sleep(self.poll_interval).await;
            let id = task_id(&task).ok_or_else(|| HarnessError::Upstream {
                harness: NAME,
                message: "A2A task carried no id to poll".to_owned(),
            })?;
            task = self.rpc(d, "tasks/get", json!({ "id": id })).await?;
        }
    }

    /// Build the success outcome from a completed A2A task, lifting the result
    /// reference the agent placed in `task.metadata.result_ref`.
    fn completed_outcome(&self, task: &Value) -> HarnessOutcome {
        let result_ref = task
            .get("metadata")
            .and_then(|m| m.get("result_ref"))
            .filter(|v| !v.is_null())
            .cloned();
        HarnessOutcome {
            ok: true,
            status: HarnessStatus::Ok,
            summary: "delegated task completed".to_owned(),
            tool_calls: 0,
            // Set later by the seal in `run`, once the produced instance is written.
            produced_instance: None,
            result_ref,
        }
    }

    /// The seal: write the step's produced instance over `/mcp` (`update_page`)
    /// with the run's scoped token, carrying the `result_ref`. This is what makes
    /// a delegate step confirm (the reducer reads this instance back) and what
    /// carries the result to `get_operation`.
    async fn seal_produced_instance(
        &self,
        mcp_endpoint: &str,
        token: &str,
        page_id: &str,
        capability: &str,
        result_ref: Option<&Value>,
    ) -> Result<(), HarnessError> {
        let content = build_instance_content(page_id, capability, result_ref);
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "update_page",
                "arguments": { "page_id": page_id, "content": content },
            },
        });
        let resp = self
            .http
            .post(mcp_endpoint)
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .map_err(|e| HarnessError::Upstream {
                harness: NAME,
                message: format!("seal update_page: transport error: {e}"),
            })?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(HarnessError::Upstream {
                harness: NAME,
                message: format!(
                    "seal update_page: gateway returned {status}: {}",
                    truncate(&text)
                ),
            });
        }
        // /mcp returns 200 with a JSON-RPC error object on tool failure.
        if let Ok(v) = serde_json::from_str::<Value>(&text)
            && let Some(err) = v.get("error").filter(|e| !e.is_null())
        {
            return Err(HarnessError::Upstream {
                harness: NAME,
                message: format!("seal update_page: {err}"),
            });
        }
        Ok(())
    }
}

/// Build the produced-instance markdown for the seal. The page id is
/// `markdown/instances/<skill>/<id>.md`; the frontmatter mirrors what a normal
/// harness writes (type/id/skill) so the reducer + readers treat it as an
/// ordinary instance, and the body records the delegated result reference.
fn build_instance_content(page_id: &str, capability: &str, result_ref: Option<&Value>) -> String {
    let rest = page_id
        .strip_prefix("markdown/instances/")
        .unwrap_or(page_id);
    let mut parts = rest.trim_end_matches(".md").splitn(2, '/');
    let skill = parts.next().unwrap_or(capability);
    let id = parts.next().unwrap_or("result");
    let rr = result_ref
        .map(|v| v.to_string())
        .unwrap_or_else(|| "null".to_owned());
    format!(
        "---\ntype: instance\nid: {id}\nskill: {skill}\n---\n\
         # {skill}\n\nDelegated `{capability}` result produced by the agent.\n\n\
         result_ref: {rr}\n"
    )
}

#[async_trait]
impl Harness for DelegateHarness {
    fn name(&self) -> &str {
        NAME
    }

    async fn run(&self, task: &TaskContext) -> Result<HarnessOutcome, HarnessError> {
        // Fail closed: a task routed to the delegate harness without delegation
        // parameters has no endpoint + no authority to delegate with. Refuse
        // (→ Permanent → dead-letter) rather than guess.
        let delegation = task.delegation().ok_or_else(|| HarnessError::Unsupported {
            harness: NAME,
            reason: "task carries no delegation parameters (not a delegate step)".to_owned(),
        })?;
        let mut outcome = self.send_and_poll(delegation, &task.input).await?;

        // THE SEAL. The agent produced the result off-instance and named it via
        // `result_ref`. Every other harness writes its produced instance over
        // `/mcp`, and the reducer confirms a step by reading that instance back;
        // a delegate step that wrote nothing would land as a converged no-op and
        // DROP the result_ref. So on success we write the step's produced
        // instance (carrying the result_ref) with the run's scoped token — the
        // step now confirms like any other and the result_ref reaches
        // `get_operation`.
        if outcome.ok
            && let Some(page_id) = delegation.produced_instance()
        {
            self.seal_produced_instance(
                &task.mcp_endpoint,
                task.token_str(),
                page_id,
                &delegation.capability,
                outcome.result_ref.as_ref(),
            )
            .await?;
            outcome.produced_instance = Some(page_id.to_owned());
        }
        Ok(outcome)
    }
}

/// The A2A task lifecycle states this harness distinguishes.
enum TaskState {
    Completed,
    Failed,
    Canceled,
    Rejected,
}

fn task_state_str(task: &Value) -> Option<&str> {
    task.get("status")
        .and_then(|s| s.get("state"))
        .and_then(Value::as_str)
}

fn task_state(task: &Value) -> Option<TaskState> {
    match task_state_str(task)? {
        "completed" => Some(TaskState::Completed),
        "failed" => Some(TaskState::Failed),
        "canceled" | "cancelled" => Some(TaskState::Canceled),
        "rejected" => Some(TaskState::Rejected),
        _ => None,
    }
}

fn task_id(task: &Value) -> Option<String> {
    task.get("id").and_then(Value::as_str).map(str::to_owned)
}

/// Build the harness's HTTP client with a per-request timeout so a hung agent
/// endpoint fails instead of blocking forever (#569 class). Falls back to a
/// default client only if the builder somehow rejects the timeout.
fn build_client(request_timeout: Duration) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(request_timeout)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// Truncate an upstream body for an error message so a large agent response
/// does not flood the log. Cuts on a CHAR boundary — `&s[..MAX]` panics when
/// MAX lands inside a multi-byte UTF-8 sequence (an agent reply is arbitrary
/// text, so this is reachable).
fn truncate(s: &str) -> String {
    const MAX: usize = 400;
    if s.len() <= MAX {
        return s.to_owned();
    }
    // Largest char boundary at or below MAX.
    let mut end = MAX;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_cuts_on_a_char_boundary_and_never_panics() {
        // A short ASCII string is returned unchanged.
        assert_eq!(truncate("hi"), "hi");
        // A long multi-byte string whose 400th byte lands mid-codepoint must not
        // panic and must stay valid UTF-8. '€' is 3 bytes, so 400 is never a
        // boundary for a run of them.
        let euros = "€".repeat(500); // 1500 bytes
        let out = truncate(&euros); // must not panic
        assert!(out.ends_with('…'));
        assert!(out.len() <= 400 + '…'.len_utf8());
        // Every retained byte is still valid UTF-8 (implicit: it's a &str).
        assert!(out.chars().filter(|c| *c == '€').count() > 0);
    }
}
