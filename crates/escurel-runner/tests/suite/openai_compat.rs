//! The OpenAI-compatible adapter (`alibaba`, `hetzner`, `openai-compatible`)
//! against a REAL gateway and a real local HTTP server that speaks the
//! `/chat/completions` wire shape.
//!
//! What is real: the gateway, its `/mcp` tool calls, the writes. What stands
//! in is the MODEL — a scripted server, the only part that can be substituted
//! without making the test a quota burn. The server is not a rubber stamp:
//! every request the adapter built is recorded and asserted on (headers,
//! `tools`, `stream`, the message history), because the request shape is the
//! contract a real provider rejects with a 400.
//!
//! Covered: a streamed tool-call loop with reasoning content (alibaba), the
//! tool-support probe and a non-streamed loop (hetzner), a server without
//! tool support refused before any run turn, 429/5xx retried with backoff and
//! bounded, and the run's binary-level wiring (`alibaba_runner_end_to_end`).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use escurel_runner_core::{SecretString, TaskContext};
use escurel_runner_harness::{
    Harness, HarnessError, OpenAiCompatHarness, OpenAiPreset, ProviderSettings, ToolSupport,
};
use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const SKILL: &str = "customer";
const SKILL_BODY: &str = "---\nkind: skill\nid: customer\nautonomy: auto\n---\n# customer\n\n\
Fold the triggering event into the named customer instance.\n";
const INSTANCE_ID: &str = "globex";
const INSTANCE_BODY: &str =
    "---\nkind: instance\nid: globex\nskill: customer\n---\n# Globex\n\nBASELINE account state.\n";
const PAGE: &str = "markdown/instances/customer/globex.md";
const KEY: &str = "sk-test-not-a-real-credential";

/// One scripted reply.
#[derive(Clone)]
pub(crate) enum Reply {
    Json(u16, Value),
    /// `Retry-After: <secs>` plus a status and error body.
    Throttle(u16, u64),
    /// Server-sent events, one `data:` line each, then `[DONE]`.
    Sse(Vec<Value>),
}

/// What the fake model saw.
#[derive(Default)]
pub(crate) struct Seen {
    pub requests: Vec<Value>,
    pub auth: Vec<String>,
    pub paths: Vec<String>,
}

/// A real HTTP server that answers each request with the next scripted reply.
pub(crate) async fn spawn_fake_model(script: Vec<Reply>) -> (String, Arc<Mutex<Seen>>) {
    use axum::{
        Router,
        body::Body,
        extract::State,
        http::{HeaderMap, Response, Uri},
    };
    #[derive(Clone)]
    struct St {
        script: Arc<Mutex<VecDeque<Reply>>>,
        seen: Arc<Mutex<Seen>>,
    }
    async fn answer(
        State(st): State<St>,
        uri: Uri,
        headers: HeaderMap,
        body: String,
    ) -> Response<Body> {
        {
            let mut seen = st.seen.lock().expect("lock");
            seen.paths.push(uri.path().to_owned());
            seen.auth.push(
                headers
                    .get("authorization")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default()
                    .to_owned(),
            );
            seen.requests
                .push(serde_json::from_str(&body).unwrap_or(Value::Null));
        }
        let next = st.script.lock().expect("lock").pop_front();
        match next {
            Some(Reply::Json(status, v)) => Response::builder()
                .status(status)
                .header("content-type", "application/json")
                .body(Body::from(v.to_string()))
                .expect("response"),
            Some(Reply::Throttle(status, secs)) => Response::builder()
                .status(status)
                .header("retry-after", secs.to_string())
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"error":{"message":"Requests rate limit exceeded","code":"Throttling"}})
                        .to_string(),
                ))
                .expect("response"),
            Some(Reply::Sse(events)) => {
                let mut text = String::new();
                for e in events {
                    text.push_str(&format!("data: {e}\n\n"));
                }
                text.push_str("data: [DONE]\n\n");
                Response::builder()
                    .status(200)
                    .header("content-type", "text/event-stream")
                    .body(Body::from(text))
                    .expect("response")
            }
            None => Response::builder()
                .status(500)
                .body(Body::from("script exhausted"))
                .expect("response"),
        }
    }
    let seen = Arc::new(Mutex::new(Seen::default()));
    let app = Router::new().fallback(answer).with_state(St {
        script: Arc::new(Mutex::new(script.into())),
        seen: Arc::clone(&seen),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake model");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}/v1"), seen)
}

async fn gateway() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill(SKILL, SKILL_BODY)
                .instance(SKILL, INSTANCE_ID, INSTANCE_BODY)
                .done(),
        ),
        ..Default::default()
    })
    .await
}

fn task(gw: &EscurelProcess) -> TaskContext {
    TaskContext::for_test(
        "Fold the event into the instance.".to_owned(),
        "event: customer wants to renew".to_owned(),
        gw.mcp_url(),
        vec!["expand".to_owned(), "update_page".to_owned()],
        SecretString::from(gw.mint_token(TENANT, Role::Agent)),
    )
}

fn harness(preset: OpenAiPreset, base: &str, extra: ProviderSettings) -> OpenAiCompatHarness {
    OpenAiCompatHarness::new(
        preset,
        ProviderSettings {
            base_url: Some(base.to_owned()),
            api_key: KEY.to_owned(),
            retry_backoff: Some(Duration::from_millis(10)),
            ..extra
        },
    )
    .expect("configured")
}

fn folded_body(marker: &str) -> String {
    format!(
        "---\nkind: instance\nid: {INSTANCE_ID}\nskill: {SKILL}\n---\n# Globex\n\n\
         BASELINE account state.\n\n{marker}\n"
    )
}

fn usage() -> Value {
    json!({ "prompt_tokens": 120, "completion_tokens": 40, "total_tokens": 160 })
}

async fn body_of(gw: &EscurelProcess) -> String {
    let resp: Value = reqwest::Client::new()
        .post(gw.mcp_url())
        .bearer_auth(gw.mint_token(TENANT, Role::Agent))
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
            "params":{"name":"expand","arguments":{"page_id":PAGE}}}))
        .send()
        .await
        .expect("expand")
        .json()
        .await
        .expect("json");
    resp["result"]["structuredContent"]["body"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

#[tokio::test]
async fn alibaba_streams_a_tool_loop_and_keeps_reasoning_out_of_the_reply() {
    let gw = gateway().await;
    let args =
        json!({ "page_id": PAGE, "content": folded_body("QWEN_STREAMED_MARKER") }).to_string();
    let (first, rest) = args.split_at(args.len() / 2);
    let (base, seen) = spawn_fake_model(vec![
        Reply::Sse(vec![
            json!({"choices":[{"index":0,"delta":{"role":"assistant","reasoning_content":"I should "}}]}),
            json!({"choices":[{"index":0,"delta":{"reasoning_content":"write the page."}}]}),
            json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_a1",
                "type":"function","function":{"name":"update_page","arguments":first}}]}}]}),
            json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,
                "function":{"arguments":rest}}]}}]}),
            json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}),
            json!({"choices":[],"usage":usage()}),
        ]),
        Reply::Sse(vec![
            json!({"choices":[{"index":0,"delta":{"reasoning_content":"Done thinking."}}]}),
            json!({"choices":[{"index":0,"delta":{"content":"Folded the "}}]}),
            json!({"choices":[{"index":0,"delta":{"content":"event."},"finish_reason":"stop"}]}),
            json!({"choices":[],"usage":usage()}),
        ]),
    ])
    .await;
    let h = harness(OpenAiPreset::Alibaba, &base, ProviderSettings::default());
    assert_eq!(h.name(), "alibaba");

    let outcome = h.run(&task(&gw)).await.expect("run");
    assert!(outcome.ok, "{outcome:?}");
    assert_eq!(outcome.tool_calls, 1);
    assert_eq!(outcome.produced_instance.as_deref(), Some(PAGE));
    assert_eq!(outcome.reply.as_deref(), Some("Folded the event."));
    assert!(
        !outcome.summary.contains("thinking") && !outcome.summary.contains("should"),
        "reasoning must not reach the summary: {}",
        outcome.summary
    );
    let usage = outcome.usage.expect("usage");
    assert_eq!(usage.billed_tokens, Some(320));
    assert_eq!(usage.model.as_deref(), Some("qwen3.8-flash"));
    assert!(body_of(&gw).await.contains("QWEN_STREAMED_MARKER"));

    let seen = seen.lock().expect("lock");
    assert_eq!(seen.requests.len(), 2, "no probe for a native preset");
    assert!(seen.paths.iter().all(|p| p == "/v1/chat/completions"));
    assert!(seen.auth.iter().all(|a| a == &format!("Bearer {KEY}")));
    let req = &seen.requests[0];
    assert_eq!(req["model"], "qwen3.8-flash");
    assert_eq!(req["stream"], true);
    assert_eq!(req["stream_options"]["include_usage"], true);
    assert_eq!(req["tool_choice"], "auto");
    assert!(req.get("max_completion_tokens").is_some(), "{req}");
    let names: Vec<&str> = req["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .map(|t| t["function"]["name"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(names.len(), 2, "exactly the packaged surface: {names:?}");
    assert!(names.contains(&"update_page") && names.contains(&"expand"));
    assert_eq!(req["messages"][0]["role"], "system");
    assert_eq!(req["messages"][1]["role"], "user");

    // The second turn carries the assistant tool call, its reasoning (Qwen 3.8
    // requires it back) and the tool result under the call's id.
    let second = &seen.requests[1]["messages"];
    assert_eq!(second[2]["role"], "assistant");
    assert_eq!(second[2]["tool_calls"][0]["id"], "call_a1");
    assert_eq!(
        second[2]["tool_calls"][0]["function"]["arguments"].as_str(),
        Some(args.as_str()),
        "fragments must be reassembled in order"
    );
    assert_eq!(second[2]["reasoning_content"], "I should write the page.");
    assert_eq!(second[3]["role"], "tool");
    assert_eq!(second[3]["tool_call_id"], "call_a1");
    assert!(second[3]["content"].is_string(), "tool content is a string");
}

#[tokio::test]
async fn hetzner_probes_tool_support_then_runs_without_streaming() {
    let gw = gateway().await;
    let (base, seen) = spawn_fake_model(vec![
        // The probe.
        Reply::Json(
            200,
            json!({"choices":[{"index":0,"finish_reason":"tool_calls","message":{"role":"assistant",
                "content":null,"tool_calls":[{"id":"p1","type":"function",
                "function":{"name":"escurel_tool_probe","arguments":"{}"}}]}}],"usage":usage()}),
        ),
        Reply::Json(
            200,
            json!({"choices":[{"index":0,"finish_reason":"tool_calls","message":{"role":"assistant",
                "content":"","reasoning_content":"private chain of thought",
                "tool_calls":[{"id":"c1","type":"function","function":{"name":"update_page",
                "arguments": json!({"page_id":PAGE,"content":folded_body("HETZNER_MARKER")}).to_string()}}]}}],
                "usage":usage()}),
        ),
        Reply::Json(
            200,
            json!({"choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant",
                "content":"<think>check the write</think>Folded."}}],"usage":usage()}),
        ),
    ])
    .await;
    let h = harness(OpenAiPreset::Hetzner, &base, ProviderSettings::default());
    let outcome = h.run(&task(&gw)).await.expect("run");
    assert!(outcome.ok, "{outcome:?}");
    assert_eq!(outcome.reply.as_deref(), Some("Folded."));
    assert!(body_of(&gw).await.contains("HETZNER_MARKER"));

    let seen = seen.lock().expect("lock");
    assert_eq!(seen.requests.len(), 3, "probe + two turns");
    let probe = &seen.requests[0];
    assert_eq!(probe["tools"][0]["function"]["name"], "escurel_tool_probe");
    assert_eq!(probe["model"], "Qwen3.8-27B");
    for req in &seen.requests[1..] {
        assert_eq!(req["stream"], false, "streaming is not assumed: {req}");
        assert!(req.get("stream_options").is_none());
        assert!(req.get("max_tokens").is_some(), "{req}");
    }
    let assistant = &seen.requests[2]["messages"][2];
    assert_eq!(assistant["role"], "assistant");
    assert!(
        assistant.get("reasoning_content").is_none(),
        "reasoning is not echoed to a provider that does not ask for it: {assistant}"
    );
}

#[tokio::test]
async fn a_server_without_tool_calling_is_refused_before_any_run_turn() {
    let gw = gateway().await;
    // vLLM without `--enable-auto-tool-choice` answers exactly like this.
    let (base, seen) = spawn_fake_model(vec![Reply::Json(
        400,
        json!({"error":{"message":"\"auto\" tool choice requires --enable-auto-tool-choice and --tool-call-parser to be set","type":"BadRequestError"}}),
    )])
    .await;
    let h = harness(
        OpenAiPreset::Generic,
        &base,
        ProviderSettings {
            model: Some("some-model".into()),
            ..ProviderSettings::default()
        },
    );
    let err = h.run(&task(&gw)).await.expect_err("refused");
    assert!(
        matches!(
            err,
            HarnessError::Unsupported {
                harness: "openai-compatible",
                ..
            }
        ),
        "{err:?}"
    );
    let msg = err.to_string();
    assert!(msg.contains("require tool calling"), "{msg}");
    assert!(msg.contains("enable-auto-tool-choice"), "{msg}");
    assert_eq!(seen.lock().expect("lock").requests.len(), 1);

    // A server that silently ignores `tools` and answers in prose is the
    // same verdict — the failure this probe exists to catch.
    let (base, seen) = spawn_fake_model(vec![Reply::Json(
        200,
        json!({"choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant",
            "content":"I cannot call functions."}}],"usage":usage()}),
    )])
    .await;
    let h = harness(OpenAiPreset::Hetzner, &base, ProviderSettings::default());
    let err = h.run(&task(&gw)).await.expect_err("refused");
    assert!(matches!(err, HarnessError::Unsupported { .. }), "{err:?}");
    assert_eq!(seen.lock().expect("lock").requests.len(), 1);
    assert!(!body_of(&gw).await.contains("MARKER"));
}

#[tokio::test]
async fn rate_limits_and_5xx_are_retried_with_backoff_and_bounded() {
    let gw = gateway().await;
    let text_reply = Reply::Json(
        200,
        json!({"choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant",
            "content":"Nothing to fold."}}],"usage":usage()}),
    );
    let (base, seen) = spawn_fake_model(vec![
        Reply::Throttle(429, 0),
        Reply::Throttle(503, 0),
        text_reply,
    ])
    .await;
    let settings = ProviderSettings {
        model: Some("m".into()),
        tools: Some(ToolSupport::Native),
        max_retries: Some(2),
        ..ProviderSettings::default()
    };
    let h = harness(OpenAiPreset::Generic, &base, settings.clone());
    let outcome = h.run(&task(&gw)).await.expect("run after retries");
    assert!(outcome.ok, "{outcome:?}");
    assert_eq!(seen.lock().expect("lock").requests.len(), 3);

    // Bounded: three 429s against two retries is an upstream error carrying
    // the provider's message, not a hang.
    let (base, seen) = spawn_fake_model(vec![
        Reply::Throttle(429, 0),
        Reply::Throttle(429, 0),
        Reply::Throttle(429, 0),
        Reply::Throttle(429, 0),
    ])
    .await;
    let h = harness(OpenAiPreset::Generic, &base, settings.clone());
    let err = h.run(&task(&gw)).await.expect_err("gives up");
    let msg = err.to_string();
    assert!(matches!(err, HarnessError::Upstream { .. }), "{err:?}");
    assert!(msg.contains("429") && msg.contains("rate limit"), "{msg}");
    assert_eq!(seen.lock().expect("lock").requests.len(), 3);

    // A 401 is not retried at all.
    let (base, seen) = spawn_fake_model(vec![Reply::Json(
        401,
        // A provider that echoes the bearer back must not publish it.
        json!({"error":{"message":format!("Incorrect API key provided: {KEY}")}}),
    )])
    .await;
    let h = harness(OpenAiPreset::Generic, &base, settings);
    let err = h.run(&task(&gw)).await.expect_err("401");
    assert!(err.to_string().contains("Incorrect API key"), "{err}");
    assert!(!err.to_string().contains(KEY), "the key is redacted: {err}");
    assert_eq!(seen.lock().expect("lock").requests.len(), 1);
}

#[tokio::test]
async fn a_response_without_usage_fails_the_run_rather_than_skipping_the_budget() {
    let gw = gateway().await;
    let (base, _seen) = spawn_fake_model(vec![Reply::Json(
        200,
        json!({"choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant",
            "content":"ok"}}]}),
    )])
    .await;
    let h = harness(
        OpenAiPreset::Generic,
        &base,
        ProviderSettings {
            model: Some("m".into()),
            tools: Some(ToolSupport::Native),
            ..ProviderSettings::default()
        },
    );
    let outcome = h.run(&task(&gw)).await.expect("outcome");
    assert!(!outcome.ok);
    assert!(outcome.summary.contains("no usage"), "{}", outcome.summary);
}

struct ChildGuard(std::process::Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn call_tool(gw: &EscurelProcess, name: &str, args: Value) -> Value {
    let body: Value = reqwest::Client::new()
        .post(gw.mcp_url())
        .bearer_auth(gw.mint_token(TENANT, Role::Agent))
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
            "params":{"name":name,"arguments":args}}))
        .send()
        .await
        .expect("post /mcp")
        .json()
        .await
        .expect("json");
    assert!(body.get("error").is_none(), "{name}: {body}");
    body["result"]["structuredContent"].clone()
}

/// The runner binary, selected by `ESCUREL_RUNNER_HARNESS=alibaba`, reads its
/// key from a FILE, folds a real event through the fake model with the
/// default ordinary-run model, and never writes the key to its logs.
#[tokio::test]
async fn alibaba_runner_end_to_end_reads_the_key_file_and_keeps_it_out_of_logs() {
    let gw = gateway().await;
    let captured = call_tool(
        &gw,
        "capture_event",
        json!({ "source": "manual", "mime": "text/plain", "label_skill": SKILL,
            "instance_page_id": PAGE, "title": "renewal", "body": "customer wants to renew" }),
    )
    .await;
    let event_id = captured["event_id"].as_str().expect("event_id").to_owned();
    let args = json!({ "page_id": PAGE, "content": folded_body("RUNNER_QWEN_MARKER") }).to_string();
    let (base, seen) = spawn_fake_model(vec![
        Reply::Sse(vec![
            json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"c1","type":"function",
                "function":{"name":"update_page","arguments":args}}]},"finish_reason":"tool_calls"}]}),
            json!({"choices":[],"usage":usage()}),
        ]),
        Reply::Sse(vec![
            json!({"choices":[{"index":0,"delta":{"content":"Folded."},"finish_reason":"stop"}]}),
            json!({"choices":[],"usage":usage()}),
        ]),
    ])
    .await;

    let dir = tempfile::tempdir().expect("tempdir");
    let key_file = dir.path().join("alibaba-key");
    std::fs::write(&key_file, format!("{KEY}\n")).expect("key file");
    let log = std::fs::File::create(dir.path().join("runner.log")).expect("log");
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env(
        "ESCUREL_RUNNER_LISTEN",
        format!("127.0.0.1:{}", escurel_test_support::free_port()),
    )
    .env("ESCUREL_RUNNER_GATEWAY_URL", gw.base_url())
    .env("ESCUREL_RUNNER_TENANT", TENANT)
    .env("ESCUREL_RUNNER_TOKEN", gw.mint_token(TENANT, Role::Admin))
    .env("ESCUREL_RUNNER_HARNESS", "alibaba")
    .env("ESCUREL_RUNNER_ALIBABA_BASE_URL", &base)
    .env("ESCUREL_RUNNER_ALIBABA_API_KEY_FILE", &key_file)
    .env(
        "ESCUREL_RUNNER_LEDGER_PATH",
        dir.path().join("ledger.duckdb").to_str().expect("utf8"),
    )
    .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms")
    .stdout(log.try_clone().expect("log"))
    .stderr(log);
    let _runner = ChildGuard(cmd.spawn().expect("spawn escurel-runner"));

    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        let events = call_tool(&gw, "list_events", json!({ "instance_page_id": PAGE })).await;
        if events["events"].as_array().is_some_and(|es| {
            es.iter()
                .any(|e| e["event_id"] == json!(event_id) && e["status"] == json!("processed"))
        }) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the alibaba harness never folded {event_id}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert!(body_of(&gw).await.contains("RUNNER_QWEN_MARKER"));
    {
        let seen = seen.lock().expect("lock");
        assert_eq!(seen.requests[0]["model"], "qwen3.8-flash");
        assert!(seen.auth.iter().all(|a| a == &format!("Bearer {KEY}")));
    }
    drop(_runner);
    let logs = std::fs::read_to_string(dir.path().join("runner.log")).expect("logs");
    assert!(!logs.is_empty(), "the runner logged something");
    assert!(!logs.contains(KEY), "the API key must never reach the logs");
}

/// A preset that cannot run refuses to START — exit 2, naming the variable —
/// rather than dispatching events it cannot serve.
#[test]
fn a_misconfigured_preset_refuses_to_start() {
    let dir = tempfile::tempdir().expect("tempdir");
    for (harness, extra) in [
        // No key file at all.
        ("alibaba", vec![]),
        // A generic endpoint with a key but no base URL or model.
        (
            "openai-compatible",
            vec![("ESCUREL_RUNNER_OPENAI_API_KEY_FILE", "/nonexistent/key")],
        ),
    ] {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
        cmd.env(
            "ESCUREL_RUNNER_LISTEN",
            format!("127.0.0.1:{}", escurel_test_support::free_port()),
        )
        .env("ESCUREL_RUNNER_HARNESS", harness)
        .env(
            "ESCUREL_RUNNER_LEDGER_PATH",
            dir.path().join(format!("{harness}.duckdb")),
        );
        for (k, v) in extra {
            cmd.env(k, v);
        }
        let log_path = dir.path().join(format!("{harness}.log"));
        let log = std::fs::File::create(&log_path).expect("log");
        cmd.stdout(log.try_clone().expect("log")).stderr(log);
        let mut child = ChildGuard(cmd.spawn().expect("spawn escurel-runner"));
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        let status = loop {
            if let Some(status) = child.0.try_wait().expect("wait") {
                break status;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "{harness}: the runner kept running instead of refusing to start"
            );
            std::thread::sleep(Duration::from_millis(100));
        };
        assert_eq!(status.code(), Some(2), "{harness}: {status:?}");
        let text = std::fs::read_to_string(&log_path).expect("log");
        assert!(text.contains("misconfigured"), "{harness}: {text}");
    }
}
