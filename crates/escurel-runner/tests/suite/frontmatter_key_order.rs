//! Key order of a page's frontmatter survives an echo-harness fold, however
//! the binaries were built.
//!
//! The gateway's `expand` hands the harness the frontmatter as a JSON object
//! and the harness re-renders it. Without `serde_json/preserve_order` in the
//! crates that parse and serialise that object, a binary built on its own
//! (`cargo build -p escurel-runner`, a release image) sorts the keys
//! alphabetically: a page written as `kind, skill, id, name` came back as
//! `id, kind, name, skill`. A rewrite that only appended to the body then
//! touched every frontmatter line, and a later three-way merge against a
//! concurrent edit of one of those lines was refused.
//!
//! This test runs the real gateway, the real runner and the real echo-harness
//! subprocess, and reads the stored bytes back with `expand { raw: true }`.

use std::process::{Child, Command};
use std::time::{Duration, Instant};

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role, free_port};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const SKILL: &str = "customer";
const SKILL_BODY: &str = "---\nkind: skill\nid: customer\nautonomy: auto\n---\n# customer\n\nFold the event into a customer instance.\n";
const INSTANCE_ID: &str = "initech";
/// Deliberately not alphabetical.
const FRONTMATTER: &str = "---\nkind: instance\nskill: customer\nid: initech\nname: Initech\n---\n";
const INSTANCE_BODY_TAIL: &str = "# Initech\n\nBASELINE account state.\n";

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn call_mcp(p: &EscurelProcess, name: &str, args: Value) -> Value {
    let token = p.mint_token(TENANT, Role::Agent);
    let body: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": name, "arguments": args },
        }))
        .send()
        .await
        .expect("post /mcp")
        .json()
        .await
        .expect("json");
    assert!(body.get("error").is_none(), "tool {name} error: {body}");
    let result = body["result"].clone();
    result.get("structuredContent").cloned().unwrap_or(result)
}

#[tokio::test]
async fn echo_fold_keeps_the_frontmatter_key_order_of_the_page() {
    let gateway = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill(SKILL, SKILL_BODY)
                .instance(
                    SKILL,
                    INSTANCE_ID,
                    format!("{FRONTMATTER}{INSTANCE_BODY_TAIL}"),
                )
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
            "title": "renewal request",
            "body": "KEY_ORDER_MARKER customer wants to renew"
        }),
    )
    .await;
    let event_id = captured["event_id"].as_str().expect("event_id").to_owned();

    let token = gateway.mint_token(TENANT, Role::Agent);
    let listen = format!("127.0.0.1:{}", free_port());
    let ledger_dir = tempfile::tempdir().expect("tempdir for ledger");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gateway.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_TOKEN", &token)
        .env("ESCUREL_RUNNER_HARNESS", "echo")
        .env(
            "ESCUREL_RUNNER_LEDGER_PATH",
            ledger_dir.path().join("ledger.sqlite").to_str().unwrap(),
        )
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms");
    let _runner = ChildGuard(cmd.spawn().expect("spawn escurel-runner"));

    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let events = call_mcp(
            &gateway,
            "list_events",
            json!({ "instance_page_id": page_id }),
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
            "echo harness never folded {event_id} into {page_id} within 30s"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let raw = call_mcp(
        &gateway,
        "expand",
        json!({ "page_id": page_id, "raw": true }),
    )
    .await;
    let content = raw["content"].as_str().expect("raw content").to_owned();
    assert!(
        content.contains(&event_id),
        "the fold must have rewritten the page: {content}"
    );
    assert!(
        content.starts_with(FRONTMATTER),
        "the rewrite must keep the frontmatter keys in the order the page wrote them \
         (kind, skill, id, name), got:\n{content}"
    );
}
