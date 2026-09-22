//! The whole P1 thread, end to end (knowledge-workbench backend, PR11 —
//! BRD ACC-1/2/5 as far as P1 reaches): one real gateway, one real runner
//! in MINTED mode (a static bearer carries no run claims), the real echo
//! harness under `autonomy: review`, one human approving.
//!
//! What must hold: the run writes its lifecycle; the agent's draft records
//! the run (from the token the runner signed); the agent's reported plan
//! lands on `run-finished`; a human's promotion publishes review events;
//! `list_lineage` folds it all under the root; and nothing under an
//! `escurel:` label ever becomes a run of its own.
//!
//! What is NOT here, on purpose: the cascade a promotion should trigger.
//! Nothing turns a human's promotion into the next hop today — the runner
//! cascades only its own landed writes — so that is a P2 item (the runner's
//! review-event subscriber), not something this test can assert.

use std::process::{Child, Command};
use std::time::{Duration, Instant};

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role, free_port};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const SKILL: &str = "note";
const SKILL_BODY: &str = "---\ntype: skill\nid: note\nautonomy: review\n---\n# note\n\n\
    Fold the event into the note it concerns; a human approves.\n";
const INSTANCE_BODY: &str =
    "---\ntype: instance\nid: plan\nskill: note\n---\n# Plan\n\nBASELINE.\n";
const PAGE: &str = "markdown/instances/note/plan.md";

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

async fn await_terminal(listen: &str, event_id: &str) -> (String, String) {
    let url = format!("http://{listen}/debug/run?tenant={TENANT}&event_id={event_id}");
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Ok(resp) = reqwest::get(&url).await
            && let Ok(v) = resp.json::<Value>().await
            && let Some(status) = v["status"].as_str()
            && status != "pending"
        {
            return (v["run_id"].as_str().unwrap().to_owned(), status.to_owned());
        }
        assert!(Instant::now() < deadline, "run never reached a terminal");
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

fn body(e: &Value) -> Value {
    serde_json::from_str(e["body"].as_str().unwrap_or("{}")).unwrap_or_default()
}

#[tokio::test]
async fn an_echo_review_run_promoted_by_a_human_yields_a_full_lineage_tree() {
    let gw = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill(SKILL, SKILL_BODY)
                .instance(SKILL, "plan", INSTANCE_BODY)
                .done(),
        ),
        ..Default::default()
    })
    .await;
    let admin = gw.mint_token(TENANT, Role::Admin);
    let human = gw.mint_token_with_sub(TENANT, Role::Agent, "alice");

    let r = call(
        &gw,
        &admin,
        "capture_event",
        json!({ "source": "manual", "mime": "text/plain", "label_skill": SKILL,
                "instance_page_id": PAGE, "title": "renewal request", "body": "please renew" }),
    )
    .await;
    let root = r["event_id"].as_str().unwrap().to_owned();

    // MINTED runner: only a runner holding a signing key scopes a run to
    // its agent and signs the run's identity into that token.
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
            ledger_dir.keep().join("ledger.sqlite"),
        )
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms");
    let _runner = ChildGuard(cmd.spawn().expect("spawn runner"));

    // 1. The run lands as a HELD write (a draft), recorded processed.
    let (run_id, status) = await_terminal(&listen, &root).await;
    assert_eq!(status, "processed");
    let deadline = Instant::now() + Duration::from_secs(10);
    let events = loop {
        let r = call(&gw, &admin, "list_events", json!({ "run_id": run_id })).await;
        let evs = r["events"].as_array().cloned().unwrap_or_default();
        if evs.iter().any(|e| e["title"] == "run-finished") || Instant::now() >= deadline {
            break evs;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let titles: Vec<&str> = events
        .iter()
        .map(|e| e["title"].as_str().unwrap())
        .collect();
    assert!(titles.contains(&"run-started"), "{titles:?}");
    assert!(
        titles.contains(&"run-progress"),
        "the echo reported its plan: {titles:?}"
    );
    let finished = events
        .iter()
        .find(|e| e["title"] == "run-finished")
        .expect("run-finished");
    let fin = body(finished);
    assert_eq!(fin["status"], "processed", "{fin}");
    assert_eq!(fin["held"], true, "a review run holds its write: {fin}");
    assert!(
        fin["plan"].is_array(),
        "the final plan rides on run-finished: {fin}"
    );
    assert_eq!(finished["provenance"]["runner"]["autonomy"], "review");

    // 2. The draft records the run — from the token, never an argument.
    let sets = call(&gw, &admin, "list_changesets", json!({})).await;
    let cs = &sets["changesets"][0];
    assert_eq!(cs["run_id"], run_id, "{sets}");
    assert_eq!(cs["root_event_id"], root);
    assert_eq!(cs["status"], "open");
    let changeset_id = cs["changeset_id"].as_str().unwrap().to_owned();
    let drafts = call(&gw, &admin, "list_drafts", json!({})).await;
    assert_eq!(
        drafts["drafts"][0]["author"],
        format!("agent:{SKILL}"),
        "{drafts}"
    );
    assert_eq!(drafts["drafts"][0]["run_id"], run_id);
    // The root still waits in the inbox: nothing has landed.
    let inbox = call(&gw, &admin, "list_inbox", json!({})).await;
    assert_eq!(inbox["events"].as_array().unwrap().len(), 1, "{inbox}");

    // 3. A human promotes; the bus says so; the page changed; the root is
    //    retired from the inbox.
    let r = call(
        &gw,
        &human,
        "promote_changeset",
        json!({ "changeset_id": changeset_id }),
    )
    .await;
    assert_eq!(r["ok"], true, "{r}");
    let page = call(&gw, &human, "expand", json!({ "page_id": PAGE })).await;
    assert!(
        page["body"].as_str().unwrap().contains("folded event"),
        "{page}"
    );
    let inbox = call(&gw, &admin, "list_inbox", json!({})).await;
    assert!(
        inbox["events"].as_array().unwrap().is_empty(),
        "retired on promotion: {inbox}"
    );
    let reviews = call(
        &gw,
        &admin,
        "list_events",
        json!({ "root_event_id": root, "include_system": true, "kind": "system" }),
    )
    .await;
    let review_titles: Vec<&str> = reviews["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["label_skill"] == "escurel:review")
        .map(|e| e["title"].as_str().unwrap())
        .collect();
    assert_eq!(
        review_titles,
        ["draft-created", "draft-promoted", "changeset-promoted"],
        "{reviews}"
    );

    // 4. One read for the thread.
    let tree = call(
        &gw,
        &human,
        "list_lineage",
        json!({ "root_event_id": root }),
    )
    .await;
    let nodes = tree["nodes"].as_array().unwrap();
    let find = |id: &str| {
        nodes
            .iter()
            .find(|n| n["id"] == id)
            .cloned()
            .unwrap_or_default()
    };
    assert!(find(&root)["parent"].is_null(), "{tree}");
    let run = find(&run_id);
    assert_eq!(run["type"], "run", "{tree}");
    assert_eq!(run["parent"], root);
    assert_eq!(run["state"], "processed");
    assert_eq!(run["held"], true);
    assert_eq!(run["harness"], "echo");
    let cs_node = find(&changeset_id);
    assert_eq!(cs_node["parent"], run_id);
    assert_eq!(cs_node["state"], "promoted");
    let draft_node = nodes
        .iter()
        .find(|n| n["type"] == "draft")
        .expect("draft node");
    assert_eq!(draft_node["parent"], changeset_id);
    assert_eq!(draft_node["state"], "promoted");
    assert_eq!(nodes.len(), 4, "root, run, changeset, draft: {tree}");

    // 5. ACC-5: nothing under `escurel:` became a run of its own.
    let ledger: Value = reqwest::get(format!("http://{listen}/debug/ledger"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(ledger["total"], 1, "{ledger}");
}
