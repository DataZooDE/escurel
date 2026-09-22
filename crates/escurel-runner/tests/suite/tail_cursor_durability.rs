//! The runner's label tails keep their cursor in the run ledger (hardening
//! H2): a request filed while the runner was down is acted on after a
//! restart — within `ESCUREL_RUNNER_TAIL_MAX_AGE` — and one older than that
//! is skipped, not replayed. Before H2 a restart caught up to the end of
//! the label and acted on nothing older than itself.
//!
//! Real gateway, real runner binaries sharing one ledger file, real echo.

use std::process::{Child, Command};
use std::time::{Duration, Instant};

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role, free_port};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const SKILL_BODY: &str = "---\ntype: skill\nid: renewal\nautonomy: auto\n---\n# renewal\n";

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

async fn control(p: &EscurelProcess, token: &str, action: &str) -> String {
    let r = call(
        p,
        token,
        "capture_event",
        json!({ "source": "workbench", "mime": "application/json",
                "label_skill": "escurel:run-control", "title": action,
                "body": json!({ "action": action }).to_string() }),
    )
    .await;
    r["event_id"].as_str().unwrap().to_owned()
}

async fn results(p: &EscurelProcess, token: &str) -> Vec<String> {
    let r = call(
        p,
        token,
        "list_events",
        json!({ "label_skill": "escurel:run-control-result" }),
    )
    .await;
    r["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            e["provenance"]["control"]["request_event_id"]
                .as_str()
                .unwrap_or("")
                .to_owned()
        })
        .collect()
}

fn spawn_runner(
    gw: &EscurelProcess,
    admin: &str,
    ledger: &std::path::Path,
) -> (ChildGuard, String) {
    let listen = format!("127.0.0.1:{}", free_port());
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gw.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_TOKEN", admin)
        .env("ESCUREL_RUNNER_HARNESS", "echo")
        .env("ESCUREL_RUNNER_LEDGER_PATH", ledger)
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms")
        .env("ESCUREL_RUNNER_TAIL_MAX_AGE", "3s");
    (ChildGuard(cmd.spawn().expect("spawn runner")), listen)
}

async fn wait_healthy(listen: &str) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if reqwest::get(format!("http://{listen}/healthz"))
            .await
            .is_ok_and(|r| r.status().is_success())
        {
            return;
        }
        assert!(Instant::now() < deadline, "runner never healthy");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn wait_for_answer(p: &EscurelProcess, token: &str, request: &str) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if results(p, token).await.iter().any(|r| r == request) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "request {request} never answered"
        );
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

#[tokio::test]
async fn a_request_filed_while_the_runner_was_down_is_acted_on_after_a_restart_within_max_age() {
    let gw = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("renewal", SKILL_BODY)
                .done(),
        ),
        ..Default::default()
    })
    .await;
    let admin = gw.mint_token(TENANT, Role::Admin);
    let ledger_dir = tempfile::tempdir().expect("tempdir");
    let ledger = ledger_dir.keep().join("ledger.duckdb");

    // Runner A boots and answers a first request (so a cursor exists), then
    // goes away.
    let (a, listen_a) = spawn_runner(&gw, &admin, &ledger);
    wait_healthy(&listen_a).await;
    let first = control(&gw, &admin, "pause").await;
    wait_for_answer(&gw, &admin, &first).await;
    drop(a);
    tokio::time::sleep(Duration::from_millis(300)).await;

    // While no runner listens: one request that will be OLDER than the
    // max age by the time runner B boots, and one that is fresh.
    let stale = control(&gw, &admin, "resume").await;
    tokio::time::sleep(Duration::from_millis(3500)).await;
    let fresh = control(&gw, &admin, "pause").await;

    let (_b, listen_b) = spawn_runner(&gw, &admin, &ledger);
    wait_healthy(&listen_b).await;
    wait_for_answer(&gw, &admin, &fresh).await;
    let answered = results(&gw, &admin).await;
    assert!(answered.iter().any(|r| r == &first), "{answered:?}");
    assert!(
        !answered.iter().any(|r| r == &stale),
        "a request older than the max age is skipped, not replayed: {answered:?}"
    );
    // …and skipping it advanced the cursor: nothing is re-read on the next poll.
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(results(&gw, &admin).await.len(), 2, "exactly first + fresh");
}
