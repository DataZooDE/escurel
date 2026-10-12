//! PR-10: the runner publishes its harness allow-list in `escurel:runner-status`.
//!
//! The extension cannot offer a picker of valid harnesses until the status
//! body carries `harness_allow` next to the existing default `harness`.
//! Real gateway, real runner binary; the allow-list is configured unsorted
//! on purpose so the test proves the runner sorts it.

use std::process::{Child, Command};
use std::time::{Duration, Instant};

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role, free_port};
use serde_json::{Value, json};

const TENANT: &str = "acme";

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

async fn latest_status(p: &EscurelProcess, token: &str) -> Option<Value> {
    let r = call(
        p,
        token,
        "list_events",
        json!({ "label_skill": "escurel:runner-status", "newest_first": true, "limit": 1 }),
    )
    .await;
    let e = r["events"].as_array()?.first()?.clone();
    serde_json::from_str(e["body"].as_str()?).ok()
}

async fn wait_for_status(p: &EscurelProcess, token: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(body) = latest_status(p, token).await {
            return body;
        }
        assert!(Instant::now() < deadline, "no runner-status row appeared");
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

fn spawn_runner(
    gw: &EscurelProcess,
    token: &str,
    extra_env: &[(&str, &str)],
    ledger: &tempfile::TempDir,
    ledger_name: &str,
) -> ChildGuard {
    let listen = format!("127.0.0.1:{}", free_port());
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gw.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_TOKEN", token)
        .env("ESCUREL_RUNNER_HARNESS", "echo")
        .env(
            "ESCUREL_RUNNER_LEDGER_PATH",
            ledger.path().join(ledger_name),
        )
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms")
        .env("ESCUREL_RUNNER_STATUS_INTERVAL", "1s");
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    ChildGuard(cmd.spawn().expect("spawn runner"))
}

/// The status body carries the configured allow-list, sorted, next to the default harness.
#[tokio::test]
async fn runner_status_publishes_sorted_harness_allow_list() {
    let gw = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(FixtureBuilder::new().tenant(TENANT).done()),
        ..Default::default()
    })
    .await;
    let admin = gw.mint_token(TENANT, Role::Admin);
    let ledger_dir = tempfile::tempdir().expect("tempdir");
    let _runner = spawn_runner(
        &gw,
        &admin,
        &[("ESCUREL_RUNNER_HARNESS_ALLOW", "gemini,echo,claude")],
        &ledger_dir,
        "ledger.sqlite",
    );

    let body = wait_for_status(&gw, &admin).await;
    assert_eq!(body["harness"], "echo", "{body}");
    assert_eq!(
        body["harness_allow"],
        json!(["claude", "echo", "gemini"]),
        "{body}"
    );
}

/// The allow-list is names only: no paths, commands, env or secrets leak into the status body.
#[tokio::test]
async fn runner_status_harness_allow_list_leaks_no_detail() {
    let gw = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(FixtureBuilder::new().tenant(TENANT).done()),
        ..Default::default()
    })
    .await;
    let admin = gw.mint_token(TENANT, Role::Admin);
    let ledger_dir = tempfile::tempdir().expect("tempdir");
    let _runner = spawn_runner(
        &gw,
        &admin,
        &[("ESCUREL_RUNNER_HARNESS_ALLOW", "echo,claude")],
        &ledger_dir,
        "ledger.sqlite",
    );

    let body = wait_for_status(&gw, &admin).await;
    let allow = body["harness_allow"]
        .as_array()
        .unwrap_or_else(|| panic!("harness_allow is an array of names: {body}"));
    assert_eq!(allow.len(), 2, "{body}");
    for name in allow {
        let name = name
            .as_str()
            .unwrap_or_else(|| panic!("allow-list entries are strings: {body}"));
        assert!(
            !name.is_empty() && name.len() <= 40,
            "short display names only, got {name:?}"
        );
        assert!(
            name.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "names only, never paths or commands, got {name:?}"
        );
    }
    let known = [
        "runner_id",
        "version",
        "harness",
        "harness_allow",
        "tenant",
        "live_runs",
        "paused_tenants",
        "runs",
        "throttled",
        "harness_permits_available",
        "draining",
        "uptime_s",
        "last_poll_age_ms",
    ];
    let obj = body.as_object().expect("status body is an object");
    for key in obj.keys() {
        assert!(
            known.contains(&key.as_str()),
            "unexpected status key {key:?}: {body}"
        );
    }
}

/// Without `ESCUREL_RUNNER_HARNESS_ALLOW` the published list is the configured harness alone.
#[tokio::test]
async fn runner_status_harness_allow_defaults_to_the_harness_alone() {
    let gw = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(FixtureBuilder::new().tenant(TENANT).done()),
        ..Default::default()
    })
    .await;
    let admin = gw.mint_token(TENANT, Role::Admin);
    let ledger_dir = tempfile::tempdir().expect("tempdir");
    let _runner = spawn_runner(&gw, &admin, &[], &ledger_dir, "ledger.sqlite");

    let body = wait_for_status(&gw, &admin).await;
    assert_eq!(body["harness"], "echo", "{body}");
    assert_eq!(body["harness_allow"], json!(["echo"]), "{body}");
}
