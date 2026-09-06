//! End-to-end DoD for the Gemini adapter — the harness a container can run.
//!
//! Every other adapter drives a CLI. That is fine on a laptop, where `claude`
//! is already logged in, and useless where the runner is deployed: no
//! interactive auth, no node runtime, nothing to log in with. The Gemini
//! adapter runs the tool loop in process over HTTP, so this test proves the
//! loop, not a subprocess contract.
//!
//! What is REAL here: the gateway, the event, the runner binary, the `/mcp`
//! tool calls, the writes, the ledger. What stands in is the MODEL — a real
//! local HTTP server speaking Gemini's `generateContent` wire shape, pointed
//! at by `ESCUREL_RUNNER_GEMINI_BASE_URL`. That is the same technique the
//! `claude` adapter's deterministic test uses with a stub executable, and it
//! is the only part that can be substituted without making the test a
//! non-deterministic quota burn: the model's OUTPUT is what varies, and this
//! test is about what the adapter does with it.
//!
//! The stub is deliberately not a rubber stamp. It reads the request the
//! adapter actually built and refuses to proceed unless the escurel tool
//! schemas arrived as `functionDeclarations` — so a broken schema
//! sanitisation (the failure mode that would make real Gemini reject the
//! whole request with a 400) fails this test rather than being discovered in
//! production.
//!
//! The live variant against real Gemini is `gemini_live.rs` (`#[ignore]`).

use std::net::TcpListener;
use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const SKILL: &str = "customer";
const SKILL_BODY: &str = "---\ntype: skill\nid: customer\nautonomy: auto\n---\n# customer\n\n\
Fold the triggering event into the named customer instance.\n";
const INSTANCE_ID: &str = "globex";
const INSTANCE_BODY: &str =
    "---\ntype: instance\nid: globex\nskill: customer\n---\n# Globex\n\nBASELINE account state.\n";
const FOLDED_MARKER: &str = "GEMINI_FOLDED_MARKER";

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind ephemeral port")
        .local_addr()
        .expect("local_addr")
        .port()
}

async fn call_mcp(p: &EscurelProcess, name: &str, args: Value) -> Value {
    let token = p.mint_token(TENANT, Role::Agent);
    let resp = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": args },
        }))
        .send()
        .await
        .expect("post /mcp");
    let body: Value = resp.json().await.expect("json");
    assert!(body.get("error").is_none(), "tool {name} error: {body}");
    body["result"]
        .get("structuredContent")
        .cloned()
        .unwrap_or_else(|| body["result"].clone())
}

/// What the stub model saw, so the test can assert on the adapter's requests
/// after the run rather than only on their effects.
#[derive(Default)]
struct Seen {
    requests: Vec<Value>,
}

/// A real HTTP server speaking `generateContent`. Turn 1 asks for the two
/// tool calls that fold the event; turn 2 (after the adapter feeds the
/// results back) answers with text, which is how the adapter learns the run
/// is done.
async fn spawn_stub_model(instance_page_id: String, seen: Arc<Mutex<Seen>>) -> String {
    use axum::{Router, extract::State, http::Uri};

    #[derive(Clone)]
    struct St {
        instance_page_id: String,
        seen: Arc<Mutex<Seen>>,
    }

    async fn generate(State(st): State<St>, uri: Uri, body: String) -> axum::Json<Value> {
        // The URL shape is part of the contract with the real API: a wrong
        // path is a 404 there and would be invisible behind a permissive
        // stub. Asserted rather than routed, because axum cannot route a
        // segment that mixes a parameter with a literal (`{model}:generate…`).
        assert!(
            uri.path().starts_with("/models/") && uri.path().ends_with(":generateContent"),
            "unexpected model URL: {uri}"
        );
        let req: Value = serde_json::from_str(&body).expect("adapter sent JSON");
        let decls = req["tools"][0]["functionDeclarations"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        // The contract with the real API: declarations, with schemas, or a
        // 400 for the whole request. Asserted here so a sanitisation
        // regression fails the test instead of production.
        assert!(
            !decls.is_empty(),
            "the adapter must declare the packaged tools: {req}"
        );
        for d in &decls {
            assert!(d["name"].is_string(), "declaration without a name: {d}");
            assert_eq!(
                d["parameters"]["type"],
                json!("object"),
                "declaration parameters must be an object schema: {d}"
            );
            assert!(
                d["parameters"].get("additionalProperties").is_none()
                    && d["parameters"].get("$schema").is_none(),
                "schema keys Gemini rejects must not be declared: {d}"
            );
        }
        let is_first = req["contents"].as_array().map_or(0, Vec::len) == 1;
        if is_first {
            let input = req["contents"][0]["parts"][0]["text"]
                .as_str()
                .unwrap_or_default();
            assert!(
                input.contains("engagement-globex"),
                "the event's provenance must reach the model: {input}"
            );
        }
        st.seen.lock().expect("lock").requests.push(req);

        let names: Vec<&str> = decls.iter().filter_map(|d| d["name"].as_str()).collect();
        assert!(
            names.contains(&"update_page") && names.contains(&"assign_event"),
            "the packaged surface must let the agent write and assign: {names:?}"
        );

        let parts = if is_first {
            let event_id = event_id_from(&st.seen);
            json!([
                { "functionCall": { "name": "update_page", "args": {
                    "page_id": st.instance_page_id,
                    "content": format!(
                        "---\ntype: instance\nid: {INSTANCE_ID}\nskill: {SKILL}\n---\n\
                         # Globex\n\nBASELINE account state.\n\n{FOLDED_MARKER} {event_id}\n"
                    ),
                } } },
                { "functionCall": { "name": "assign_event", "args": {
                    "event_id": event_id,
                    "instance_page_id": st.instance_page_id,
                } } },
            ])
        } else {
            json!([{ "text": "Folded the event into the instance." }])
        };
        axum::Json(json!({ "candidates": [{ "content": { "parts": parts } }] }))
    }

    /// The event id the runner packaged, read out of the first request's
    /// input — the stub must act on the REAL event, not a constant, or the
    /// assign would pass against something the runner never dispatched.
    fn event_id_from(seen: &Arc<Mutex<Seen>>) -> String {
        let guard = seen.lock().expect("lock");
        let req = guard.requests.last().expect("a request was just pushed");
        let input = req["contents"][0]["parts"][0]["text"]
            .as_str()
            .unwrap_or_default();
        input
            .split_whitespace()
            .find(|w| w.starts_with("01") && w.len() == 26)
            .unwrap_or_default()
            .to_owned()
    }

    let app = Router::new().fallback(generate).with_state(St {
        instance_page_id,
        seen,
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stub model");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn gemini_adapter_folds_an_event_through_real_mcp_calls() {
    let gateway = EscurelProcess::spawn(Opts {
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
    .await;
    let instance_page_id = format!("markdown/instances/{SKILL}/{INSTANCE_ID}.md");

    let captured = call_mcp(
        &gateway,
        "capture_event",
        json!({
            "source": "manual",
            "mime": "text/plain",
            "label_skill": SKILL,
            "instance_page_id": instance_page_id,
            "title": "renewal request",
            "body": "customer wants to renew",
            // What an AUTHORED route already decided about this event. It is
            // the question the agent is about to be asked, answered — and
            // until the packager rendered provenance, the model never saw it.
            "provenance": { "engagement": "engagement-globex" },
        }),
    )
    .await;
    let event_id = captured["event_id"].as_str().expect("event_id").to_owned();

    let seen = Arc::new(Mutex::new(Seen::default()));
    let model_base = spawn_stub_model(instance_page_id.clone(), Arc::clone(&seen)).await;

    let token = gateway.mint_token(TENANT, Role::Agent);
    let listen = format!("127.0.0.1:{}", free_port());
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gateway.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_TOKEN", &token)
        .env("ESCUREL_RUNNER_HARNESS", "gemini")
        .env("ESCUREL_GEMINI_API_KEY", "test-key-not-a-real-credential")
        .env("ESCUREL_RUNNER_GEMINI_BASE_URL", &model_base)
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms");
    let _runner = ChildGuard(cmd.spawn().expect("spawn escurel-runner"));

    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let events = call_mcp(
            &gateway,
            "list_events",
            json!({ "instance_page_id": instance_page_id }),
        )
        .await;
        let processed = events["events"].as_array().is_some_and(|es| {
            es.iter()
                .any(|e| e["event_id"] == json!(event_id) && e["status"] == json!("processed"))
        });
        if processed {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the gemini adapter never folded {event_id} into {instance_page_id} within 60s"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    // The write is the point, not the status flip: the adapter must have made
    // the model's `update_page` call for real, under the scoped token.
    let expanded = call_mcp(&gateway, "expand", json!({ "page_id": instance_page_id })).await;
    let body = expanded["body"].as_str().unwrap_or_default();
    assert!(
        body.contains(FOLDED_MARKER),
        "the instance must carry the model's write: {body}"
    );
    assert!(
        body.contains("BASELINE account state"),
        "the fold must not have destroyed the existing body: {body}"
    );

    // Two turns: the tool calls, then the results fed back. A single-turn run
    // would mean the adapter never returned the tool results to the model —
    // which is the whole loop.
    let guard = seen.lock().expect("lock");
    assert!(
        guard.requests.len() >= 2,
        "the adapter must feed tool results back to the model, got {} request(s)",
        guard.requests.len()
    );
    let second = &guard.requests[1];
    let roles: Vec<&str> = second["contents"]
        .as_array()
        .expect("contents")
        .iter()
        .filter_map(|c| c["role"].as_str())
        .collect();
    assert_eq!(
        roles,
        vec!["user", "model", "user"],
        "the second turn must carry the model turn and the tool responses: {second}"
    );
    assert!(
        second["contents"][2]["parts"][0]["functionResponse"]["response"].is_object(),
        "the tool result must reach the model as a functionResponse: {second}"
    );
}
