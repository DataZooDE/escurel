//! The OpenAI-compatible harness adapter — an in-process agent loop over any
//! `/chat/completions` endpoint.
//!
//! The same shape as [`crate::GeminiHarness`], for the same deployment reason
//! (a container has no CLI to log in with): the adapter asks the gateway for
//! the schemas of the tools this run may call, hands them to the model as
//! `tools`, makes the real `/mcp` call for every `tool_calls` entry the model
//! returns under the packaged scoped token, and stops when the model answers
//! without calling a tool. It makes **no escurel decision of its own**.
//!
//! One adapter, several **presets** — the selector value names the provider,
//! so the run's events and `ESCUREL_RUNNER_HARNESS_ALLOW` say which one ran:
//!
//! | preset | default endpoint | default models | streaming | tool calling |
//! |---|---|---|---|---|
//! | `alibaba` | Model Studio, China (Beijing) | `qwen3.8-flash` / long `qwen3.8-max` | on | documented |
//! | `hetzner` | Hetzner Inference API | `Qwen3.8-27B` | off | probed |
//! | `openai-compatible` | none (must be set) | none (must be set) | off | probed |
//!
//! **Tool calling is not assumed where a provider does not document it.** An
//! escurel run without tools cannot fold an event, and a model that silently
//! ignores `tools` answers with prose that reads like success. So a `probe`
//! preset sends one small request with a single declared tool before its
//! run and refuses — [`HarnessError::Unsupported`], a permanent failure
//! naming the endpoint and model — unless the model calls it. A positive
//! verdict is cached per `(endpoint, model)` for the life of the process; a
//! negative one is not, so a later run probes again.
//!
//! **Reasoning content** (`reasoning_content`, or `reasoning` on some
//! servers, or an inline `<think>…</think>` block) never reaches the reply or
//! the run summary. Within a run it is sent back on the assistant turn only
//! where the provider requires it (Qwen 3.8 on Model Studio: "you must send
//! back all historical `reasoning_content`"); elsewhere it is dropped, because
//! some OpenAI-compatible servers reject the unknown field.
//!
//! **Retries:** 429 and 500/502/503/504, and connection failures before a
//! request was sent, are retried with exponential backoff (honouring a
//! `Retry-After` in seconds, capped). Nothing else is: a 400 or 401 answers
//! the same way however long you wait, and a stream that fails half way may
//! already have been billed.
//!
//! **Secrets:** the API key is read from a file by the runner
//! ([`read_api_key_file`]), held only here, sent only as a bearer header, and
//! redacted from `Debug`. A key is never sent over plain HTTP to anything but
//! a loopback address.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use escurel_runner_core::TaskContext;
use serde_json::{Value, json};

use crate::harness::{Harness, HarnessError, HarnessOutcome, HarnessStatus, Usage};

/// Default per-run wall clock, covering every model turn and tool call.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);
/// Default bound on one HTTP request to the model, including reading a stream.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(180);
/// Model turns per run; the same bound, for the same reason, as Gemini's.
const DEFAULT_MAX_TURNS: u32 = 24;
const DEFAULT_MAX_TOTAL_TOKENS: u64 = 250_000;
const MAX_OUTPUT_TOKENS_PER_TURN: u64 = 8_192;
const DEFAULT_MAX_RETRIES: u32 = 3;
const DEFAULT_RETRY_BACKOFF: Duration = Duration::from_secs(1);
/// Longest single wait between retries, whatever `Retry-After` asks for.
const MAX_RETRY_WAIT: Duration = Duration::from_secs(30);
/// The tool the capability probe declares.
const PROBE_TOOL: &str = "escurel_tool_probe";
/// Tool calls one streamed turn may carry. The stream's `index` is the
/// provider's number; without a bound, one event with a huge index would
/// allocate without limit in a loop no timeout can preempt.
const MAX_TOOL_CALLS_PER_TURN: usize = 64;
/// Bytes one turn may accumulate (content, reasoning, arguments, buffer).
const MAX_TURN_BYTES: usize = 8 * 1024 * 1024;

/// A provider preset of the OpenAI-compatible adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    /// Alibaba Cloud Model Studio (Qwen), OpenAI-compatible mode.
    Alibaba,
    /// Hetzner Inference API (experimental).
    Hetzner,
    /// Any other OpenAI-compatible endpoint; nothing is assumed.
    Generic,
}

pub use escurel_runner_core::ToolSupport;

impl Preset {
    /// Every preset, for selector parsing and documentation.
    pub const ALL: [Self; 3] = [Self::Alibaba, Self::Hetzner, Self::Generic];

    /// The selector a preset is chosen by (`ESCUREL_RUNNER_HARNESS=<name>`)
    /// and the harness name its runs report.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Alibaba => "alibaba",
            Self::Hetzner => "hetzner",
            Self::Generic => "openai-compatible",
        }
    }

    /// The preset a selector names.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.name() == name)
    }

    /// The endpoint used when none is configured. Model Studio's Beijing
    /// compatible-mode host; a workspace-specific
    /// `https://{WorkspaceId}.cn-beijing.maas.aliyuncs.com/compatible-mode/v1`
    /// is what Alibaba recommends and is set via the base-URL variable.
    #[must_use]
    pub fn default_base_url(self) -> Option<&'static str> {
        match self {
            Self::Alibaba => Some("https://dashscope.aliyuncs.com/compatible-mode/v1"),
            Self::Hetzner => Some("https://inference.hetzner.com/api/v1"),
            Self::Generic => None,
        }
    }

    /// The model for ordinary runs when none is configured.
    #[must_use]
    pub fn default_model(self) -> Option<&'static str> {
        match self {
            Self::Alibaba => Some("qwen3.8-flash"),
            Self::Hetzner => Some("Qwen3.8-27B"),
            Self::Generic => None,
        }
    }

    /// The model for long, multi-step runs (workflow steps) when none is
    /// configured; `None` means "the ordinary model".
    #[must_use]
    pub fn default_long_model(self) -> Option<&'static str> {
        match self {
            Self::Alibaba => Some("qwen3.8-max"),
            Self::Hetzner | Self::Generic => None,
        }
    }

    fn default_stream(self) -> bool {
        matches!(self, Self::Alibaba)
    }

    fn default_tools(self) -> ToolSupport {
        match self {
            Self::Alibaba => ToolSupport::Native,
            Self::Hetzner | Self::Generic => ToolSupport::Probe,
        }
    }

    /// Qwen 3.8 on Model Studio requires historical `reasoning_content` back.
    fn echoes_reasoning(self) -> bool {
        matches!(self, Self::Alibaba)
    }

    /// The output-limit field: `max_completion_tokens` is the current OpenAI
    /// name and Model Studio's preferred one; `max_tokens` is what every
    /// vLLM-style server accepts.
    fn max_tokens_field(self) -> &'static str {
        match self {
            Self::Hetzner => "max_tokens",
            Self::Alibaba | Self::Generic => "max_completion_tokens",
        }
    }
}

/// What a deployment configured for one preset. Every `None` takes the
/// preset's default; the runner fills this from `ESCUREL_RUNNER_<PRESET>_*`.
#[derive(Clone, Default)]
pub struct ProviderSettings {
    pub base_url: Option<String>,
    /// The API key itself (read from its file by the caller). Redacted.
    pub api_key: String,
    pub model: Option<String>,
    pub stream: Option<bool>,
    pub tools: Option<ToolSupport>,
    /// Passed through as `reasoning_effort` when set.
    pub reasoning_effort: Option<String>,
    pub max_turns: Option<u32>,
    pub max_total_tokens: Option<u64>,
    pub timeout: Option<Duration>,
    pub request_timeout: Option<Duration>,
    pub max_retries: Option<u32>,
    pub retry_backoff: Option<Duration>,
}

impl std::fmt::Debug for ProviderSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderSettings")
            .field("base_url", &self.base_url)
            .field("api_key", &"<redacted>")
            .field("model", &self.model)
            .field("stream", &self.stream)
            .field("tools", &self.tools)
            .finish_non_exhaustive()
    }
}

/// Read an API key from `path`: trimmed, non-empty. The error names the path
/// and never the content.
///
/// # Errors
/// [`HarnessError::Unsupported`] when the file is unreadable or empty — the
/// runner turns it into a startup refusal.
pub fn read_api_key_file(harness: &'static str, path: &str) -> Result<String, HarnessError> {
    let raw = std::fs::read_to_string(path).map_err(|e| HarnessError::Unsupported {
        harness,
        reason: format!("cannot read the API key file {path:?}: {}", e.kind()),
    })?;
    let key = raw.trim();
    if key.is_empty() || key.contains(char::is_whitespace) {
        return Err(HarnessError::Unsupported {
            harness,
            reason: format!("the API key file {path:?} is empty or holds more than one token"),
        });
    }
    Ok(key.to_owned())
}

/// Adapter that drives an OpenAI-compatible chat endpoint, in process.
#[derive(Clone)]
pub struct OpenAiCompatHarness {
    preset: Preset,
    base_url: String,
    api_key: String,
    model: String,
    stream: bool,
    tools: ToolSupport,
    reasoning_effort: Option<String>,
    timeout: Duration,
    request_timeout: Duration,
    max_turns: u32,
    max_total_tokens: u64,
    max_retries: u32,
    retry_backoff: Duration,
}

impl std::fmt::Debug for OpenAiCompatHarness {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiCompatHarness")
            .field("preset", &self.preset)
            .field("base_url", &self.base_url)
            .field("api_key", &"<redacted>")
            .field("model", &self.model)
            .field("stream", &self.stream)
            .field("tools", &self.tools)
            .field("timeout", &self.timeout)
            .field("max_turns", &self.max_turns)
            .field("max_total_tokens", &self.max_total_tokens)
            .finish_non_exhaustive()
    }
}

/// One parsed model turn.
#[derive(Debug, Default)]
struct Turn {
    content: String,
    reasoning: String,
    tool_calls: Vec<ToolCall>,
    finish_reason: Option<String>,
    usage: Option<Usage>,
}

#[derive(Debug, Default, Clone)]
struct ToolCall {
    id: String,
    name: String,
    arguments: String,
}

/// Why a model request did not produce a response.
enum SendError {
    /// The endpoint answered with a non-success status.
    Status(reqwest::StatusCode, String),
    /// The request never completed.
    Transport(String),
}

impl OpenAiCompatHarness {
    /// Build the adapter for `preset` from what the deployment configured.
    ///
    /// # Errors
    /// [`HarnessError::Unsupported`] when the configuration cannot run: no
    /// endpoint or model for a preset without defaults, an empty key, or an
    /// endpoint that would carry the key over plain HTTP off the host.
    pub fn new(preset: Preset, settings: ProviderSettings) -> Result<Self, HarnessError> {
        let harness = preset.name();
        let refuse = |reason: String| HarnessError::Unsupported { harness, reason };
        let base_url = settings
            .base_url
            .filter(|b| !b.trim().is_empty())
            .or_else(|| preset.default_base_url().map(str::to_owned))
            .ok_or_else(|| refuse("no base URL is configured".to_owned()))?
            .trim()
            .trim_end_matches('/')
            .to_owned();
        // The URL is never quoted back: the inputs refused here (userinfo, a
        // query) are the ones that carry secrets, and the refusal is logged.
        let url = reqwest::Url::parse(&base_url)
            .map_err(|e| refuse(format!("the base URL is not a URL: {e}")))?;
        let loopback = matches!(
            url.host_str(),
            Some("127.0.0.1" | "localhost" | "[::1]" | "::1")
        );
        match url.scheme() {
            "https" => {}
            "http" if loopback => {}
            _ => {
                return Err(refuse(
                    "the base URL must be https (plain http only to a loopback address): the \
                     API key travels in a header"
                        .to_owned(),
                ));
            }
        }
        if !url.username().is_empty() || url.password().is_some() || url.query().is_some() {
            return Err(refuse(
                "the base URL must not carry credentials or a query".to_owned(),
            ));
        }
        let model = settings
            .model
            .map(|m| m.trim().to_owned())
            .filter(|m| !m.is_empty())
            .or_else(|| preset.default_model().map(str::to_owned))
            .ok_or_else(|| refuse("no model is configured".to_owned()))?;
        if settings.api_key.trim().is_empty() {
            return Err(refuse("the API key is empty".to_owned()));
        }
        Ok(Self {
            preset,
            base_url,
            api_key: settings.api_key.trim().to_owned(),
            model,
            stream: settings.stream.unwrap_or_else(|| preset.default_stream()),
            tools: settings.tools.unwrap_or_else(|| preset.default_tools()),
            reasoning_effort: settings.reasoning_effort.filter(|r| !r.trim().is_empty()),
            timeout: settings.timeout.unwrap_or(DEFAULT_TIMEOUT),
            request_timeout: settings.request_timeout.unwrap_or(DEFAULT_REQUEST_TIMEOUT),
            max_turns: settings
                .max_turns
                .filter(|t| *t > 0)
                .unwrap_or(DEFAULT_MAX_TURNS),
            max_total_tokens: settings
                .max_total_tokens
                .filter(|t| *t > 0)
                .unwrap_or(DEFAULT_MAX_TOTAL_TOKENS),
            max_retries: settings.max_retries.unwrap_or(DEFAULT_MAX_RETRIES),
            retry_backoff: settings.retry_backoff.unwrap_or(DEFAULT_RETRY_BACKOFF),
        })
    }

    /// The model this adapter sends.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    fn name_static(&self) -> &'static str {
        self.preset.name()
    }

    fn upstream(&self, message: impl Into<String>) -> HarnessError {
        HarnessError::Upstream {
            harness: self.name_static(),
            message: message.into(),
        }
    }

    fn failed(
        summary: String,
        usage: Option<Usage>,
        tool_calls: u32,
        produced_instance: Option<String>,
    ) -> HarnessOutcome {
        HarnessOutcome {
            ok: false,
            status: HarnessStatus::Failed,
            summary,
            tool_calls,
            produced_instance,
            result_ref: None,
            usage,
            reply: None,
        }
    }

    /// POST one chat request, retrying what is worth retrying.
    async fn send(
        &self,
        client: &reqwest::Client,
        body: &Value,
        cancel: Option<&escurel_runner_core::Cancel>,
    ) -> Result<reqwest::Response, SendError> {
        let url = format!("{}/chat/completions", self.base_url);
        let mut backoff = self.retry_backoff;
        let mut attempt = 0;
        loop {
            let sent = client
                .post(&url)
                .bearer_auth(&self.api_key)
                .timeout(self.request_timeout)
                .json(body)
                .send()
                .await;
            let wait = match sent {
                Ok(resp) if resp.status().is_success() => return Ok(resp),
                Ok(resp) => {
                    let status = resp.status();
                    let retry_after = resp
                        .headers()
                        .get(reqwest::header::RETRY_AFTER)
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.trim().parse::<u64>().ok())
                        .map(Duration::from_secs);
                    let retryable = status == reqwest::StatusCode::TOO_MANY_REQUESTS
                        || matches!(status.as_u16(), 500 | 502 | 503 | 504);
                    if !retryable || attempt >= self.max_retries {
                        let text = resp.text().await.unwrap_or_default();
                        return Err(SendError::Status(status, error_message(&text)));
                    }
                    retry_after.unwrap_or(backoff)
                }
                // Only a request that never left: a timeout may have reached
                // the model, and retrying it multiplies the bound.
                Err(e) if e.is_connect() && attempt < self.max_retries => backoff,
                Err(e) => return Err(SendError::Transport(e.to_string())),
            };
            if cancel.is_some_and(escurel_runner_core::Cancel::is_cancelled) {
                return Err(SendError::Transport("cancelled".to_owned()));
            }
            tokio::time::sleep(wait.min(MAX_RETRY_WAIT)).await;
            backoff = backoff.saturating_mul(2);
            attempt += 1;
        }
    }

    /// Remove the API key from provider-derived text before it becomes an
    /// error, a log line or a run record: a provider or proxy that echoes
    /// the bearer back ("invalid key sk-…") must not publish it.
    fn redact(&self, text: &str) -> String {
        text.replace(&self.api_key, "<redacted>")
    }

    fn send_error(&self, what: &str, e: SendError) -> HarnessError {
        match e {
            SendError::Status(status, message) => {
                self.upstream(format!("{what} {status}: {}", self.redact(&message)))
            }
            SendError::Transport(message) => {
                self.upstream(format!("{what}: {}", self.redact(&message)))
            }
        }
    }

    /// Verify, once per `(endpoint, model)`, that the model calls a declared
    /// tool. A `native` preset skips this.
    async fn ensure_tool_calling(
        &self,
        client: &reqwest::Client,
        task: &TaskContext,
    ) -> Result<(), HarnessError> {
        static VERDICTS: OnceLock<Mutex<HashMap<(String, String), bool>>> = OnceLock::new();
        if self.tools == ToolSupport::Native {
            return Ok(());
        }
        let key = (self.base_url.clone(), self.model.clone());
        let verdicts = VERDICTS.get_or_init(|| Mutex::new(HashMap::new()));
        let known = verdicts
            .lock()
            .ok()
            .and_then(|v| v.get(&key).copied())
            .filter(|v| *v);
        let mut detail = String::new();
        let supported = match known {
            Some(v) => v,
            None => {
                let mut body = json!({
                    "model": self.model,
                    "messages": [{ "role": "user", "content":
                        format!("Call the function `{PROBE_TOOL}` now. Do not answer in text.") }],
                    "tools": [{ "type": "function", "function": {
                        "name": PROBE_TOOL,
                        "description": "Connectivity probe. Call it with no arguments.",
                        "parameters": { "type": "object", "properties": {} },
                    } }],
                    "tool_choice": "auto",
                    "stream": false,
                });
                body[self.preset.max_tokens_field()] = json!(1024);
                let verdict = match self.send(client, &body, task.cancel.as_ref()).await {
                    Ok(resp) => {
                        let parsed: Value = resp
                            .json()
                            .await
                            .map_err(|e| self.upstream(format!("tool probe: not JSON: {e}")))?;
                        parsed["choices"][0]["message"]["tool_calls"]
                            .as_array()
                            .is_some_and(|calls| {
                                calls.iter().any(|c| c["function"]["name"] == PROBE_TOOL)
                            })
                    }
                    // A 4xx is the endpoint telling us it will not take `tools`.
                    Err(SendError::Status(status, message))
                        if status.is_client_error()
                            && status != reqwest::StatusCode::UNAUTHORIZED
                            && status != reqwest::StatusCode::FORBIDDEN
                            && status != reqwest::StatusCode::TOO_MANY_REQUESTS =>
                    {
                        detail = format!(
                            " (the endpoint answered {status}: {})",
                            self.redact(&message)
                        );
                        false
                    }
                    Err(e) => return Err(self.send_error("tool probe", e)),
                };
                // Only a positive verdict is remembered. A negative one may be
                // one model's whim under `tool_choice: auto`; caching it would
                // fail every later run until a restart.
                if verdict && let Ok(mut v) = verdicts.lock() {
                    v.insert(key, true);
                }
                verdict
            }
        };
        if supported {
            Ok(())
        } else {
            Err(HarnessError::Unsupported {
                harness: self.name_static(),
                reason: format!(
                    "model {:?} at {} did not call a declared tool when probed{detail}; \
                     escurel runs require tool calling (function calling with \
                     tool_choice=auto). Choose a model or endpoint that supports it, or set \
                     the preset's TOOLS=native if the provider documents it",
                    self.model, self.base_url
                ),
            })
        }
    }

    /// One model turn: build the request, send it, parse the answer.
    async fn turn(
        &self,
        client: &reqwest::Client,
        task: &TaskContext,
        messages: &[Value],
        tools: &[Value],
        max_output: u64,
    ) -> Result<Turn, HarnessError> {
        let mut body = json!({
            "model": self.model,
            "messages": messages,
            "tools": tools,
            "tool_choice": "auto",
            "stream": self.stream,
        });
        body[self.preset.max_tokens_field()] = json!(max_output);
        if self.stream {
            body["stream_options"] = json!({ "include_usage": true });
        }
        if let Some(effort) = &self.reasoning_effort {
            body["reasoning_effort"] = json!(effort);
        }
        let resp = self
            .send(client, &body, task.cancel.as_ref())
            .await
            .map_err(|e| self.send_error("chat/completions", e))?;
        let mut turn = if self.stream {
            self.read_stream(resp).await?
        } else {
            let bytes = resp
                .bytes()
                .await
                .map_err(|e| self.upstream(format!("chat/completions: {e}")))?;
            if bytes.len() > MAX_TURN_BYTES {
                return Err(self.upstream(format!(
                    "chat/completions response exceeded {MAX_TURN_BYTES} bytes"
                )));
            }
            let parsed: Value = serde_json::from_slice(&bytes)
                .map_err(|e| self.upstream(format!("chat/completions: not JSON: {e}")))?;
            parse_message(&parsed, &self.model)
        };
        if turn.tool_calls.len() > MAX_TOOL_CALLS_PER_TURN {
            return Err(self.upstream(format!(
                "{} tool calls in one turn exceed the bound {MAX_TOOL_CALLS_PER_TURN}",
                turn.tool_calls.len()
            )));
        }
        let (content, inline_reasoning) = split_think(&turn.content);
        turn.content = content;
        if !inline_reasoning.is_empty() {
            turn.reasoning.push_str(&inline_reasoning);
        }
        Ok(turn)
    }

    /// Accumulate a server-sent-event stream into one [`Turn`].
    async fn read_stream(&self, mut resp: reqwest::Response) -> Result<Turn, HarnessError> {
        let mut turn = Turn::default();
        let mut buf: Vec<u8> = Vec::new();
        let mut done = false;
        let mut received: usize = 0;
        while !done {
            let chunk = resp
                .chunk()
                .await
                .map_err(|e| self.upstream(format!("chat/completions stream: {e}")))?;
            let Some(chunk) = chunk else { break };
            received = received.saturating_add(chunk.len());
            if received > MAX_TURN_BYTES {
                return Err(self.upstream(format!(
                    "chat/completions stream exceeded {MAX_TURN_BYTES} bytes in one turn"
                )));
            }
            buf.extend_from_slice(&chunk);
            while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = buf.drain(..=pos).collect();
                let line = String::from_utf8_lossy(&line);
                let Some(data) = line.trim().strip_prefix("data:") else {
                    continue;
                };
                let data = data.trim();
                if data == "[DONE]" {
                    done = true;
                    break;
                }
                let event: Value = serde_json::from_str(data).map_err(|e| {
                    self.upstream(format!("chat/completions stream: bad event: {e}"))
                })?;
                if let Some(err) = event.get("error") {
                    return Err(self.upstream(format!(
                        "chat/completions stream error: {}",
                        self.redact(&error_message(&err.to_string()))
                    )));
                }
                apply_delta(&mut turn, &event, &self.model)
                    .map_err(|m| self.upstream(format!("chat/completions stream: {m}")))?;
            }
        }
        // A connection cut mid-stream is not a finished turn: half a tool
        // call's arguments would reach the model as its own mistake.
        if !done && turn.finish_reason.is_none() {
            return Err(
                self.upstream("chat/completions stream ended before [DONE] or a finish_reason")
            );
        }
        Ok(turn)
    }

    /// The whole run, minus the timeout wrapper.
    async fn converse(&self, task: &TaskContext) -> Result<HarnessOutcome, HarnessError> {
        let name = self.name_static();
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| self.upstream(format!("HTTP client: {e}")))?;
        let tools: Vec<Value> = crate::mcp::allowed_tools(&client, task, name)
            .await?
            .iter()
            .map(|t| {
                let mut parameters = t["inputSchema"].clone();
                if let Some(obj) = parameters.as_object_mut() {
                    obj.remove("$schema");
                } else {
                    parameters = json!({ "type": "object", "properties": {} });
                }
                json!({ "type": "function", "function": {
                    "name": t["name"],
                    "description": t["description"],
                    "parameters": parameters,
                } })
            })
            .collect();
        self.ensure_tool_calling(&client, task).await?;

        let mut messages = vec![
            json!({ "role": "system", "content": task.instructions }),
            json!({ "role": "user", "content": task.input }),
        ];
        let mut tool_calls: u32 = 0;
        let mut produced_instance: Option<String> = None;
        let mut summary = String::new();
        let mut usage: Option<Usage> = None;
        let mut billed_tokens = task.prior_model_tokens;

        for turn_no in 0..self.max_turns {
            if billed_tokens >= self.max_total_tokens {
                return Ok(Self::failed(
                    format!(
                        "{name} run token budget exhausted: {billed_tokens} >= {}",
                        self.max_total_tokens
                    ),
                    usage,
                    tool_calls,
                    produced_instance,
                ));
            }
            if task.cancel.as_ref().is_some_and(|c| c.is_cancelled()) {
                return Err(HarnessError::Cancelled { harness: name });
            }
            let max_output = MAX_OUTPUT_TOKENS_PER_TURN.min(self.max_total_tokens - billed_tokens);
            let turn = self
                .turn(&client, task, &messages, &tools, max_output)
                .await?;

            let Some(turn_usage) = turn.usage.clone() else {
                return Ok(Self::failed(
                    format!("{name} response has no usage; token budget cannot be checked"),
                    usage,
                    tool_calls,
                    produced_instance,
                ));
            };
            billed_tokens = billed_tokens.saturating_add(turn_usage.billed_tokens.unwrap_or(0));
            usage
                .get_or_insert_with(Usage::default)
                .accumulate(&turn_usage);
            if billed_tokens > self.max_total_tokens {
                return Ok(Self::failed(
                    format!(
                        "{name} run token budget exceeded: {billed_tokens} > {}",
                        self.max_total_tokens
                    ),
                    usage,
                    tool_calls,
                    produced_instance,
                ));
            }
            summary.push_str(&turn.content);

            if turn.tool_calls.is_empty() {
                if turn.finish_reason.as_deref() == Some("length") {
                    // A truncated answer is not an answer.
                    return Ok(Self::failed(
                        format!(
                            "{name} answer was cut off at the output limit: {}",
                            summary.trim()
                        ),
                        usage,
                        tool_calls,
                        produced_instance,
                    ));
                }
                let answer = turn.content.trim();
                return Ok(HarnessOutcome {
                    ok: true,
                    status: HarnessStatus::Ok,
                    summary: summary.trim().to_owned(),
                    tool_calls,
                    produced_instance,
                    result_ref: None,
                    usage,
                    reply: (!answer.is_empty()).then(|| answer.to_owned()),
                });
            }

            let calls: Vec<ToolCall> = turn
                .tool_calls
                .iter()
                .enumerate()
                .map(|(i, c)| ToolCall {
                    id: if c.id.is_empty() {
                        format!("call_{turn_no}_{i}")
                    } else {
                        c.id.clone()
                    },
                    ..c.clone()
                })
                .collect();
            let mut assistant = json!({
                "role": "assistant",
                "content": if turn.content.is_empty() { Value::Null } else { json!(turn.content) },
                "tool_calls": calls.iter().map(|c| json!({
                    "id": c.id, "type": "function",
                    "function": { "name": c.name, "arguments": c.arguments },
                })).collect::<Vec<_>>(),
            });
            if self.preset.echoes_reasoning() && !turn.reasoning.is_empty() {
                assistant["reasoning_content"] = json!(turn.reasoning);
            }
            messages.push(assistant);

            for call in &calls {
                let content = if !task.allowed_tools.contains(&call.name) {
                    // The declarations are a prompt, not a control: refuse an
                    // out-of-surface call before the wire, and name what IS
                    // available so the model does not retry the same name.
                    json!({ "error": format!(
                        "tool `{}` is not available for this run; call one of: {}",
                        call.name,
                        task.allowed_tools.join(", ")
                    ) })
                } else {
                    let args_raw = if call.arguments.trim().is_empty() {
                        "{}"
                    } else {
                        call.arguments.as_str()
                    };
                    match serde_json::from_str::<Value>(args_raw) {
                        Ok(args) if args.is_object() => {
                            tool_calls += 1;
                            let result = crate::mcp::call(
                                &client,
                                task,
                                "tools/call",
                                json!({ "name": call.name, "arguments": args }),
                                name,
                            )
                            .await?;
                            if crate::mcp::landed(&result)
                                && let Some(page) = crate::mcp::written_page(&args)
                            {
                                produced_instance = Some(page);
                            }
                            // The model sees the error envelope too: a refused
                            // write it cannot read is one it repeats verbatim.
                            if let Some(err) = result.get("error") {
                                json!({ "error": err })
                            } else {
                                // A refusal's payload (`ok: false`, issues) is
                                // passed on as is: the model must read why.
                                escurel_types::call_result::payload_of(&result["result"])
                            }
                        }
                        // Malformed arguments are the model's mistake to
                        // correct, not the run's to fail; it was NOT executed.
                        _ => json!({ "error": format!(
                            "the arguments for `{}` were not a JSON object and the call was NOT \
                             executed; call it again with arguments as a JSON object matching \
                             its parameters",
                            call.name
                        ) }),
                    }
                };
                messages.push(json!({
                    "role": "tool",
                    "tool_call_id": call.id,
                    "content": content.to_string(),
                }));
            }
        }

        Ok(Self::failed(
            format!(
                "stopped after {} model turns without a final answer: {}",
                self.max_turns,
                summary.trim()
            ),
            usage,
            tool_calls,
            produced_instance,
        ))
    }
}

/// The provider's own message out of an error body, truncated — "429" alone
/// has cost real debugging time on this project.
fn error_message(body: &str) -> String {
    let parsed: Option<Value> = serde_json::from_str(body).ok();
    let message = parsed
        .as_ref()
        .and_then(|v| {
            v["error"]["message"]
                .as_str()
                .or_else(|| v["message"].as_str())
                .or_else(|| v["error"].as_str())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| body.to_owned());
    let message = message.trim();
    if message.is_empty() {
        "no message".to_owned()
    } else {
        message.chars().take(500).collect()
    }
}

/// `usage` of a response or final stream chunk.
fn parse_usage(v: &Value, model: &str) -> Option<Usage> {
    let u = v.get("usage")?.as_object()?;
    let input_tokens = u.get("prompt_tokens").and_then(Value::as_u64)?;
    let output_tokens = u
        .get("completion_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let total = u
        .get("total_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .max(input_tokens.saturating_add(output_tokens));
    Some(Usage {
        input_tokens,
        output_tokens,
        billed_tokens: Some(total),
        cost_usd: None,
        model: Some(model.to_owned()),
    })
}

fn reasoning_of(v: &Value) -> Option<&str> {
    v["reasoning_content"]
        .as_str()
        .or_else(|| v["reasoning"].as_str())
}

/// A non-streaming `chat.completion` body.
fn parse_message(body: &Value, model: &str) -> Turn {
    let choice = &body["choices"][0];
    let message = &choice["message"];
    Turn {
        content: message["content"].as_str().unwrap_or_default().to_owned(),
        reasoning: reasoning_of(message).unwrap_or_default().to_owned(),
        tool_calls: message["tool_calls"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|c| ToolCall {
                id: c["id"].as_str().unwrap_or_default().to_owned(),
                name: c["function"]["name"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                arguments: match &c["function"]["arguments"] {
                    Value::String(s) => s.clone(),
                    Value::Null => String::new(),
                    // Some servers send the object itself.
                    other => other.to_string(),
                },
            })
            .collect(),
        finish_reason: choice["finish_reason"].as_str().map(str::to_owned),
        usage: parse_usage(body, model),
    }
}

/// Fold one `chat.completion.chunk` into the turn being read. Tool calls
/// arrive by `index`: `id` and `function.name` in the first chunk for that
/// index, `function.arguments` as fragments to concatenate.
fn apply_delta(turn: &mut Turn, event: &Value, model: &str) -> Result<(), String> {
    if let Some(u) = parse_usage(event, model) {
        turn.usage = Some(u);
    }
    let Some(choice) = event["choices"].get(0) else {
        return Ok(());
    };
    let delta = &choice["delta"];
    if let Some(text) = delta["content"].as_str() {
        turn.content.push_str(text);
    }
    if let Some(text) = reasoning_of(delta) {
        turn.reasoning.push_str(text);
    }
    for call in delta["tool_calls"].as_array().into_iter().flatten() {
        let index = call["index"]
            .as_u64()
            .and_then(|i| usize::try_from(i).ok())
            .unwrap_or(turn.tool_calls.len());
        if index >= MAX_TOOL_CALLS_PER_TURN {
            return Err(format!(
                "tool call index {index} exceeds the per-turn bound {MAX_TOOL_CALLS_PER_TURN}"
            ));
        }
        while turn.tool_calls.len() <= index {
            turn.tool_calls.push(ToolCall::default());
        }
        let slot = &mut turn.tool_calls[index];
        if let Some(id) = call["id"].as_str().filter(|s| !s.is_empty()) {
            slot.id = id.to_owned();
        }
        if let Some(name) = call["function"]["name"].as_str().filter(|s| !s.is_empty()) {
            slot.name = name.to_owned();
        }
        if let Some(args) = call["function"]["arguments"].as_str() {
            slot.arguments.push_str(args);
        }
    }
    if let Some(reason) = choice["finish_reason"].as_str() {
        turn.finish_reason = Some(reason.to_owned());
    }
    Ok(())
}

/// Split inline `<think>…</think>` reasoning out of `content`, returning
/// `(answer, reasoning)`. A missing opening tag (some servers put it in the
/// prompt template) still ends the reasoning at `</think>`.
fn split_think(content: &str) -> (String, String) {
    let mut answer = String::new();
    let mut reasoning = String::new();
    let mut rest = content;
    if !rest.contains("<think>")
        && let Some(end) = rest.find("</think>")
    {
        reasoning.push_str(&rest[..end]);
        rest = &rest[end + "</think>".len()..];
    }
    while let Some(start) = rest.find("<think>") {
        answer.push_str(&rest[..start]);
        let after = &rest[start + "<think>".len()..];
        match after.find("</think>") {
            Some(end) => {
                reasoning.push_str(&after[..end]);
                rest = &after[end + "</think>".len()..];
            }
            None => {
                // An unterminated block is reasoning cut off mid-thought.
                reasoning.push_str(after);
                rest = "";
            }
        }
    }
    answer.push_str(rest);
    (answer.trim().to_owned(), reasoning)
}

#[async_trait]
impl Harness for OpenAiCompatHarness {
    fn name(&self) -> &str {
        self.preset.name()
    }

    async fn run(&self, task: &TaskContext) -> Result<HarnessOutcome, HarnessError> {
        match tokio::time::timeout(self.timeout, self.converse(task)).await {
            Ok(result) => result,
            Err(_) => Err(HarnessError::Timeout {
                harness: self.name_static(),
                timeout_ms: u64::try_from(self.timeout.as_millis()).unwrap_or(u64::MAX),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(key: &str) -> ProviderSettings {
        ProviderSettings {
            api_key: key.to_owned(),
            ..ProviderSettings::default()
        }
    }

    #[test]
    fn presets_carry_their_documented_defaults() {
        let a = OpenAiCompatHarness::new(Preset::Alibaba, settings("sk-test")).unwrap();
        assert_eq!(a.model(), "qwen3.8-flash");
        assert_eq!(
            a.base_url,
            "https://dashscope.aliyuncs.com/compatible-mode/v1"
        );
        assert!(a.stream);
        assert_eq!(a.tools, ToolSupport::Native);
        assert_eq!(Preset::Alibaba.default_long_model(), Some("qwen3.8-max"));

        let h = OpenAiCompatHarness::new(Preset::Hetzner, settings("hz")).unwrap();
        assert_eq!(h.model(), "Qwen3.8-27B");
        assert_eq!(h.base_url, "https://inference.hetzner.com/api/v1");
        assert!(!h.stream, "Hetzner does not document streaming");
        assert_eq!(h.tools, ToolSupport::Probe, "nor tool calling");
        assert_eq!(h.name(), "hetzner");
    }

    #[test]
    fn misconfiguration_is_refused_with_a_reason() {
        // A generic endpoint has no defaults to fall back on.
        let err = OpenAiCompatHarness::new(Preset::Generic, settings("k")).unwrap_err();
        assert!(err.to_string().contains("no base URL"), "{err}");
        let err = OpenAiCompatHarness::new(
            Preset::Generic,
            ProviderSettings {
                base_url: Some("https://llm.example/v1".into()),
                ..settings("k")
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("no model"), "{err}");
        // An empty key.
        let err = OpenAiCompatHarness::new(Preset::Alibaba, settings("  ")).unwrap_err();
        assert!(err.to_string().contains("API key is empty"), "{err}");
        // A key over plain HTTP off the host.
        let err = OpenAiCompatHarness::new(
            Preset::Alibaba,
            ProviderSettings {
                base_url: Some("http://llm.example/v1".into()),
                ..settings("k")
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("must be https"), "{err}");
        // Loopback HTTP is the test path and stays allowed.
        assert!(
            OpenAiCompatHarness::new(
                Preset::Alibaba,
                ProviderSettings {
                    base_url: Some("http://127.0.0.1:9/v1/".into()),
                    ..settings("k")
                },
            )
            .is_ok()
        );
        // Credentials in the URL would end up in logs.
        assert!(
            OpenAiCompatHarness::new(
                Preset::Alibaba,
                ProviderSettings {
                    base_url: Some("https://user:pw@llm.example/v1".into()),
                    ..settings("k")
                },
            )
            .is_err()
        );
    }

    #[test]
    fn the_key_never_appears_in_debug_output() {
        let h = OpenAiCompatHarness::new(Preset::Alibaba, settings("sk-sp-SECRET123")).unwrap();
        assert!(!format!("{h:?}").contains("SECRET123"));
        assert!(!format!("{:?}", settings("sk-sp-SECRET123")).contains("SECRET123"));
    }

    #[test]
    fn the_key_file_is_read_trimmed_and_its_errors_do_not_quote_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key");
        std::fs::write(&path, "sk-sp-SECRET123\n").unwrap();
        assert_eq!(
            read_api_key_file("alibaba", path.to_str().unwrap()).unwrap(),
            "sk-sp-SECRET123"
        );
        std::fs::write(&path, "sk-sp-SECRET123 trailing\n").unwrap();
        let err = read_api_key_file("alibaba", path.to_str().unwrap()).unwrap_err();
        assert!(!err.to_string().contains("SECRET123"), "{err}");
        let err = read_api_key_file("alibaba", "/nonexistent/key").unwrap_err();
        assert!(err.to_string().contains("cannot read"), "{err}");
    }

    #[test]
    fn inline_think_blocks_are_split_from_the_answer() {
        assert_eq!(
            split_think("<think>plan it</think>\nDone."),
            ("Done.".to_owned(), "plan it".to_owned())
        );
        assert_eq!(
            split_think("plan it</think>Done."),
            ("Done.".to_owned(), "plan it".to_owned())
        );
        assert_eq!(split_think("Done."), ("Done.".to_owned(), String::new()));
        assert_eq!(
            split_think("<think>cut off"),
            (String::new(), "cut off".to_owned())
        );
    }

    #[test]
    fn stream_deltas_assemble_split_tool_calls_and_usage() {
        let mut turn = Turn::default();
        for ev in [
            json!({"choices":[{"delta":{"reasoning_content":"think "}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","type":"function",
                "function":{"name":"update_page","arguments":"{\"page_"}}]}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"id\":\"x\"}"}}]}}]}),
            json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
            json!({"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}),
        ] {
            apply_delta(&mut turn, &ev, "m").expect("bounded");
        }
        assert_eq!(turn.reasoning, "think ");
        assert_eq!(turn.tool_calls.len(), 1);
        assert_eq!(turn.tool_calls[0].name, "update_page");
        assert_eq!(turn.tool_calls[0].arguments, "{\"page_id\":\"x\"}");
        assert_eq!(turn.finish_reason.as_deref(), Some("tool_calls"));
        assert_eq!(turn.usage.unwrap().billed_tokens, Some(15));
    }

    #[test]
    fn a_huge_streamed_tool_call_index_is_refused_before_allocating() {
        let mut turn = Turn::default();
        let ev = json!({"choices":[{"delta":{"tool_calls":[{"index":4_294_967_295_u64,
            "function":{"name":"x","arguments":"{}"}}]}}]});
        let err = apply_delta(&mut turn, &ev, "m").expect_err("bounded");
        assert!(err.contains("exceeds the per-turn bound"), "{err}");
        assert!(turn.tool_calls.is_empty());
    }

    #[test]
    fn provider_text_is_redacted_and_rejected_urls_are_not_quoted() {
        let h = OpenAiCompatHarness::new(Preset::Alibaba, settings("sk-sp-SECRET123")).unwrap();
        let err = h.send_error(
            "chat/completions",
            SendError::Status(
                reqwest::StatusCode::UNAUTHORIZED,
                "Invalid API key sk-sp-SECRET123".to_owned(),
            ),
        );
        assert!(!err.to_string().contains("SECRET123"), "{err}");
        for url in [
            "https://user:SECRET123@llm.example/v1",
            "https://llm.example/v1?api_key=SECRET123",
            "http://SECRET123.example/v1",
        ] {
            let err = OpenAiCompatHarness::new(
                Preset::Generic,
                ProviderSettings {
                    base_url: Some(url.to_owned()),
                    model: Some("m".to_owned()),
                    ..settings("k")
                },
            )
            .unwrap_err();
            assert!(!err.to_string().contains("SECRET123"), "{err}");
        }
    }

    #[test]
    fn error_messages_carry_the_providers_text_truncated() {
        assert_eq!(
            error_message(r#"{"error":{"message":"Rate limit","code":"Throttling"}}"#),
            "Rate limit"
        );
        assert_eq!(error_message(""), "no message");
        assert_eq!(error_message(&"x".repeat(900)).len(), 500);
    }
}
