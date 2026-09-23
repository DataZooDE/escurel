//! Per-skill narrowing of the per-run agent token (knowledge-workbench backend
//! P3-6 — #510 step 2). With `ESCUREL_RUNNER_AGENT_NARROW=1` the bearer a run's
//! harness receives carries `escurel:agent` plus the target skill's own
//! `acl.create` / `acl.update` groups instead of `escurel:admin`, so the
//! harness may write that skill's instances and nothing else. The runner's
//! OWN bookkeeping (run events, cascades) keeps its admin identity.
//!
//! Real gateway under `ESCUREL_WRITE_ACL=enforce`, real runner in MINTED mode
//! (a static bearer cannot mint), real echo harness. Default off: the same
//! corpus with the flag unset writes everything, as before.

use std::process::{Child, Command};
use std::time::{Duration, Instant};

use escurel_test_support::{
    AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role, WriteAclMode, free_port,
};
use serde_json::{Value, json};

const TENANT: &str = "acme";
/// Grants its writes to the `ops` group: a narrowed agent carries `ops`.
const RENEWAL: &str = "---\ntype: skill\nid: renewal\nautonomy: auto\n\
acl:\n  create: [ops]\n  update: [ops]\n---\n# renewal\n\nFold the event into the instance.\n";
/// Declares no write grant: admin-write-only under the tenant default.
const AUDIT: &str =
    "---\ntype: skill\nid: audit\nautonomy: auto\n---\n# audit\n\nFold the event.\n";

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn instance_body(skill: &str) -> String {
    format!("---\ntype: instance\nid: c1\nskill: {skill}\n---\n# C1\n\nBASELINE.\n")
}

fn page_id(skill: &str) -> String {
    format!("markdown/instances/{skill}/c1.md")
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

async fn gateway() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            write_acl: Some(WriteAclMode::Enforce),
            ..Default::default()
        },
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("renewal", RENEWAL)
                .skill("audit", AUDIT)
                .instance("renewal", "c1", instance_body("renewal").as_str())
                .instance("audit", "c1", instance_body("audit").as_str())
                .done(),
        ),
    })
    .await
}

/// A minted-mode runner (its own signing key) driving the echo harness.
fn spawn_runner(gw: &EscurelProcess, narrow: bool) -> (ChildGuard, String) {
    let (signing_key, kid) = gw.signing_material();
    let listen = format!("127.0.0.1:{}", free_port());
    let ledger_dir = tempfile::tempdir().expect("tempdir");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gw.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env_remove("ESCUREL_RUNNER_TOKEN")
        .env("ESCUREL_RUNNER_AUTH_ISSUER", gw.issuer_url())
        .env("ESCUREL_RUNNER_AUTH_KID", kid)
        .env("ESCUREL_RUNNER_AUTH_SIGNING_KEY", &signing_key)
        .env("ESCUREL_RUNNER_HARNESS", "echo")
        .env(
            "ESCUREL_RUNNER_LEDGER_PATH",
            ledger_dir.keep().join("ledger.duckdb"),
        )
        .env("ESCUREL_RUNNER_MAX_ATTEMPTS", "2")
        .env("ESCUREL_RUNNER_RETRY_BACKOFF", "50ms")
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms");
    if narrow {
        cmd.env("ESCUREL_RUNNER_AGENT_NARROW", "1");
    }
    (ChildGuard(cmd.spawn().expect("spawn runner")), listen)
}

async fn capture(gw: &EscurelProcess, skill: &str) -> String {
    let r = call(
        gw,
        "capture_event",
        json!({ "source": "manual", "mime": "text/plain", "label_skill": skill,
                "instance_page_id": page_id(skill), "title": "work item",
                "body": format!("ECHO_FOLD_MARKER work for {skill}") }),
    )
    .await;
    r["event_id"].as_str().unwrap().to_owned()
}

/// The ledger terminal for `event_id`.
async fn await_terminal(listen: &str, event_id: &str) -> String {
    let url = format!("http://{listen}/debug/run?tenant={TENANT}&event_id={event_id}");
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Ok(resp) = reqwest::get(&url).await
            && let Ok(v) = resp.json::<Value>().await
            && let Some(status) = v["status"].as_str()
            && status != "pending"
        {
            return status.to_owned();
        }
        assert!(Instant::now() < deadline, "run never reached a terminal");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn writer_of(gw: &EscurelProcess, skill: &str) -> (bool, String) {
    let e = call(gw, "expand", json!({ "page_id": page_id(skill) })).await;
    let folded = e["body"]
        .as_str()
        .unwrap_or_default()
        .contains("folded event");
    (
        folded,
        e["page"]["last_written_by"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
    )
}

#[tokio::test]
async fn a_narrowed_run_writes_its_skills_instance_and_is_refused_another_skills() {
    let gw = gateway().await;
    let (_runner, listen) = spawn_runner(&gw, true);

    // The skill that grants `ops`: the narrowed agent carries it and writes.
    let e1 = capture(&gw, "renewal").await;
    assert_eq!(await_terminal(&listen, &e1).await, "processed");
    let (folded, writer) = writer_of(&gw, "renewal").await;
    assert!(folded, "renewal's instance was not written");
    assert_eq!(writer, "agent:renewal", "the run writes as its agent");

    // The skill that grants nothing: admin-write-only, and the narrowed
    // agent is not admin — the write is refused and the run cannot land.
    let e2 = capture(&gw, "audit").await;
    let status = await_terminal(&listen, &e2).await;
    assert_ne!(status, "processed", "a narrowed agent must not write audit");
    let (folded, _) = writer_of(&gw, "audit").await;
    assert!(!folded, "audit's instance must be untouched");
}

#[tokio::test]
async fn with_the_flag_off_the_agent_token_keeps_admin_and_writes_both() {
    let gw = gateway().await;
    let (_runner, listen) = spawn_runner(&gw, false);
    for skill in ["renewal", "audit"] {
        let e = capture(&gw, skill).await;
        assert_eq!(await_terminal(&listen, &e).await, "processed", "{skill}");
        let (folded, writer) = writer_of(&gw, skill).await;
        assert!(folded, "{skill} was not written with the flag off");
        assert_eq!(writer, format!("agent:{skill}"));
    }
}
