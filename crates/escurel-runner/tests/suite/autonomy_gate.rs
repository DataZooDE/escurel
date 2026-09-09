//! `autonomy:` becomes behaviour — the review gate, end to end.
//!
//! escurel has published a skill's `autonomy: auto | review | confirm` on
//! `list_skills` since #360 and enforced nothing; the runner committed
//! regardless. These tests pin the enforcement:
//!
//! - a skill declaring `review` produces a DRAFT and writes nothing;
//! - a skill declaring `auto` writes, which is the positive control that
//!   makes the first assertion about the DECLARATION and not about the
//!   pipeline being broken;
//! - an unrecognised `autonomy:` behaves as `review`. A typo that silently
//!   meant "commit without a gate" is the one direction this must not fail
//!   in.
//!
//! Real gateway, real runner binary, real `/mcp` writes and drafts. The
//! model is a real local HTTP server speaking Gemini's wire shape — and it
//! is deliberately a BADLY BEHAVED one: it always tries `update_page`
//! first, whatever the instructions say. A gate that only works when the
//! agent cooperates is not a gate, and prompt text is not a control.

use std::net::TcpListener;
use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const INSTANCE_ID: &str = "globex";
const WRITTEN_MARKER: &str = "AGENT_WROTE_THIS";

fn skill_body(id: &str, autonomy: Option<&str>) -> String {
    let line = autonomy
        .map(|a| format!("autonomy: {a}\n"))
        .unwrap_or_default();
    format!("---\ntype: skill\nid: {id}\n{line}---\n# {id}\n\nFold the event in.\n")
}

fn instance_body(skill: &str) -> String {
    format!("---\ntype: instance\nid: {INSTANCE_ID}\nskill: {skill}\n---\n# Globex\n\nBASELINE.\n")
}

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

/// The tool names the adapter declared, per request.
#[derive(Default)]
struct Seen {
    declared: Vec<Vec<String>>,
}

/// A model that always reaches for `update_page` first and falls back to
/// `create_draft` only when it is refused — the adversarial case. It also
/// records which tools it was OFFERED, which is what the narrowing test
/// asserts on.
async fn spawn_stub_model(page_id: String, seen: Arc<Mutex<Seen>>) -> String {
    use axum::{Router, extract::State};

    #[derive(Clone)]
    struct St {
        page_id: String,
        seen: Arc<Mutex<Seen>>,
    }

    async fn generate(State(st): State<St>, body: String) -> axum::Json<Value> {
        let req: Value = serde_json::from_str(&body).expect("adapter sent JSON");
        let declared: Vec<String> = req["tools"][0]["functionDeclarations"]
            .as_array()
            .map(|ds| {
                ds.iter()
                    .filter_map(|d| d["name"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        // `contents` grows by TWO per completed turn (the model turn plus
        // the tool responses), so the first request has 1 and the second 3.
        let turn = req["contents"].as_array().map_or(0, Vec::len).div_ceil(2);
        let input = req["contents"][0]["parts"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        st.seen
            .lock()
            .expect("lock")
            .declared
            .push(declared.clone());

        let event_id = input
            .split_whitespace()
            .find(|w| w.starts_with("01") && w.len() == 26)
            .unwrap_or_default()
            .to_owned();
        let content = format!(
            "---\ntype: instance\nid: {INSTANCE_ID}\nskill: {}\n---\n# Globex\n\nBASELINE.\n\n\
             {WRITTEN_MARKER} {event_id}\n",
            st.page_id.split('/').nth(2).unwrap_or_default()
        );

        let parts = match turn {
            // Turn 1: try to commit, always.
            1 => json!([
                { "functionCall": { "name": "update_page", "args": {
                    "page_id": st.page_id, "content": content,
                } } },
                { "functionCall": { "name": "assign_event", "args": {
                    "event_id": event_id, "instance_page_id": st.page_id,
                } } },
            ]),
            // Turn 2: whatever came back, draft it. Under `auto` the write
            // already landed and this is a harmless second proposal; under
            // `review` it is the only thing that CAN happen.
            2 => json!([
                { "functionCall": { "name": "create_draft", "args": {
                    "target_page_id": st.page_id,
                    "content": content,
                    "event_id": event_id,
                } } },
            ]),
            _ => json!([{ "text": "done" }]),
        };
        axum::Json(json!({ "candidates": [{ "content": { "parts": parts } }] }))
    }

    let app = Router::new()
        .fallback(generate)
        .with_state(St { page_id, seen });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stub model");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

struct Run {
    gateway: EscurelProcess,
    page_id: String,
    event_id: String,
    seen: Arc<Mutex<Seen>>,
    _runner: ChildGuard,
    /// Held for the run's lifetime on purpose: a `TempDir` deletes its
    /// directory when dropped, and dropping it at the end of `start` took the
    /// runner's ledger with it before the runner had finished opening it.
    _ledger_dir: tempfile::TempDir,
}

/// Seed a tenant whose one skill declares `autonomy: <declaration>`, capture
/// an event for it, and start the real runner against the stub model.
async fn start(skill: &str, declaration: Option<&str>) -> Run {
    let gateway = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill(skill, skill_body(skill, declaration))
                .instance(skill, INSTANCE_ID, instance_body(skill))
                .done(),
        ),
        ..Default::default()
    })
    .await;
    let page_id = format!("markdown/instances/{skill}/{INSTANCE_ID}.md");

    let captured = call_mcp(
        &gateway,
        "capture_event",
        json!({
            "source": "manual",
            "mime": "text/plain",
            "label_skill": skill,
            "instance_page_id": page_id,
            "title": "renewal",
            "body": "they want to renew",
        }),
    )
    .await;
    let event_id = captured["event_id"].as_str().expect("event_id").to_owned();

    let seen = Arc::new(Mutex::new(Seen::default()));
    let model_base = spawn_stub_model(page_id.clone(), Arc::clone(&seen)).await;

    let token = gateway.mint_token(TENANT, Role::Agent);
    let listen = format!("127.0.0.1:{}", free_port());
    // Its OWN ledger. Without this the runner falls back to
    // `./escurel-runner-ledger.sqlite` in the crate directory — one file
    // shared by every test in the suite AND by every previous run of it. A
    // row another test left behind is a row this one inherits: the content
    // dedup saw its fixture already folded in and correctly declined to run
    // it again, which is right behaviour reading wrong state.
    let ledger_dir = tempfile::tempdir().expect("tempdir for ledger");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gateway.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_TOKEN", &token)
        .env("ESCUREL_RUNNER_HARNESS", "gemini")
        .env(
            "ESCUREL_RUNNER_LEDGER_PATH",
            ledger_dir.path().join("ledger.sqlite").to_str().unwrap(),
        )
        .env("ESCUREL_GEMINI_API_KEY", "test-key-not-a-real-credential")
        .env("ESCUREL_RUNNER_GEMINI_BASE_URL", &model_base)
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms");
    let runner = ChildGuard(cmd.spawn().expect("spawn escurel-runner"));

    Run {
        gateway,
        page_id,
        event_id,
        seen,
        _runner: runner,
        _ledger_dir: ledger_dir,
    }
}

/// Poll until `f` holds, or fail with `what`.
async fn until<F, Fut>(what: &str, secs: u64, mut f: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if f().await {
            return;
        }
        assert!(Instant::now() < deadline, "{what} within {secs}s");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn page_body(r: &Run) -> String {
    call_mcp(&r.gateway, "expand", json!({ "page_id": r.page_id })).await["body"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

#[tokio::test]
async fn a_review_skill_produces_a_draft_and_writes_nothing() {
    let r = start("customer_review", Some("review")).await;

    until("a draft appears for the event", 60, || async {
        let drafts = call_mcp(&r.gateway, "list_drafts", json!({})).await;
        drafts["drafts"]
            .as_array()
            .is_some_and(|ds| ds.iter().any(|d| d["event_id"] == json!(r.event_id)))
    })
    .await;

    // Nothing landed, and the model TRIED to land it: the surface is the
    // gate, not the prompt.
    let body = page_body(&r).await;
    assert!(
        !body.contains(WRITTEN_MARKER),
        "a review run must not write the page: {body}"
    );
    let declared = r.seen.lock().expect("lock").declared[0].clone();
    assert!(
        !declared.contains(&"update_page".to_owned())
            && !declared.contains(&"assign_event".to_owned()),
        "a review run must not be offered the committing tools: {declared:?}"
    );
    assert!(
        declared.contains(&"create_draft".to_owned()),
        "a review run must be offered the draft tool: {declared:?}"
    );

    // The event stays in the inbox — deliberately. Marking it processed
    // would say the knowledge base absorbed something it has not.
    let inbox = call_mcp(&r.gateway, "list_inbox", json!({})).await;
    assert!(
        inbox["events"]
            .as_array()
            .is_some_and(|es| es.iter().any(|e| e["event_id"] == json!(r.event_id))),
        "the event must stay in the inbox until the draft is decided: {inbox}"
    );
}

/// The positive control for the test above: the SAME pipeline, the same
/// stub model, one word different in the skill page.
#[tokio::test]
async fn an_auto_skill_still_commits() {
    let r = start("customer_auto", Some("auto")).await;

    until("the event is processed", 60, || async {
        let events = call_mcp(
            &r.gateway,
            "list_events",
            json!({ "instance_page_id": r.page_id }),
        )
        .await;
        events["events"].as_array().is_some_and(|es| {
            es.iter()
                .any(|e| e["event_id"] == json!(r.event_id) && e["status"] == json!("processed"))
        })
    })
    .await;

    let body = page_body(&r).await;
    assert!(
        body.contains(WRITTEN_MARKER),
        "an auto run must write the page: {body}"
    );
    let declared = r.seen.lock().expect("lock").declared[0].clone();
    assert!(
        declared.contains(&"update_page".to_owned()),
        "an auto run must be offered the committing tools: {declared:?}"
    );
}

/// A typo must not buy unattended writes.
#[tokio::test]
async fn an_unrecognised_autonomy_behaves_as_review() {
    let r = start("customer_typo", Some("atuo")).await;

    until("a draft appears for the event", 60, || async {
        let drafts = call_mcp(&r.gateway, "list_drafts", json!({})).await;
        drafts["drafts"]
            .as_array()
            .is_some_and(|ds| ds.iter().any(|d| d["event_id"] == json!(r.event_id)))
    })
    .await;

    let body = page_body(&r).await;
    assert!(
        !body.contains(WRITTEN_MARKER),
        "`autonomy: atuo` must not commit: {body}"
    );
}

/// A skill that declares nothing is not thereby unattended.
#[tokio::test]
async fn an_absent_autonomy_behaves_as_review() {
    let r = start("customer_silent", None).await;

    until("a draft appears for the event", 60, || async {
        let drafts = call_mcp(&r.gateway, "list_drafts", json!({})).await;
        drafts["drafts"]
            .as_array()
            .is_some_and(|ds| ds.iter().any(|d| d["event_id"] == json!(r.event_id)))
    })
    .await;

    let body = page_body(&r).await;
    assert!(
        !body.contains(WRITTEN_MARKER),
        "a skill with no `autonomy:` must not commit: {body}"
    );
}
