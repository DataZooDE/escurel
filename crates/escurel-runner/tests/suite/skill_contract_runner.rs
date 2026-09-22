//! The runner honours the skill contract keys (knowledge-workbench backend
//! P2-7b — BRD FR-S-1..5): `autonomy: confirm` holds the write like
//! `review` and says so on the run; a skill's `harness:` is honoured within
//! `ESCUREL_RUNNER_HARNESS_ALLOW` and refused outside it; `actions:` limits
//! which skills a confirmed write may cascade to; `cascade.max_depth` caps
//! how deep a skill's cascades go. Real gateway, real runner, real echo.

use std::process::{Child, Command};
use std::time::{Duration, Instant};

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role, free_port};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const CONFIRM_SKILL: &str =
    "---\ntype: skill\nid: renewal\nautonomy: confirm\nsummary: s.\n---\n# renewal\n";
const CODEX_SKILL: &str =
    "---\ntype: skill\nid: invoice\nautonomy: auto\nharness: codex\n---\n# invoice\n";
const MEETING: &str = "---\ntype: skill\nid: meeting\nautonomy: auto\nactions:\n  - decision-record\n---\n# meeting\n";
const MEETING_LOCKED: &str = "---\ntype: skill\nid: meeting-locked\nautonomy: auto\nactions:\n  - changelog\n---\n# meeting-locked\n";
const DECISION: &str = "---\ntype: skill\nid: decision-record\nautonomy: auto\ncascade:\n  target: markdown/instances/changelog/log.md\n  max_depth: 1\n---\n# decision-record\n";
const CHANGELOG: &str = "---\ntype: skill\nid: changelog\nautonomy: auto\n---\n# changelog\n";
fn instance(skill: &str, id: &str) -> String {
    format!("---\ntype: instance\nid: {id}\nskill: {skill}\n---\n# {id}\n\nBASELINE.\n")
}

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

async fn capture(p: &EscurelProcess, token: &str, label: &str, page: &str) -> String {
    let r = call(
        p,
        token,
        "capture_event",
        json!({ "source": "manual", "mime": "text/plain", "label_skill": label,
                "instance_page_id": page, "title": format!("{label} event"), "body": "x" }),
    )
    .await;
    r["event_id"].as_str().unwrap().to_owned()
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

async fn lineage_labels(p: &EscurelProcess, token: &str, root: &str) -> Vec<String> {
    let tree = call(p, token, "list_events", json!({ "root_event_id": root })).await;
    tree["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["label_skill"].as_str().unwrap().to_owned())
        .collect()
}

fn spawn_runner(gw: &EscurelProcess, admin: &str) -> (ChildGuard, String) {
    let listen = format!("127.0.0.1:{}", free_port());
    let ledger_dir = tempfile::tempdir().expect("tempdir");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gw.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_TOKEN", admin)
        .env("ESCUREL_RUNNER_HARNESS", "echo")
        .env("ESCUREL_RUNNER_HARNESS_ALLOW", "echo")
        .env(
            "ESCUREL_RUNNER_LEDGER_PATH",
            ledger_dir.keep().join("ledger.sqlite"),
        )
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms");
    (ChildGuard(cmd.spawn().expect("spawn runner")), listen)
}

#[tokio::test]
async fn confirm_holds_the_write_and_a_declared_harness_is_honoured_within_the_allow_list() {
    let gw = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("renewal", CONFIRM_SKILL)
                .skill("invoice", CODEX_SKILL)
                .instance("renewal", "c1", instance("renewal", "c1"))
                .instance("invoice", "i1", instance("invoice", "i1"))
                .done(),
        ),
        ..Default::default()
    })
    .await;
    let admin = gw.mint_token(TENANT, Role::Admin);
    let (_runner, listen) = spawn_runner(&gw, &admin);

    // confirm: held like review, and the run says which policy held it.
    let e = capture(&gw, &admin, "renewal", "markdown/instances/renewal/c1.md").await;
    let run = wait_for_terminal(&listen, &e).await;
    assert_eq!(run["status"], "processed", "{run}");
    let sets = call(&gw, &admin, "list_changesets", json!({})).await;
    assert_eq!(
        sets["changesets"].as_array().unwrap().len(),
        1,
        "held for a human: {sets}"
    );
    let finished = run_event(&gw, &admin, run["run_id"].as_str().unwrap(), "run-finished").await;
    let body: Value = serde_json::from_str(finished["body"].as_str().unwrap()).unwrap();
    assert_eq!(body["held"], true, "{body}");
    assert_eq!(
        finished["provenance"]["runner"]["autonomy"], "confirm",
        "{finished}"
    );
    let page = call(
        &gw,
        &admin,
        "expand",
        json!({ "page_id": "markdown/instances/renewal/c1.md" }),
    )
    .await;
    assert!(
        !page["body"].as_str().unwrap().contains("folded"),
        "nothing landed: {page}"
    );

    // A skill-declared harness outside the allow-list fails the run closed.
    let e = capture(&gw, &admin, "invoice", "markdown/instances/invoice/i1.md").await;
    let run = wait_for_terminal(&listen, &e).await;
    assert_eq!(run["status"], "failed", "{run}");
    let finished = run_event(&gw, &admin, run["run_id"].as_str().unwrap(), "run-finished").await;
    let body: Value = serde_json::from_str(finished["body"].as_str().unwrap()).unwrap();
    assert!(
        body["error"].as_str().unwrap_or("").contains("codex"),
        "{body}"
    );
}

#[tokio::test]
async fn actions_limit_the_fan_out_and_cascade_max_depth_caps_the_chain() {
    let gw = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("meeting", MEETING)
                .skill("meeting-locked", MEETING_LOCKED)
                .skill("decision-record", DECISION)
                .skill("changelog", CHANGELOG)
                .instance("decision-record", "q3", instance("decision-record", "q3"))
                .instance("decision-record", "q4", instance("decision-record", "q4"))
                .instance("changelog", "log", instance("changelog", "log"))
                .done(),
        ),
        ..Default::default()
    })
    .await;
    let admin = gw.mint_token(TENANT, Role::Admin);
    let (_runner, listen) = spawn_runner(&gw, &admin);

    // meeting → decision-record is allowed (`actions`), so it cascades;
    // decision-record → changelog would be depth 2, beyond decision-record's
    // `cascade.max_depth: 1`, so the chain stops there.
    let root = capture(
        &gw,
        &admin,
        "meeting",
        "markdown/instances/decision-record/q3.md",
    )
    .await;
    let deadline = Instant::now() + Duration::from_secs(30);
    let hop = loop {
        let tree = call(&gw, &admin, "list_events", json!({ "root_event_id": root })).await;
        if let Some(h) = tree["events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["label_skill"] == "decision-record")
        {
            break h.clone();
        }
        assert!(Instant::now() < deadline, "meeting never cascaded: {tree}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    assert_eq!(
        hop["instance_page_id"], "markdown/instances/changelog/log.md",
        "cascade.target pre-flags the hop: {hop}"
    );
    let run2 = wait_for_terminal(&listen, hop["event_id"].as_str().unwrap()).await;
    assert_eq!(run2["status"], "processed", "{run2}");
    let page = call(
        &gw,
        &admin,
        "expand",
        json!({ "page_id": "markdown/instances/changelog/log.md" }),
    )
    .await;
    assert!(
        page["body"].as_str().unwrap().contains("folded"),
        "run 2 wrote the changelog: {page}"
    );
    tokio::time::sleep(Duration::from_millis(2000)).await;
    let labels = lineage_labels(&gw, &admin, &root).await;
    assert!(
        !labels.iter().any(|l| l == "changelog"),
        "depth 2 is capped: {labels:?}"
    );

    // meeting-locked may only fan out to `changelog`, so its decision-record
    // write does not cascade at all.
    let root2 = capture(
        &gw,
        &admin,
        "meeting-locked",
        "markdown/instances/decision-record/q4.md",
    )
    .await;
    let run = wait_for_terminal(&listen, &root2).await;
    assert_eq!(run["status"], "processed", "{run}");
    tokio::time::sleep(Duration::from_millis(2000)).await;
    let labels = lineage_labels(&gw, &admin, &root2).await;
    assert_eq!(
        labels,
        vec!["meeting-locked".to_owned()],
        "no cascade outside `actions`: {labels:?}"
    );
}
