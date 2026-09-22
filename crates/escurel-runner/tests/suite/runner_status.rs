//! The runner reports its own health as `escurel:runner-status` system
//! events (knowledge-workbench backend P2-4 — BRD FR-C runner status): a
//! heartbeat every `ESCUREL_RUNNER_STATUS_INTERVAL`, and one at once when
//! what it reports changes (a run starts or ends, a tenant pauses). The
//! workbench reads the latest with `list_events { label_skill,
//! newest_first: true, limit: 1 }`.
//!
//! Real gateway, real runner binary, real echo harness idling so a run is
//! observably live.

use std::process::{Child, Command};
use std::time::{Duration, Instant};

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role, free_port};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const SKILL_BODY: &str = "---\ntype: skill\nid: renewal\nautonomy: auto\n---\n# renewal\n\nFold the event into the instance.\n";
const INSTANCE_BODY: &str = "---\ntype: instance\nid: c1\nskill: renewal\n---\n# C1\n\nBASELINE.\n";
const PAGE: &str = "markdown/instances/renewal/c1.md";

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn call(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
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

/// The latest status row and its parsed body.
async fn latest(p: &EscurelProcess, token: &str) -> Option<(Value, Value)> {
    let r = call(
        p,
        token,
        "list_events",
        json!({ "label_skill": "escurel:runner-status", "newest_first": true, "limit": 1 }),
    )
    .await;
    let e = r["events"].as_array()?.first()?.clone();
    let body: Value = serde_json::from_str(e["body"].as_str()?).ok()?;
    Some((e, body))
}

async fn wait_for(
    p: &EscurelProcess,
    token: &str,
    what: &str,
    pred: impl Fn(&Value, &Value) -> bool,
) -> (Value, Value) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some((e, body)) = latest(p, token).await
            && pred(&e, &body)
        {
            return (e, body);
        }
        assert!(Instant::now() < deadline, "no status where {what}");
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

#[tokio::test]
async fn the_runner_heartbeats_and_reports_a_change_at_once() {
    let gw = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("renewal", SKILL_BODY)
                .instance("renewal", "c1", INSTANCE_BODY)
                .done(),
        ),
        ..Default::default()
    })
    .await;
    let admin = gw.mint_token(TENANT, Role::Admin);

    let listen = format!("127.0.0.1:{}", free_port());
    let ledger_dir = tempfile::tempdir().expect("tempdir");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gw.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_TOKEN", &admin)
        .env("ESCUREL_RUNNER_HARNESS", "echo")
        .env("ESCUREL_RUNNER_ID", "runner-a")
        .env(
            "ESCUREL_RUNNER_LEDGER_PATH",
            ledger_dir.keep().join("ledger.sqlite"),
        )
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms")
        .env("ESCUREL_RUNNER_STATUS_INTERVAL", "2s")
        .env("ESCUREL_ECHO_SLEEP_MS", "3000");
    let _runner = ChildGuard(cmd.spawn().expect("spawn runner"));

    // Boot: the first status says who and what, idle.
    let (e, body) = wait_for(&gw, &admin, "the runner has booted", |_, _| true).await;
    assert_eq!(e["kind"], "system", "{e}");
    assert_eq!(
        e["status"], "inbox",
        "unassigned bookkeeping, hidden from the inbox: {e}"
    );
    assert_eq!(body["runner_id"], "runner-a", "{body}");
    assert_eq!(body["harness"], "echo", "{body}");
    assert_eq!(body["tenant"], TENANT, "{body}");
    assert!(body["version"].is_string(), "{body}");
    assert_eq!(body["live_runs"], json!([]), "{body}");
    assert_eq!(body["paused_tenants"], json!([]), "{body}");
    assert_eq!(body["runs"]["dead_letter"], 0, "{body}");
    assert!(body["uptime_s"].is_number(), "{body}");
    let inbox = call(&gw, &admin, "list_inbox", json!({})).await;
    assert!(inbox["events"].as_array().unwrap().is_empty(), "{inbox}");

    // A run starts: a `changed` status names it as live, before any
    // scheduled heartbeat could.
    let r = call(
        &gw,
        &admin,
        "capture_event",
        json!({ "source": "manual", "mime": "text/plain", "label_skill": "renewal",
                "instance_page_id": PAGE, "title": "renew", "body": "please renew" }),
    )
    .await;
    let event_id = r["event_id"].as_str().unwrap().to_owned();
    let (e, body) = wait_for(&gw, &admin, "a run is live", |_, b| {
        b["live_runs"].as_array().is_some_and(|a| a.len() == 1)
    })
    .await;
    assert_eq!(e["title"], "changed", "{e}");
    assert_eq!(body["live_runs"][0]["event_id"], event_id, "{body}");
    assert!(body["live_runs"][0]["run_id"].is_string(), "{body}");
    assert!(
        body["last_poll_age_ms"]
            .as_u64()
            .is_some_and(|ms| ms < 5000),
        "{body}"
    );

    // …and ends: idle again, with the run counted.
    let (_, body) = wait_for(&gw, &admin, "the run has ended", |_, b| {
        b["live_runs"].as_array().is_some_and(Vec::is_empty) && b["runs"]["processed"] == 1
    })
    .await;
    assert_eq!(body["runs"]["pending"], 0, "{body}");

    // A change of policy is reported at once too.
    call(
        &gw,
        &admin,
        "capture_event",
        json!({ "source": "workbench", "mime": "application/json",
                "label_skill": "escurel:run-control", "title": "pause",
                "body": json!({ "action": "pause" }).to_string() }),
    )
    .await;
    let (e, _) = wait_for(&gw, &admin, "the tenant is paused", |_, b| {
        b["paused_tenants"] == json!([TENANT])
    })
    .await;
    assert_eq!(e["title"], "changed", "{e}");

    // Heartbeats keep coming while nothing changes.
    let before = call(
        &gw,
        &admin,
        "list_events",
        json!({ "label_skill": "escurel:runner-status", "limit": 1000 }),
    )
    .await["events"]
        .as_array()
        .unwrap()
        .len();
    tokio::time::sleep(Duration::from_millis(4500)).await;
    let after = call(
        &gw,
        &admin,
        "list_events",
        json!({ "label_skill": "escurel:runner-status", "limit": 1000 }),
    )
    .await;
    let rows = after["events"].as_array().unwrap();
    assert!(
        rows.len() >= before + 2,
        "heartbeats: {before} → {}",
        rows.len()
    );
    assert!(
        rows.iter().rev().take(2).all(|e| e["title"] == "heartbeat"),
        "{after}"
    );
}
