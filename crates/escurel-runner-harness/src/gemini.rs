//! The Gemini harness adapter — an in-process agent loop over HTTP.
//!
//! Every other adapter in this crate drives a *CLI* as a subprocess. That
//! works on a laptop, where `claude` or `codex` is already logged in, and it
//! does not work where the runner is actually deployed: a container has no
//! interactive auth, no node runtime, and nothing to log in with. The
//! harness a cluster can run is one that needs a single API key and speaks
//! HTTP.
//!
//! So [`GeminiHarness`] runs the loop itself:
//!
//! 1. ask the gateway `tools/list` for the schemas of the tools this run is
//!    allowed to call, and hand them to Gemini as `functionDeclarations`;
//! 2. send the packaged skill body as the system instruction and the event
//!    as the user turn;
//! 3. for every `functionCall` the model returns, make the real `/mcp` call
//!    under the packaged scoped token and feed the result back;
//! 4. stop when the model answers without calling a tool.
//!
//! The [`crate::Harness`] contract still holds, and holds more visibly here
//! than in the subprocess adapters: the adapter makes **no escurel decision
//! of its own**. Every escurel effect is a tool the model chose, from the
//! narrowed surface the packager allowed, under the scoped token — this code
//! is transport, and its `/mcp` call is the model's call, not its own.
//!
//! The tool surface is taken from `allowed_tools` and intersected with what
//! the gateway actually advertises. A name the packager allows but the
//! gateway does not serve is dropped rather than declared: a model told a
//! tool exists will call it, and a `method not found` mid-run is a wasted
//! turn and a confusing transcript.
//!
//! `base_url` is configurable so a deterministic test can point the adapter
//! at a real local HTTP server that speaks the Gemini wire shape — the same
//! technique the `claude` adapter's test uses with a stub executable,
//! exercising the whole build-request/parse-response path without burning
//! quota. The live test against real Gemini lives in `escurel-runner` behind
//! `#[ignore]`.

use std::time::Duration;

use async_trait::async_trait;
use escurel_runner_core::TaskContext;
use serde_json::{Value, json};

use crate::harness::{Harness, HarnessError, HarnessOutcome, HarnessStatus};

/// The adapter's stable name — the `ESCUREL_RUNNER_HARNESS=gemini` selector.
const NAME: &str = "gemini";

/// Default Generative Language API base. Overridable
/// (`ESCUREL_RUNNER_GEMINI_BASE_URL`) for the deterministic test.
pub const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

/// Default model. Overridable (`ESCUREL_RUNNER_GEMINI_MODEL`).
pub const DEFAULT_MODEL: &str = "gemini-2.5-flash";

/// Default per-run wall clock, covering every model turn and tool call.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);

/// How many model turns one run may take before the adapter stops it.
///
/// A bound, not a tuning knob: without one, a model that keeps calling a
/// failing tool loops until the timeout, and the run's cost is then a
/// function of the model's mood. Reaching it is a `Failed` outcome the
/// reconciler can retry — never a silent truncation reported as success.
const DEFAULT_MAX_TURNS: u32 = 12;

/// The JSON-Schema keys Gemini's `functionDeclarations` accepts.
///
/// escurel's `inputSchema`s carry keys Gemini rejects outright
/// (`additionalProperties`, `$schema`, `default`), and a rejected schema
/// fails the WHOLE request — one unusable tool would take the run with it.
/// So the schema is copied through this allowlist rather than filtered by a
/// denylist, which would need updating every time either side grows a key.
const SCHEMA_KEYS: [&str; 7] = [
    "type",
    "description",
    "properties",
    "required",
    "items",
    "enum",
    "format",
];

/// Adapter that drives Gemini over HTTP, in process.
#[derive(Debug, Clone)]
pub struct GeminiHarness {
    api_key: String,
    model: String,
    base_url: String,
    timeout: Duration,
    max_turns: u32,
}

impl GeminiHarness {
    /// Build an adapter authenticating with `api_key`.
    #[must_use]
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            model: DEFAULT_MODEL.to_owned(),
            base_url: DEFAULT_BASE_URL.to_owned(),
            timeout: DEFAULT_TIMEOUT,
            max_turns: DEFAULT_MAX_TURNS,
        }
    }

    /// Set the model (`ESCUREL_RUNNER_GEMINI_MODEL`). An empty value keeps
    /// the default.
    #[must_use]
    pub fn with_model(mut self, model: Option<String>) -> Self {
        if let Some(m) = model.filter(|m| !m.is_empty()) {
            self.model = m;
        }
        self
    }

    /// Point the adapter at a different API base
    /// (`ESCUREL_RUNNER_GEMINI_BASE_URL`). An empty value keeps the default.
    #[must_use]
    pub fn with_base_url(mut self, base: Option<String>) -> Self {
        if let Some(b) = base.filter(|b| !b.is_empty()) {
            self.base_url = b.trim_end_matches('/').to_owned();
        }
        self
    }

    /// Override the per-run wall clock.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Override the model-turn bound.
    #[must_use]
    pub fn with_max_turns(mut self, turns: u32) -> Self {
        if turns > 0 {
            self.max_turns = turns;
        }
        self
    }

    fn upstream(message: impl Into<String>) -> HarnessError {
        HarnessError::Upstream {
            harness: NAME,
            message: message.into(),
        }
    }

    /// One JSON-RPC call against the gateway, as the packaged agent.
    async fn mcp(
        client: &reqwest::Client,
        task: &TaskContext,
        method: &str,
        params: Value,
    ) -> Result<Value, HarnessError> {
        let body = client
            .post(&task.mcp_endpoint)
            .header("authorization", format!("Bearer {}", task.token_str()))
            .json(&json!({
                "jsonrpc": "2.0", "id": 1, "method": method, "params": params,
            }))
            .send()
            .await
            .map_err(|e| Self::upstream(format!("{method}: {e}")))?
            .json::<Value>()
            .await
            .map_err(|e| Self::upstream(format!("{method}: response was not JSON: {e}")))?;
        Ok(body)
    }

    /// Copy a JSON schema through [`SCHEMA_KEYS`], recursively.
    fn sanitize_schema(schema: &Value) -> Value {
        let Some(obj) = schema.as_object() else {
            return json!({ "type": "string" });
        };
        let mut out = serde_json::Map::new();
        for key in SCHEMA_KEYS {
            let Some(v) = obj.get(key) else { continue };
            let cleaned = match key {
                "properties" => {
                    let mut props = serde_json::Map::new();
                    for (name, sub) in v.as_object().into_iter().flatten() {
                        props.insert(name.clone(), Self::sanitize_schema(sub));
                    }
                    Value::Object(props)
                }
                "items" => Self::sanitize_schema(v),
                _ => v.clone(),
            };
            out.insert(key.to_owned(), cleaned);
        }
        // Gemini requires an object schema to declare `type`; escurel's do,
        // but a tool that ever omits it must not fail the whole request.
        out.entry("type".to_owned())
            .or_insert_with(|| json!("object"));
        Value::Object(out)
    }

    /// The `functionDeclarations` for this run: the gateway's own schemas for
    /// exactly the tools the packager allowed.
    async fn function_declarations(
        client: &reqwest::Client,
        task: &TaskContext,
    ) -> Result<Vec<Value>, HarnessError> {
        let listed = Self::mcp(client, task, "tools/list", json!({})).await?;
        let tools = listed["result"]["tools"]
            .as_array()
            .ok_or_else(|| Self::upstream(format!("tools/list returned no tools: {listed}")))?;
        let decls: Vec<Value> = tools
            .iter()
            .filter(|t| {
                t["name"]
                    .as_str()
                    .is_some_and(|n| task.allowed_tools.iter().any(|a| a == n))
            })
            .map(|t| {
                json!({
                    "name": t["name"],
                    "description": t["description"],
                    "parameters": Self::sanitize_schema(&t["inputSchema"]),
                })
            })
            .collect();
        if decls.is_empty() {
            // Fail loudly: an agent with no tools cannot fold an event, and a
            // model asked to do so will answer with prose that reads like
            // success. The likeliest cause is a packaged tool name the
            // gateway does not serve.
            return Err(Self::upstream(format!(
                "none of the packaged tools {:?} are advertised by the gateway",
                task.allowed_tools
            )));
        }
        Ok(decls)
    }

    /// The whole run, minus the timeout wrapper.
    async fn converse(&self, task: &TaskContext) -> Result<HarnessOutcome, HarnessError> {
        let client = reqwest::Client::new();
        let decls = Self::function_declarations(&client, task).await?;

        let mut contents = vec![json!({
            "role": "user",
            "parts": [{ "text": task.input }],
        })];
        let url = format!("{}/models/{}:generateContent", self.base_url, self.model);

        let mut tool_calls: u32 = 0;
        let mut produced_instance: Option<String> = None;
        let mut summary = String::new();

        for _turn in 0..self.max_turns {
            let request = json!({
                "systemInstruction": { "parts": [{ "text": task.instructions }] },
                "contents": contents,
                "tools": [{ "functionDeclarations": decls }],
            });
            let resp = client
                .post(&url)
                .header("x-goog-api-key", &self.api_key)
                .json(&request)
                .send()
                .await
                .map_err(|e| Self::upstream(format!("generateContent: {e}")))?;
            let status = resp.status();
            let body: Value = resp
                .json()
                .await
                .map_err(|e| Self::upstream(format!("generateContent: not JSON: {e}")))?;
            if !status.is_success() {
                // Carry the upstream's own message: "429" alone has cost real
                // debugging time on this project.
                return Err(Self::upstream(format!(
                    "generateContent {status}: {}",
                    body["error"]["message"].as_str().unwrap_or("no message")
                )));
            }

            let parts = body["candidates"][0]["content"]["parts"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            if parts.is_empty() {
                return Err(Self::upstream(format!(
                    "generateContent returned no parts: {body}"
                )));
            }

            let mut responses = Vec::new();
            for part in &parts {
                if let Some(text) = part["text"].as_str() {
                    summary.push_str(text);
                }
                let Some(call) = part.get("functionCall") else {
                    continue;
                };
                let name = call["name"].as_str().unwrap_or_default().to_owned();
                let args = call.get("args").cloned().unwrap_or_else(|| json!({}));

                // The narrowed surface is enforced HERE too, not only by
                // declaring it: the declarations are a prompt, and a prompt is
                // not a control. The gateway would refuse an out-of-surface
                // call anyway; refusing it before the wire keeps the scoped
                // token off a call the packager never allowed.
                if !task.allowed_tools.contains(&name) {
                    responses.push(json!({
                        "functionResponse": {
                            "name": name,
                            "response": { "error": "tool not allowed for this run" },
                        }
                    }));
                    continue;
                }

                tool_calls += 1;
                let result = Self::mcp(
                    &client,
                    task,
                    "tools/call",
                    json!({ "name": name, "arguments": args }),
                )
                .await?;
                let payload = result["result"]["structuredContent"].clone();
                let landed = payload.get("ok").and_then(Value::as_bool) != Some(false)
                    && result.get("error").is_none();
                if landed
                    && let Some(page) = args
                        .get("page_id")
                        .or_else(|| args.get("target_page_id"))
                        .and_then(Value::as_str)
                {
                    produced_instance = Some(page.to_owned());
                }
                responses.push(json!({
                    "functionResponse": {
                        "name": name,
                        // The model sees the error envelope too when there is
                        // one: a refused write it cannot read is a write it
                        // will repeat verbatim.
                        "response": if result.get("error").is_some() {
                            result["error"].clone()
                        } else {
                            payload
                        },
                    }
                }));
            }

            if responses.is_empty() {
                // No tool calls this turn: the model is done talking.
                return Ok(HarnessOutcome {
                    result_ref: None,
                    ok: true,
                    status: HarnessStatus::Ok,
                    summary: summary.trim().to_owned(),
                    tool_calls,
                    produced_instance,
                });
            }
            contents.push(json!({ "role": "model", "parts": parts }));
            contents.push(json!({ "role": "user", "parts": responses }));
        }

        // Out of turns. Reported as a run that FAILED, with what it did get
        // done — the reconciler decides retry-vs-dead, and it can only do
        // that if this is not dressed up as success.
        Ok(HarnessOutcome {
            result_ref: None,
            ok: false,
            status: HarnessStatus::Failed,
            summary: format!(
                "stopped after {} model turns without a final answer: {}",
                self.max_turns,
                summary.trim()
            ),
            tool_calls,
            produced_instance,
        })
    }
}

#[async_trait]
impl Harness for GeminiHarness {
    fn name(&self) -> &str {
        NAME
    }

    async fn run(&self, task: &TaskContext) -> Result<HarnessOutcome, HarnessError> {
        match tokio::time::timeout(self.timeout, self.converse(task)).await {
            Ok(result) => result,
            Err(_) => Err(HarnessError::Timeout {
                harness: NAME,
                timeout_ms: u64::try_from(self.timeout.as_millis()).unwrap_or(u64::MAX),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The keys Gemini rejects must not survive, and the ones it needs must.
    #[test]
    fn schema_sanitisation_keeps_the_contract_and_drops_the_rest() {
        let escurel_shaped = json!({
            "type": "object",
            "additionalProperties": true,
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "required": ["page_id"],
            "properties": {
                "page_id": { "type": "string", "description": "the page", "default": "x" },
                "tags": { "type": "array", "items": { "type": "string", "default": "y" } },
            },
        });
        let out = GeminiHarness::sanitize_schema(&escurel_shaped);
        assert_eq!(out["type"], json!("object"));
        assert_eq!(out["required"], json!(["page_id"]));
        assert_eq!(
            out["properties"]["page_id"]["description"],
            json!("the page")
        );
        assert_eq!(out["properties"]["tags"]["items"]["type"], json!("string"));
        assert!(out.get("additionalProperties").is_none());
        assert!(out.get("$schema").is_none());
        assert!(out["properties"]["page_id"].get("default").is_none());
        assert!(out["properties"]["tags"]["items"].get("default").is_none());
    }

    /// A schema that is not an object at all must still produce something
    /// declarable — one odd tool must not fail the whole request.
    #[test]
    fn a_non_object_schema_degrades_instead_of_failing() {
        assert_eq!(
            GeminiHarness::sanitize_schema(&json!("nonsense")),
            json!({ "type": "string" })
        );
        assert_eq!(
            GeminiHarness::sanitize_schema(&json!({ "properties": {} }))["type"],
            json!("object")
        );
    }
}
