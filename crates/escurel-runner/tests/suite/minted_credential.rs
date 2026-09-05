//! The runner mints its own gateway bearer — end to end, no pasted token.
//!
//! `ESCUREL_RUNNER_TOKEN` is a bearer somebody minted by hand and pasted into
//! a secret, and it expires silently: a runner whose token has lapsed still
//! answers `/healthz`, still polls, and simply stops being able to read the
//! inbox — which looks exactly like an empty inbox. Every other workload on
//! this substrate stopped holding static bearers for that reason.
//!
//! This test runs the real runner with **no `ESCUREL_RUNNER_TOKEN` at all**,
//! against a real gateway doing real signature verification, and asserts the
//! whole loop still works. The gateway checks the token against the JWKS its
//! issuer publishes; nothing here is stubbed but the model.
//!
//! Two ways this could pass for the wrong reason, both closed below: the
//! runner could be falling back to some other credential (there is none — the
//! env var is absent), and the gateway could be accepting anything (a
//! deliberately WRONG key is refused, in the same test).

use std::net::TcpListener;
use std::process::{Child, Command};
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
const MARKER: &str = "MINTED_CREDENTIAL_FOLD";

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind")
        .local_addr()
        .expect("addr")
        .port()
}

async fn call_mcp(p: &EscurelProcess, name: &str, args: Value) -> Value {
    let token = p.mint_token(TENANT, Role::Agent);
    let body: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": args },
        }))
        .send()
        .await
        .expect("post /mcp")
        .json()
        .await
        .expect("json");
    assert!(body.get("error").is_none(), "tool {name} error: {body}");
    body["result"]
        .get("structuredContent")
        .cloned()
        .unwrap_or_else(|| body["result"].clone())
}

/// A model that folds the event, so the run reaches a confirmed effect. Every
/// `/mcp` call it causes is made with the token the runner MINTED.
async fn spawn_stub_model(page_id: String) -> String {
    use axum::{Router, extract::State};

    async fn generate(State(page_id): State<String>, body: String) -> axum::Json<Value> {
        let req: Value = serde_json::from_str(&body).expect("json");
        let turn = req["contents"].as_array().map_or(0, Vec::len).div_ceil(2);
        let input = req["contents"][0]["parts"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let event_id = input
            .split_whitespace()
            .find(|w| w.starts_with("01") && w.len() == 26)
            .unwrap_or_default()
            .to_owned();
        let parts = if turn == 1 {
            json!([
                { "functionCall": { "name": "update_page", "args": {
                    "page_id": page_id,
                    "content": format!(
                        "---\ntype: instance\nid: {INSTANCE_ID}\nskill: {SKILL}\n---\n\
                         # Globex\n\nBASELINE account state.\n\n{MARKER} {event_id}\n"
                    ),
                } } },
                { "functionCall": { "name": "assign_event", "args": {
                    "event_id": event_id, "instance_page_id": page_id,
                } } },
            ])
        } else {
            json!([{ "text": "done" }])
        };
        axum::Json(json!({ "candidates": [{ "content": { "parts": parts } }] }))
    }

    let app = Router::new().fallback(generate).with_state(page_id);
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
async fn the_runner_mints_its_own_bearer_and_the_gateway_accepts_it() {
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
    let page_id = format!("markdown/instances/{SKILL}/{INSTANCE_ID}.md");

    let captured = call_mcp(
        &gateway,
        "capture_event",
        json!({
            "source": "manual",
            "mime": "text/plain",
            "label_skill": SKILL,
            "instance_page_id": page_id,
            "title": "renewal",
            "body": "they want to renew",
        }),
    )
    .await;
    let event_id = captured["event_id"].as_str().expect("event_id").to_owned();

    let model_base = spawn_stub_model(page_id.clone()).await;
    let (signing_key, kid) = gateway.signing_material();
    let issuer = gateway.issuer_url();

    let listen = format!("127.0.0.1:{}", free_port());
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gateway.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        // The point of the test: NO ESCUREL_RUNNER_TOKEN.
        .env_remove("ESCUREL_RUNNER_TOKEN")
        .env("ESCUREL_RUNNER_AUTH_ISSUER", &issuer)
        .env("ESCUREL_RUNNER_AUTH_KID", kid)
        .env("ESCUREL_RUNNER_AUTH_SIGNING_KEY", &signing_key)
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
            json!({ "instance_page_id": page_id }),
        )
        .await;
        if events["events"].as_array().is_some_and(|es| {
            es.iter()
                .any(|e| e["event_id"] == json!(event_id) && e["status"] == json!("processed"))
        }) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "a runner holding NO static token never folded {event_id} — its \
             minted bearer was not accepted by the gateway"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    let expanded = call_mcp(&gateway, "expand", json!({ "page_id": page_id })).await;
    assert!(
        expanded["body"]
            .as_str()
            .unwrap_or_default()
            .contains(MARKER),
        "the write made under the minted bearer must have landed"
    );
}

/// The control: the gateway is really verifying.
///
/// Same runner, same wiring, a DIFFERENT key. If this folded the event too,
/// the test above would prove nothing — it would mean any signature passes.
#[tokio::test]
async fn a_bearer_minted_with_the_wrong_key_is_refused() {
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
    let page_id = format!("markdown/instances/{SKILL}/{INSTANCE_ID}.md");

    let captured = call_mcp(
        &gateway,
        "capture_event",
        json!({
            "source": "manual",
            "mime": "text/plain",
            "label_skill": SKILL,
            "instance_page_id": page_id,
            "title": "renewal",
            "body": "they want to renew",
        }),
    )
    .await;
    let event_id = captured["event_id"].as_str().expect("event_id").to_owned();

    let model_base = spawn_stub_model(page_id.clone()).await;
    let (_, kid) = gateway.signing_material();
    let issuer = gateway.issuer_url();
    // A real, valid, WRONG key: 2048-bit RSA the gateway's JWKS never saw.
    let wrong_key = {
        use rsa::pkcs1::EncodeRsaPrivateKey;
        let mut rng = rand::thread_rng();
        rsa::RsaPrivateKey::new(&mut rng, 2048)
            .expect("keygen")
            .to_pkcs1_pem(rsa::pkcs1::LineEnding::LF)
            .expect("pem")
            .to_string()
    };

    let listen = format!("127.0.0.1:{}", free_port());
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gateway.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env_remove("ESCUREL_RUNNER_TOKEN")
        .env("ESCUREL_RUNNER_AUTH_ISSUER", &issuer)
        .env("ESCUREL_RUNNER_AUTH_KID", kid)
        .env("ESCUREL_RUNNER_AUTH_SIGNING_KEY", &wrong_key)
        .env("ESCUREL_RUNNER_HARNESS", "gemini")
        .env("ESCUREL_GEMINI_API_KEY", "test-key-not-a-real-credential")
        .env("ESCUREL_RUNNER_GEMINI_BASE_URL", &model_base)
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms");
    let _runner = ChildGuard(cmd.spawn().expect("spawn escurel-runner"));

    // Long enough that the passing case above would have finished several
    // times over.
    tokio::time::sleep(Duration::from_secs(10)).await;

    let events = call_mcp(
        &gateway,
        "list_events",
        json!({ "instance_page_id": page_id }),
    )
    .await;
    assert!(
        !events["events"].as_array().is_some_and(|es| {
            es.iter()
                .any(|e| e["event_id"] == json!(event_id) && e["status"] == json!("processed"))
        }),
        "a bearer signed with a key the gateway never published must be \
         refused — if this folded, the gateway is not verifying at all"
    );
    let expanded = call_mcp(&gateway, "expand", json!({ "page_id": page_id })).await;
    assert!(
        !expanded["body"]
            .as_str()
            .unwrap_or_default()
            .contains(MARKER),
        "nothing may be written under an unverifiable bearer"
    );
}
