//! The one JSON-RPC call an in-process harness makes against the gateway.
//!
//! Shared by the in-process adapters ([`crate::GeminiHarness`],
//! [`crate::OpenAiCompatHarness`]): both make the model's `/mcp` calls under
//! the packaged scoped token, and a second copy of this would drift the way
//! the five subprocess copies of `run_capture` once did.

use escurel_runner_core::TaskContext;
use serde_json::{Value, json};

use crate::harness::HarnessError;

/// One JSON-RPC call against the gateway, as the packaged agent.
pub(crate) async fn call(
    client: &reqwest::Client,
    task: &TaskContext,
    method: &str,
    params: Value,
    harness: &'static str,
) -> Result<Value, HarnessError> {
    let upstream = |message: String| HarnessError::Upstream { harness, message };
    client
        .post(&task.mcp_endpoint)
        .header("authorization", format!("Bearer {}", task.token_str()))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": method, "params": params,
        }))
        .send()
        .await
        .map_err(|e| upstream(format!("{method}: {e}")))?
        .json::<Value>()
        .await
        .map_err(|e| upstream(format!("{method}: response was not JSON: {e}")))
}

/// The gateway's `tools/list` entries for exactly the tools the packager
/// allowed. An empty intersection is an error: an agent with no tools cannot
/// fold an event, and a model asked to will answer with prose that reads like
/// success.
pub(crate) async fn allowed_tools(
    client: &reqwest::Client,
    task: &TaskContext,
    harness: &'static str,
) -> Result<Vec<Value>, HarnessError> {
    let listed = call(client, task, "tools/list", json!({}), harness)
        .await
        .map_err(|e| HarnessError::Preflight {
            harness,
            message: e.to_string(),
        })?;
    let tools = listed["result"]["tools"]
        .as_array()
        .ok_or_else(|| HarnessError::Preflight {
            harness,
            message: format!("tools/list returned no tools: {listed}"),
        })?;
    let allowed: Vec<Value> = tools
        .iter()
        .filter(|t| {
            t["name"]
                .as_str()
                .is_some_and(|n| task.allowed_tools.iter().any(|a| a == n))
        })
        .cloned()
        .collect();
    if allowed.is_empty() {
        return Err(HarnessError::Upstream {
            harness,
            message: format!(
                "none of the packaged tools {:?} are advertised by the gateway",
                task.allowed_tools
            ),
        });
    }
    Ok(allowed)
}

/// Whether a `tools/call` result landed: not refused (`isError`, `ok: false`)
/// and not a JSON-RPC error.
pub(crate) fn landed(result: &Value) -> bool {
    result.get("error").is_none()
        && escurel_types::call_result::refusal_of(&result["result"]).is_none()
}

/// The page a landed call wrote to, read from its arguments.
pub(crate) fn written_page(args: &Value) -> Option<String> {
    args.get("page_id")
        .or_else(|| args.get("target_page_id"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}
