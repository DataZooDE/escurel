//! Manual start (knowledge-workbench backend P2-5a — BRD FR-M-1). A human
//! starts a run by capturing an ordinary event with a `provenance.manual`
//! block: `{harness?, mode?}`. The gateway stamps `requested_by` from the
//! token (a caller-supplied one is replaced) and refuses an unknown `mode`;
//! the runner honours `harness` only within `ESCUREL_RUNNER_HARNESS_ALLOW`
//! and otherwise fails the run closed rather than running something else.
//! The run's `run-started` carries the manual block.
//!
//! Real gateway, real runner binary, real echo harness.

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

async fn rpc(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
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

async fn call(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    let body = rpc(p, token, name, args).await;
    assert!(body.get("error").is_none(), "{name}: {body}");
    body["result"]["structuredContent"].clone()
}

fn manual(manual: Value, title: &str) -> Value {
    json!({ "source": "workbench", "mime": "text/plain", "label_skill": "renewal",
            "instance_page_id": PAGE, "title": title, "body": "please renew",
            "provenance": { "manual": manual } })
}

async fn wait_for_terminal(listen: &str, event_id: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(resp) = reqwest::get(format!(
            "http://{listen}/debug/run?tenant={TENANT}&event_id={event_id}"
        ))
        .await
            && resp.status().is_success()
            && let Ok(run) = resp.json::<Value>().await
            && run["status"] != "pending"
        {
            return run;
        }
        assert!(Instant::now() < deadline, "run never reached a terminal");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The run's event with `title`, waiting briefly: the runner writes its
/// lifecycle events best-effort right after the ledger's terminal, so a
/// read the instant `/debug/run` flips can precede `run-finished`.
async fn run_event(p: &EscurelProcess, token: &str, run_id: &str, title: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let own = call(p, token, "list_events", json!({ "run_id": run_id })).await;
        if let Some(e) = own["events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["title"] == title)
        {
            return e.clone();
        }
        assert!(Instant::now() < deadline, "no {title}: {own}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn a_manual_start_names_its_harness_within_the_allow_list_and_who_asked() {
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
    let alice = gw.mint_token_with_sub(TENANT, Role::Agent, "alice");

    let listen = format!("127.0.0.1:{}", free_port());
    let ledger_dir = tempfile::tempdir().expect("tempdir");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gw.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_TOKEN", &admin)
        .env("ESCUREL_RUNNER_HARNESS", "echo")
        .env("ESCUREL_RUNNER_HARNESS_ALLOW", "echo")
        .env(
            "ESCUREL_RUNNER_LEDGER_PATH",
            ledger_dir.keep().join("ledger.sqlite"),
        )
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms");
    let _runner = ChildGuard(cmd.spawn().expect("spawn runner"));

    // An unknown mode is a caller mistake, refused at the gateway.
    let bad = rpc(
        &gw,
        &alice,
        "capture_event",
        manual(json!({ "mode": "explode" }), "bad"),
    )
    .await;
    assert_eq!(bad["error"]["code"], -32602, "{bad}");
    assert!(
        bad["error"]["message"].as_str().unwrap().contains("manual"),
        "{bad}"
    );

    // A manual start with an allowed harness runs on it; the requester is
    // the token's subject, whatever the caller wrote.
    let r = call(
        &gw,
        &alice,
        "capture_event",
        manual(
            json!({ "harness": "echo", "requested_by": "mallory" }),
            "renew now",
        ),
    )
    .await;
    assert_eq!(
        r["provenance"]["manual"]["requested_by"], "alice",
        "server-stamped: {r}"
    );
    assert_eq!(r["provenance"]["manual"]["mode"], "run", "defaulted: {r}");
    let e1 = r["event_id"].as_str().unwrap().to_owned();
    let run = wait_for_terminal(&listen, &e1).await;
    assert_eq!(run["status"], "processed", "{run}");
    let started = run_event(&gw, &admin, run["run_id"].as_str().unwrap(), "run-started").await;
    assert_eq!(
        started["provenance"]["runner"]["harness"], "echo",
        "{started}"
    );
    let m = &started["provenance"]["runner"]["manual"];
    assert_eq!(
        m["requested_by"], "alice",
        "the run records who asked: {started}"
    );
    assert_eq!(m["harness"], "echo", "{started}");
    assert_eq!(m["mode"], "run", "{started}");

    // A harness outside the allow-list fails the run closed — nothing runs
    // in its place — and the terminal says why.
    let r = call(
        &gw,
        &alice,
        "capture_event",
        manual(json!({ "harness": "codex" }), "renew with codex"),
    )
    .await;
    let e2 = r["event_id"].as_str().unwrap().to_owned();
    let run = wait_for_terminal(&listen, &e2).await;
    assert_eq!(
        run["status"], "dead_letter",
        "refused, not run on the default: {run}"
    );
    let finished = run_event(&gw, &admin, run["run_id"].as_str().unwrap(), "run-finished").await;
    let body: Value = serde_json::from_str(finished["body"].as_str().unwrap()).unwrap();
    assert_eq!(body["status"], "dead_letter", "{body}");
    assert_eq!(body["reason"], "permanent", "{body}");
    assert!(
        body["error"].as_str().unwrap_or("").contains("codex"),
        "{body}"
    );
    assert!(
        body["error"]
            .as_str()
            .unwrap_or("")
            .contains("HARNESS_ALLOW"),
        "{body}"
    );
    let page = call(&gw, &admin, "expand", json!({ "page_id": PAGE })).await;
    assert!(!page["body"].as_str().unwrap().contains("codex"), "{page}");
}
