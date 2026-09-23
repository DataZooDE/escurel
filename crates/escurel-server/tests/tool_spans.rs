//! A run's `/mcp` calls are OpenInference `TOOL` spans on the run's own
//! trace (knowledge-workbench backend P3-3). Standalone on purpose: it owns
//! the process-global `tracing` subscriber (an in-memory OTel exporter), like
//! `logs_json.rs`, so it cannot share a process with the suite.
//!
//! Real gateway, real auth (a run-bound bearer carrying `trace_id`), a real
//! OTel pipeline ending in memory instead of a collector.

use std::sync::OnceLock;

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use serde_json::{Value, json};
use tracing_subscriber::layer::SubscriberExt;

const TENANT: &str = "carl";
const RUN: &str = "01HRUNSPANS000000000000000";
const ROOT: &str = "01HROOTSPANS00000000000000";
const TRACE: &str = "0123456789abcdef0123456789abcdef";
const SKILL: &str = "---\ntype: skill\nid: note\ndescription: d.\n---\n# note\n";

static PIPELINE: OnceLock<(InMemorySpanExporter, SdkTracerProvider)> = OnceLock::new();

fn pipeline() -> &'static (InMemorySpanExporter, SdkTracerProvider) {
    PIPELINE.get_or_init(|| {
        let exporter = InMemorySpanExporter::default();
        let provider = SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build();
        let tracer = provider.tracer("escurel-test");
        let subscriber =
            tracing_subscriber::registry().with(tracing_opentelemetry::layer().with_tracer(tracer));
        tracing::subscriber::set_global_default(subscriber).expect("global subscriber");
        (exporter, provider)
    })
}

async fn call(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": { "name": name, "arguments": args } }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json")
}

fn attr<'a>(
    span: &'a opentelemetry_sdk::trace::SpanData,
    key: &str,
) -> Option<&'a opentelemetry::Value> {
    span.attributes
        .iter()
        .find(|kv| kv.key.as_str() == key)
        .map(|kv| &kv.value)
}

#[tokio::test]
async fn a_run_bound_call_is_a_tool_span_on_the_runs_trace() {
    let (exporter, provider) = pipeline();
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("note", SKILL)
                .done(),
        ),
        ..Default::default()
    })
    .await;
    let agent = p.mint_token_for_run_traced(TENANT, Role::Agent, "agent:note", RUN, ROOT, TRACE);
    let plain = p.mint_token(TENANT, Role::Agent);

    let ok = call(&p, &agent, "list_skills", json!({})).await;
    assert!(ok.get("error").is_none(), "{ok}");
    let _ = call(&p, &plain, "list_skills", json!({})).await;
    provider.force_flush().expect("flush");

    let spans = exporter.get_finished_spans().expect("spans");
    let requests: Vec<_> = spans.iter().filter(|s| s.name == "mcp.request").collect();
    assert!(
        requests.len() >= 2,
        "two mcp.request spans: {}",
        requests.len()
    );
    let tool_span = requests
        .iter()
        .find(|s| attr(s, "escurel.run_id").is_some_and(|v| v.to_string() == RUN))
        .unwrap_or_else(|| {
            panic!(
                "no span carrying the run: {:?}",
                requests.iter().map(|s| &s.attributes).collect::<Vec<_>>()
            )
        });
    assert_eq!(
        attr(tool_span, "openinference.span.kind")
            .map(ToString::to_string)
            .as_deref(),
        Some("TOOL")
    );
    assert_eq!(
        attr(tool_span, "escurel.root_event_id")
            .map(ToString::to_string)
            .as_deref(),
        Some(ROOT)
    );
    assert_eq!(
        attr(tool_span, "tool").map(ToString::to_string).as_deref(),
        Some("list_skills")
    );
    assert!(
        attr(tool_span, "input.size").is_some() && attr(tool_span, "output.size").is_some(),
        "{:?}",
        tool_span.attributes
    );
    // On the run's trace, not a fresh one: the token's trace_id is the parent.
    assert_eq!(tool_span.span_context.trace_id().to_string(), TRACE);
    assert_ne!(
        tool_span.parent_span_id,
        opentelemetry::trace::SpanId::INVALID,
        "a remote parent"
    );
    // The plain call is an ordinary request span.
    let plain_span = requests
        .iter()
        .find(|s| attr(s, "escurel.run_id").is_none())
        .expect("plain span");
    assert!(
        attr(plain_span, "openinference.span.kind").is_none(),
        "{:?}",
        plain_span.attributes
    );
    assert_ne!(plain_span.span_context.trace_id().to_string(), TRACE);
    p.shutdown().await;
}
