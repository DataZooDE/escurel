//! `cascade_target: produced` routes the follow-on to the instance this run
//! actually wrote — not to one page named in frontmatter.
//!
//! A static target cannot express the chain most corpora want. `datazoo-loops`
//! (#502) holds 37 emails, 24 contacts and 5 customers, and its skills describe
//! themselves as folding an event "into the typed entities it concerns" —
//! which contact that is depends on the content. A static `cascade_target` on
//! `contact` would send all 24 contacts' follow-ons to one hard-coded page: a
//! cascade that fires, looks healthy, and files into the wrong record. So that
//! corpus declares `autonomy: review` on all eleven skills and
//! `cascade_target` on none, and nothing chains.
//!
//! **The fixture is built so that only the right answer passes.** There are
//! TWO beta instances. The seed event is pre-flagged onto `b2`, so `b2` is the
//! page that gets written — while `b1` sits there as exactly the page a static
//! target would have named. A follow-on on `b1` is the bug; a follow-on on
//! `b2` is the fix; an unassigned follow-on is the old absent-key default.
//! The three outcomes are distinguishable, which is the point of the second
//! instance.
//!
//! The second test pins the static form unchanged, because this is a value
//! added to an existing key: a corpus that names a page must keep getting that
//! page, and "the new value works" is not the same claim as "the old one still
//! does".
//!
//! Real gateway, real runner binary, the `echo` harness — deterministic, so
//! the assertions are about routing and not about what a model felt like
//! doing.

use std::net::TcpListener;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const B2_PAGE: &str = "markdown/instances/beta/b2.md";
const B1_PAGE: &str = "markdown/instances/beta/b1.md";

const ALPHA_SKILL_BODY: &str =
    "---\ntype: skill\nid: alpha\nautonomy: auto\n---\n# alpha\n\nFold the event in.\n";
const A_INSTANCE_BODY: &str =
    "---\ntype: instance\nid: a1\nskill: alpha\n---\n# A1\n\nBASELINE alpha.\n";
const B1_INSTANCE_BODY: &str =
    "---\ntype: instance\nid: b1\nskill: beta\n---\n# B1\n\nBASELINE b1.\n";
const B2_INSTANCE_BODY: &str =
    "---\ntype: instance\nid: b2\nskill: beta\n---\n# B2\n\nBASELINE b2.\n";

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind")
        .local_addr()
        .expect("addr")
        .port()
}

async fn call_mcp(p: &EscurelProcess, name: &str, args: Value) -> Value {
    let token = p.mint_token(TENANT, Role::Agent);
    let resp = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": args },
        }))
        .send()
        .await
        .expect("post /mcp");
    assert_eq!(resp.status(), 200, "http status");
    let body: Value = resp.json().await.expect("json");
    assert!(body.get("error").is_none(), "tool {name} error: {body}");
    let result = body["result"].clone();
    result.get("structuredContent").cloned().unwrap_or(result)
}

/// Boot a gateway whose `beta` skill declares `cascade_target: <target>`, seed
/// an alpha event pre-flagged onto `b2`, and run the real runner against it.
/// Returns the cascaded beta event, or `None` if none appeared in time.
async fn cascaded_beta_event(beta_cascade_target: &str) -> Option<Value> {
    let beta_skill = format!(
        "---\ntype: skill\nid: beta\nautonomy: auto\ncascade_target: {beta_cascade_target}\n\
         ---\n# beta\n\nFold the event in.\n"
    );
    let gateway = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("alpha", ALPHA_SKILL_BODY)
                .skill("beta", beta_skill.as_str())
                .instance("alpha", "a1", A_INSTANCE_BODY)
                .instance("beta", "b1", B1_INSTANCE_BODY)
                .instance("beta", "b2", B2_INSTANCE_BODY)
                .done(),
        ),
        ..Default::default()
    })
    .await;

    // The seed is pre-flagged onto b2, so b2 is the page that gets written —
    // and b1 is left as the page a static target would have named.
    let captured = call_mcp(
        &gateway,
        "capture_event",
        json!({
            "source": "manual",
            "mime": "text/plain",
            "label_skill": "alpha",
            "instance_page_id": B2_PAGE,
            "title": "seed",
            "body": "kick off the cascade",
        }),
    )
    .await;
    let seed = captured["event_id"].as_str().expect("event_id").to_owned();

    let token = gateway.mint_token(TENANT, Role::Agent);
    let ledger_dir = tempfile::tempdir().expect("tempdir");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env(
        "ESCUREL_RUNNER_LISTEN",
        format!("127.0.0.1:{}", free_port()),
    )
    .env("ESCUREL_RUNNER_GATEWAY_URL", gateway.base_url())
    .env("ESCUREL_RUNNER_TENANT", TENANT)
    .env("ESCUREL_RUNNER_TOKEN", &token)
    .env("ESCUREL_RUNNER_HARNESS", "echo")
    .env(
        "ESCUREL_RUNNER_LEDGER_PATH",
        ledger_dir.path().join("ledger.sqlite").to_str().unwrap(),
    )
    .env("ESCUREL_RUNNER_MAX_DEPTH", "4")
    .env("ESCUREL_RUNNER_MAX_RUNS_PER_ROOT", "16")
    .env("ESCUREL_RUNNER_POLL_INTERVAL", "150ms");
    let _runner = ChildGuard(cmd.spawn().expect("spawn escurel-runner"));

    // Sweep both the live inbox and each instance's history: a hop transits
    // the inbox and then binds, so which surface holds it depends on timing.
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        let inbox = call_mcp(&gateway, "list_inbox", json!({})).await;
        let mut all: Vec<Value> = inbox["events"].as_array().cloned().unwrap_or_default();
        for page in [B1_PAGE, B2_PAGE, "markdown/instances/alpha/a1.md"] {
            let hist = call_mcp(&gateway, "list_events", json!({ "instance_page_id": page })).await;
            if let Some(more) = hist["events"].as_array() {
                all.extend(more.iter().cloned());
            }
        }
        let found = all.into_iter().find(|e| {
            e["label_skill"] == json!("beta")
                && e["event_id"] != json!(seed)
                && e["provenance"]["runner"].is_object()
        });
        if found.is_some() {
            return found;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    None
}

#[tokio::test]
async fn a_produced_target_routes_the_follow_on_to_the_page_just_written() {
    let hop = cascaded_beta_event("produced")
        .await
        .expect("a cascaded beta event within 60s");

    assert_eq!(
        hop["instance_page_id"],
        json!(B2_PAGE),
        "the follow-on must land on the page this run WROTE: {hop}"
    );
    // Spelled out rather than left to the assertion above, because this is
    // the failure that would look healthy: `b1` is a real beta page, a
    // follow-on on it would run, and the only sign anything was wrong is that
    // the wrong record grew.
    assert_ne!(
        hop["instance_page_id"],
        json!(B1_PAGE),
        "routing to the page a static target would have named is the bug: {hop}"
    );
}

#[tokio::test]
async fn a_static_target_still_routes_to_the_page_it_names() {
    // The backward-compatibility control. `produced` is a new VALUE on an
    // existing key, so a corpus that names a page must keep getting it — and
    // without this, a change that routed everything to the produced instance
    // would pass the test above and silently break every existing cascade.
    let hop = cascaded_beta_event(B1_PAGE)
        .await
        .expect("a cascaded beta event within 60s");

    assert_eq!(
        hop["instance_page_id"],
        json!(B1_PAGE),
        "a static target must still route to the page it names: {hop}"
    );
}
