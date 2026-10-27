//! A skill with no `autonomy:` key runs auto (BRD chat-agent Q2, owner decision
//! 2026-10-25: one default, the gateway's). The runner used to hold such a
//! skill's write as a draft while the gateway treated it as auto; now both land
//! it. An unrecognised value still holds for review (a typo must not commit).
//!
//! Real gateway, real runner binary, real echo harness.

use std::process::{Child, Command};
use std::time::{Duration, Instant};

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role, free_port};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const PAGE: &str = "markdown/instances/renewal/c1.md";
const INSTANCE: &str = "---\nkind: instance\nid: c1\nskill: renewal\n---\n# C1\n\nBASELINE.\n";

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn call(p: &EscurelProcess, name: &str, args: Value) -> Value {
    let token = p.mint_token(TENANT, Role::Admin);
    let body: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": { "name": name, "arguments": args } }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json");
    assert!(body.get("error").is_none(), "{name}: {body}");
    body["result"]["structuredContent"].clone()
}

/// Run one event through a skill declared with `autonomy_line`; returns the
/// page body afterwards and how many drafts are open.
async fn run_with(autonomy_line: &str) -> (String, usize) {
    let skill = format!(
        "---\nkind: skill\nid: renewal\n{autonomy_line}---\n# renewal\n\nFold the event.\n"
    );
    let gw = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("renewal", skill)
                .instance("renewal", "c1", INSTANCE)
                .done(),
        ),
        ..Default::default()
    })
    .await;
    let event_id = call(
        &gw,
        "capture_event",
        json!({ "source": "manual", "label_skill": "renewal", "instance_page_id": PAGE,
                "title": "renewal request", "body": "customer wants to renew" }),
    )
    .await["event_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let listen = format!("127.0.0.1:{}", free_port());
    let ledger = tempfile::tempdir().expect("tempdir");
    let _runner = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_escurel-runner"))
            .env("ESCUREL_RUNNER_LISTEN", &listen)
            .env("ESCUREL_RUNNER_GATEWAY_URL", gw.base_url())
            .env("ESCUREL_RUNNER_TENANT", TENANT)
            .env("ESCUREL_RUNNER_TOKEN", gw.mint_token(TENANT, Role::Admin))
            .env("ESCUREL_RUNNER_HARNESS", "echo")
            .env(
                "ESCUREL_RUNNER_LEDGER_PATH",
                ledger.path().join("ledger.duckdb"),
            )
            .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms")
            .spawn()
            .expect("spawn runner"),
    );
    let url = format!("http://{listen}/debug/run?tenant={TENANT}&event_id={event_id}");
    let deadline = Instant::now() + Duration::from_secs(40);
    loop {
        if let Ok(resp) = reqwest::get(&url).await
            && let Ok(v) = resp.json::<Value>().await
            && v["status"].as_str().is_some_and(|s| s != "pending")
        {
            assert_eq!(v["status"], "processed", "{v}");
            break;
        }
        assert!(Instant::now() < deadline, "run never reached a terminal");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let body = call(&gw, "expand", json!({ "page_id": PAGE })).await["body"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let drafts = call(&gw, "list_drafts", json!({})).await["drafts"]
        .as_array()
        .map_or(0, Vec::len);
    (body, drafts)
}

#[tokio::test]
async fn a_skill_without_autonomy_lands_its_write_like_the_gateway_says() {
    let (body, drafts) = run_with("").await;
    assert!(body.contains("folded event"), "the write landed: {body}");
    assert_eq!(drafts, 0, "nothing held for review");
}

#[tokio::test]
async fn an_unrecognised_autonomy_still_holds_for_review() {
    let (body, drafts) = run_with("autonomy: atuo\n").await;
    assert!(
        !body.contains("folded event"),
        "a typo must not commit: {body}"
    );
    assert_eq!(drafts, 1);
}
