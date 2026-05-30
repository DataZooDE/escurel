use thiserror::Error;

/// Errors the demo agent can surface while talking to the gateway.
#[derive(Debug, Error)]
pub enum AgentError {
    #[error("transport: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("tool {tool} returned an error: {message}")]
    Tool { tool: String, message: String },
}
