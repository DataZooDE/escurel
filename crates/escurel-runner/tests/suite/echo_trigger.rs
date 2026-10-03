//! The echo harness folds the event a run was TRIGGERED for, not whatever has waited longest.
//!
//! A `review` run leaves its event in the inbox until a human promotes its draft, so an OLDER event
//! can sit there for as long as the human takes. The harness used to fold the oldest inbox event that
//! had a target, so a run started for a NEWER event folded the older one instead: it either did
//! nothing for its own event (a clean no-op) or tried to draft on a page that already held an open
//! draft and dead-lettered. Every test and demo that left one review run waiting then broke the next.
//!
//! Real gateway, real runner in MINTED mode, real echo harness.

use std::process::{Child, Command};
use std::time::{Duration, Instant};

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role, free_port};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const SKILL_BODY: &str = "---\nkind: skill\nid: renewal\nautonomy: review\n---\n# renewal\n\nFold the event into the instance.\n";
const PAGE_A: &str = "markdown/instances/renewal/c1.md";
const PAGE_B: &str = "markdown/instances/renewal/c2.md";

fn instance(id: &str) -> String {
    format!("---\nkind: instance\nid: {id}\nskill: renewal\n---\n# {id}\n\nBASELINE.\n")
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

async fn capture(p: &EscurelProcess, token: &str, page: &str, title: &str) -> String {
    let r = call(
        p,
        token,
        "capture_event",
        json!({ "source": "workbench", "mime": "text/plain", "label_skill": "renewal",
                "instance_page_id": page, "title": title, "body": title,
                "provenance": { "manual": { "mode": "run" } } }),
    )
    .await;
    r["event_id"].as_str().unwrap().to_owned()
}

/// The open draft the run for `event_id` left on `page`, waiting for a human.
async fn open_draft_for(p: &EscurelProcess, token: &str, event_id: &str, page: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let drafts = call(p, token, "list_drafts", json!({})).await;
        if let Some(d) = drafts["drafts"].as_array().unwrap().iter().find(|d| {
            d["status"] == "open" && d["event_id"] == event_id && d["target_page_id"] == page
        }) {
            return d.clone();
        }
        assert!(
            Instant::now() < deadline,
            "the run for {event_id} never left an open draft on {page}: {drafts}"
        );
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

#[tokio::test]
async fn a_run_folds_its_own_event_while_an_older_one_still_waits_on_a_human() {
    let gw = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("renewal", SKILL_BODY)
                .instance("renewal", "c1", instance("c1"))
                .instance("renewal", "c2", instance("c2"))
                .done(),
        ),
        ..Default::default()
    })
    .await;
    let alice = gw.mint_token_with_sub(TENANT, Role::Agent, "alice");

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

    // The first run drafts a change and leaves its event in the inbox: it waits for a human.
    let older = capture(&gw, &alice, PAGE_A, "older signal").await;
    let first = open_draft_for(&gw, &alice, &older, PAGE_A).await;
    assert_eq!(first["event_id"], older.as_str());

    // A NEWER event, for a different page, is started while the older one still waits. Its run must
    // fold ITS event: before the harness named its trigger it folded the older one, tried to draft on
    // the page that already held an open draft, and dead-lettered, leaving nothing on this page.
    let newer = capture(&gw, &alice, PAGE_B, "newer signal").await;
    let second = open_draft_for(&gw, &alice, &newer, PAGE_B).await;
    assert_eq!(second["event_id"], newer.as_str());
    assert_eq!(second["target_page_id"], PAGE_B);
}
