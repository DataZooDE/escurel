//! A human's promotion cascades (knowledge-workbench backend P2-1 — the
//! gap PR11 recorded; BRD ACC-1's shape).
//!
//! The contract always said "cascade fires on promotion, because until a
//! human promotes it nothing has landed", and only the first half was
//! true: `emit_cascade` ran after a write the RUNNER landed, never after a
//! human landed a held one. The runner now tails `escurel:review` and, on
//! `draft-promoted`, cascades from the promoted page under the original
//! run's lineage. One real gateway, one real minted runner, the echo
//! harness drafting under review, one human approving — and then a second
//! run the human never asked for directly, which is the point.

use std::process::{Child, Command};
use std::time::{Duration, Instant};

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role, free_port};
use serde_json::{Value, json};

const TENANT: &str = "acme";
/// The review-gated skill a human must approve…
const MEETING_SKILL_BODY: &str = "---\ntype: skill\nid: meeting\nautonomy: review\n---\n# meeting\n\n\
    Fold the meeting note into the decision record it concerns; a human approves.\n";
/// …and the skill of the page it folds into, whose own change cascades onto
/// its changelog page (`cascade_target`) and lands its writes itself. The
/// target is a DIFFERENT page on purpose: re-entering the page run 1 wrote
/// would close a cycle, and the loop control dead-letters that hop.
const DECISION_SKILL_BODY: &str = "---\ntype: skill\nid: decision-record\nautonomy: auto\n\
    cascade_target: markdown/instances/decision-record/changelog.md\n---\n# decision-record\n\n\
    Maintain the running decision record.\n";
const DECISION_INSTANCE_BODY: &str =
    "---\ntype: instance\nid: q3\nskill: decision-record\n---\n# Q3\n\nBASELINE.\n";
const CHANGELOG_INSTANCE_BODY: &str =
    "---\ntype: instance\nid: changelog\nskill: decision-record\n---\n# Changelog\n\n(empty)\n";
const PAGE: &str = "markdown/instances/decision-record/q3.md";
const CHANGELOG: &str = "markdown/instances/decision-record/changelog.md";

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

async fn ledger(listen: &str) -> Value {
    reqwest::get(format!("http://{listen}/debug/ledger"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

#[tokio::test]
async fn a_promoted_draft_cascades_under_the_original_runs_lineage() {
    let gw = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("meeting", MEETING_SKILL_BODY)
                .skill("decision-record", DECISION_SKILL_BODY)
                .instance("decision-record", "q3", DECISION_INSTANCE_BODY)
                .instance("decision-record", "changelog", CHANGELOG_INSTANCE_BODY)
                .done(),
        ),
        ..Default::default()
    })
    .await;
    let admin = gw.mint_token(TENANT, Role::Admin);
    let human = gw.mint_token_with_sub(TENANT, Role::Agent, "alice");

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

    // The root: a meeting note pre-flagged onto the decision record.
    let r = call(
        &gw,
        &admin,
        "capture_event",
        json!({ "source": "manual", "mime": "text/plain", "label_skill": "meeting",
                "instance_page_id": PAGE, "title": "kickoff", "body": "we decided X" }),
    )
    .await;
    let root = r["event_id"].as_str().unwrap().to_owned();

    // Run 1 drafts and stops; the human promotes.
    let deadline = Instant::now() + Duration::from_secs(60);
    let changeset_id = loop {
        let sets = call(&gw, &admin, "list_changesets", json!({})).await;
        if let Some(cs) = sets["changesets"].as_array().and_then(|a| a.first()) {
            break cs["changeset_id"].as_str().unwrap().to_owned();
        }
        assert!(Instant::now() < deadline, "run 1 never drafted");
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    assert_eq!(
        ledger(&listen).await["total"],
        1,
        "one run before the human acts"
    );
    let r = call(
        &gw,
        &human,
        "promote_changeset",
        json!({ "changeset_id": changeset_id }),
    )
    .await;
    assert_eq!(r["ok"], true, "{r}");

    // The runner notices, cascades from the promoted page under run 1's
    // lineage, and the cascade is dispatched as run 2.
    let deadline = Instant::now() + Duration::from_secs(60);
    let hop = loop {
        let tree = call(&gw, &admin, "list_events", json!({ "root_event_id": root })).await;
        let hop = tree["events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["label_skill"] == "decision-record")
            .cloned();
        if let Some(h) = hop
            && ledger(&listen).await["total"] == 2
            && ledger(&listen).await["terminal"] == 2
        {
            break h;
        }
        assert!(
            Instant::now() < deadline,
            "promotion never cascaded: {tree}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    let runner = &hop["provenance"]["runner"];
    assert_eq!(runner["root_event_id"], root, "{hop}");
    assert_eq!(runner["depth"], 1);
    assert_eq!(runner["parent_event_id"], root);
    assert!(
        runner["parent_run_id"]
            .as_str()
            .is_some_and(|r| !r.is_empty()),
        "{hop}"
    );
    assert_eq!(runner["cause"], "instance-updated:meeting");
    assert_eq!(
        runner["changed_instance"], PAGE,
        "the promoted page is what changed"
    );
    assert_eq!(
        hop["instance_page_id"], CHANGELOG,
        "cascade_target pre-flags the hop"
    );
    let dlq: Value = reqwest::get(format!("http://{listen}/dlq"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        hop["status"], "processed",
        "run 2 folded it: dlq={dlq} hop={hop}"
    );
    let ledger_now = ledger(&listen).await;
    assert_eq!(ledger_now["succeeded"], 2, "both runs landed: {ledger_now}");
    // Promoting again (a retry) does not cascade a second time.
    let r = call(
        &gw,
        &human,
        "promote_changeset",
        json!({ "changeset_id": changeset_id }),
    )
    .await;
    assert_eq!(r["already_decided"], true, "{r}");
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(
        ledger(&listen).await["total"],
        2,
        "no second cascade for a retried decision"
    );

    // And the thread reads as one tree: root → run 1 → {changeset → draft, hop → run 2}.
    let tree = call(
        &gw,
        &admin,
        "list_lineage",
        json!({ "root_event_id": root }),
    )
    .await;
    let nodes = tree["nodes"].as_array().unwrap();
    let hop_node = nodes
        .iter()
        .find(|n| n["id"] == hop["event_id"])
        .expect("hop node");
    let run1 = nodes
        .iter()
        .find(|n| n["type"] == "run" && n["parent"] == json!(root))
        .expect("run 1");
    assert_eq!(hop_node["parent"], run1["id"], "{tree}");
    let run2 = nodes
        .iter()
        .find(|n| n["type"] == "run" && n["parent"] == hop["event_id"])
        .expect("run 2");
    assert_eq!(run2["state"], "processed", "{tree}");
    assert_eq!(nodes.iter().filter(|n| n["type"] == "run").count(), 2);
}
