//! Always-on real-`/mcp` DoD test for the Google ADK adapter (#154) — the
//! **first true trigger→adk-runner→instance end-to-end**, with **no mocks**.
//!
//! Per the issue's DoD ("a real runner invocation against the real `/mcp` …
//! assert the fold via real `expand`/`list_events`"), this is NOT a canned
//! parse: it stands up a **real `EscurelProcess` gateway**, seeds real data
//! via `FixtureBuilder`, `capture_event`s a real event, and drives a **real
//! adk-runner subprocess** that performs the **real escurel `/mcp` fold**
//! (`expand` → `update_page` → `assign_event`) over JSON-RPC under the scoped
//! bearer the adapter handed it.
//!
//! The runner here is a real `#!/usr/bin/env bash` + `curl`/`jq` script — a
//! genuine external process speaking the [`AdkHarness`] I/O contract
//! (token-less `AdkTask` JSON on stdin + the scoped bearer in
//! `ESCUREL_MCP_BEARER`; a `HarnessOutcome` JSON on stdout). It is the
//! deterministic analogue of the template's `StaticBrain`: only the *brain*
//! (which tool to call) is hard-coded; **every escurel effect is a real
//! `/mcp` call against the real gateway** — no mock of the gateway, no
//! stubbed transport. The live test that swaps this stub for a real adk-rust
//! `LlmAgent` lives in `adk_live.rs`, `#[ignore]`'d.
//!
//! Flow:
//! 1. Spawn a real gateway (`EscurelProcess`, TestIssuer auth) seeded via
//!    `FixtureBuilder` with a skill page + a target instance page.
//! 2. `capture_event` a real inbox event labelled with the skill and
//!    pre-flagged to the target instance, over the real `/mcp`.
//! 3. Write the real adk-runner curl stub; spawn the real `escurel-runner`
//!    with `ESCUREL_RUNNER_HARNESS=adk` + `ESCUREL_RUNNER_ADK_BIN=<stub>`. The
//!    runner packages the trigger and drives the stub through `AdkHarness`,
//!    which makes the real `/mcp` `update_page` + `assign_event` calls.
//! 4. Assert the end-to-end effect on the REAL gateway: the event is now
//!    `processed` (via `list_events`), the instance body carries the folded
//!    note and still has the baseline (via `expand`), and the runner's ledger
//!    run is terminal `processed` (via `GET /debug/ledger`).

use std::net::TcpListener;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const SKILL: &str = "customer";
const SKILL_BODY: &str =
    "---\ntype: skill\nid: customer\n---\n# customer\n\nFold the event into a customer instance.\n";
const INSTANCE_ID: &str = "globex";
const INSTANCE_BODY: &str =
    "---\ntype: instance\nid: globex\nskill: customer\n---\n# Globex\n\nBASELINE account state.\n";

/// Kills the spawned runner on drop so a test failure never orphans it.
struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    listener.local_addr().expect("read local_addr").port()
}

/// Call an MCP tool over `/mcp` with a freshly minted bearer; return the
/// JSON-RPC `result`.
async fn call_mcp(p: &EscurelProcess, role: Role, name: &str, args: Value) -> Value {
    let token = p.mint_token(TENANT, role);
    let resp = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": name, "arguments": args },
        }))
        .send()
        .await
        .expect("post /mcp");
    assert_eq!(resp.status(), 200, "http status");
    let body: Value = resp.json().await.unwrap();
    assert!(body.get("error").is_none(), "tool {name} error: {body}");
    let result = body["result"].clone();
    result.get("structuredContent").cloned().unwrap_or(result)
}

/// Write a REAL adk-runner stub: a bash + curl/jq script speaking the
/// [`escurel_runner_harness::AdkHarness`] I/O contract. It reads the
/// token-less `AdkTask` JSON on stdin, reads the scoped bearer from
/// `ESCUREL_MCP_BEARER`, and performs the REAL escurel `/mcp` fold
/// (`list_inbox` → `expand` → `update_page` → `assign_event`) over JSON-RPC,
/// then prints a `HarnessOutcome` JSON on stdout.
///
/// This is the deterministic analogue of the datazoo-agent-template's
/// `StaticBrain`: the *tool choice* is scripted, but each `/mcp` call is a
/// real network round-trip to the real gateway. Returns the stub path.
fn write_adk_runner_stub(dir: &std::path::Path) -> std::path::PathBuf {
    let stub = dir.join("adk-runner-stub.sh");
    // jsonrpc helper `rpc <tool> <args-json>` -> echoes the `.result` object.
    // The fold: read the oldest inbox event that carries a target instance,
    // expand it, append a note (preserving the baseline), write it back, then
    // assign_event to mark it processed.
    let script = r#"#!/usr/bin/env bash
set -euo pipefail

# Read the token-less AdkTask the adapter wrote to our stdin.
task="$(cat)"
endpoint="$(printf '%s' "$task" | jq -r '.mcp_endpoint')"
bearer="${ESCUREL_MCP_BEARER:?ESCUREL_MCP_BEARER not set}"

rpc() {
  # $1 = tool name, $2 = arguments JSON
  curl -sS -X POST "$endpoint" \
    -H "authorization: Bearer ${bearer}" \
    -H 'content-type: application/json' \
    -d "$(jq -nc --arg n "$1" --argjson a "$2" \
        '{jsonrpc:"2.0",id:1,method:"tools/call",params:{name:$n,arguments:$a}}')" \
    | jq '.result.structuredContent // .result'
}

# 1. Oldest inbox event with a target instance (list_inbox is newest-first).
inbox="$(rpc list_inbox '{}')"
event="$(printf '%s' "$inbox" \
  | jq -c '[.events[] | select(.instance_page_id != null and .instance_page_id != "")] | last')"
if [ "$event" = "null" ] || [ -z "$event" ]; then
  printf '%s\n' '{"ok":true,"status":"ok","summary":"no inbox event to fold","tool_calls":1,"produced_instance":null}'
  exit 0
fi
event_id="$(printf '%s' "$event" | jq -r '.event_id')"
instance="$(printf '%s' "$event" | jq -r '.instance_page_id')"
title="$(printf '%s' "$event" | jq -r '.title // ""')"

# 2. Expand the instance to append (never clobber) its current body. The full
#    page markdown (frontmatter block + body) is reconstructed by jq in one
#    shot so no trailing newline is lost to command substitution.
expanded="$(rpc expand "$(jq -nc --arg p "$instance" '{page_id:$p}')")"
note="$(printf '\n- adk-runner folded event `%s`: %s\n' "$event_id" "$title")"
content="$(printf '%s' "$expanded" | jq -r --arg note "$note" '
  ( if (.frontmatter | type) == "object" and (.frontmatter | length) > 0
    then "---\n" + ([.frontmatter | to_entries[] | "\(.key): \(.value)"] | join("\n")) + "\n---\n"
    else "" end )
  + ((.body // "") | sub("\n+$"; ""))
  + $note')"

# 3. Write the full page back.
rpc update_page "$(jq -nc --arg p "$instance" --arg c "$content" '{page_id:$p,content:$c}')" >/dev/null

# 4. Mark the event processed + bound to the instance.
rpc assign_event "$(jq -nc --arg e "$event_id" --arg p "$instance" '{event_id:$e,instance_page_id:$p}')" >/dev/null

# Emit the HarnessOutcome the adapter parses.
jq -nc --arg s "adk-runner folded $event_id into $instance" --arg i "$instance" \
  '{ok:true,status:"ok",summary:$s,tool_calls:4,produced_instance:$i}'
"#;
    std::fs::write(&stub, script).expect("write adk runner stub");
    let mut perms = std::fs::metadata(&stub).expect("stat stub").permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&stub, perms).expect("chmod stub");
    stub
}

#[tokio::test]
async fn adk_runner_folds_event_into_instance_end_to_end() {
    // 1. Real gateway with a skill + target instance.
    let gateway = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill(SKILL, SKILL_BODY)
                .instance(SKILL, INSTANCE_ID, INSTANCE_BODY)
                .done(),
        ),
        ..Default::default()
    })
    .await;

    let instance_page_id = format!("markdown/instances/{SKILL}/{INSTANCE_ID}.md");

    // 2. Capture a real inbox event pre-flagged to the target instance.
    let captured = call_mcp(
        &gateway,
        Role::Agent,
        "capture_event",
        json!({
            "source": "manual",
            "mime": "text/plain",
            "label_skill": SKILL,
            "instance_page_id": instance_page_id,
            "title": "renewal request",
            "body": "ADK_FOLD_MARKER customer wants to renew"
        }),
    )
    .await;
    let event_id = captured["event_id"]
        .as_str()
        .expect("capture_event returns an event_id")
        .to_owned();

    // Sanity: the event starts in the inbox (not yet processed).
    let inbox = call_mcp(&gateway, Role::Agent, "list_inbox", json!({})).await;
    assert!(
        inbox["events"]
            .as_array()
            .map(|es| es.iter().any(|e| e["event_id"] == json!(event_id)))
            .unwrap_or(false),
        "seeded event must start in the inbox: {inbox}"
    );

    // 3. Write the real adk-runner stub; spawn the real runner pointed at it.
    let stub_dir = tempfile::tempdir().expect("tempdir");
    let stub = write_adk_runner_stub(stub_dir.path());

    let token = gateway.mint_token(TENANT, Role::Agent);
    let port = free_port();
    let listen = format!("127.0.0.1:{port}");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_escurel-runner"));
    cmd.env("ESCUREL_RUNNER_LISTEN", &listen)
        .env("ESCUREL_RUNNER_GATEWAY_URL", gateway.base_url())
        .env("ESCUREL_RUNNER_TENANT", TENANT)
        .env("ESCUREL_RUNNER_TOKEN", &token)
        .env("ESCUREL_RUNNER_HARNESS", "adk")
        .env(
            "ESCUREL_RUNNER_ADK_BIN",
            stub.to_string_lossy().into_owned(),
        )
        .env("ESCUREL_RUNNER_POLL_INTERVAL", "250ms");
    let _runner = ChildGuard(cmd.spawn().expect("spawn escurel-runner"));

    let http = reqwest::Client::new();
    let ledger_url = format!("http://{listen}/debug/ledger");

    // 4. Wait for the end-to-end effect: the run becomes terminal in the
    //    runner's ledger AND the event is processed on the gateway.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let terminal = http
            .get(&ledger_url)
            .send()
            .await
            .ok()
            .and_then(|r| r.status().is_success().then_some(r));
        let mut ledger_terminal = false;
        if let Some(resp) = terminal {
            let body: Value = resp.json().await.unwrap_or(json!({}));
            ledger_terminal = body["terminal"].as_u64().unwrap_or(0) >= 1;
        }

        if ledger_terminal {
            let events = call_mcp(
                &gateway,
                Role::Agent,
                "list_events",
                json!({ "instance_page_id": instance_page_id }),
            )
            .await;
            let processed = events["events"]
                .as_array()
                .map(|es| {
                    es.iter().any(|e| {
                        e["event_id"] == json!(event_id) && e["status"] == json!("processed")
                    })
                })
                .unwrap_or(false);
            if processed {
                break;
            }
        }

        if Instant::now() >= deadline {
            panic!("adk runner never folded {event_id} into {instance_page_id} within 30s");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // The instance page must have been written by the runner via real `/mcp`:
    // its expanded body now carries the folded event note AND keeps baseline.
    let expanded = call_mcp(
        &gateway,
        Role::Agent,
        "expand",
        json!({ "page_id": instance_page_id }),
    )
    .await;
    let body = expanded["body"].as_str().unwrap_or_default();
    assert!(
        body.contains(&event_id),
        "the instance body must carry the folded event note (event_id): {body}"
    );
    // The note must carry the ADK runner's own marker — this is what proves
    // the fold was done by the adk harness path (via `ESCUREL_RUNNER_HARNESS=
    // adk` → `AdkHarness` → the adk-runner stub), NOT by an echo-harness
    // fallback (whose note has no `adk-runner` marker). Without the `adk`
    // selector arm in `build_harness`, the runner falls back to echo and this
    // assertion fails — so this is the load-bearing red→green discriminator.
    assert!(
        body.contains("adk-runner folded event"),
        "the fold must be performed by the ADK runner path, not an echo fallback: {body}"
    );
    assert!(
        body.contains("BASELINE"),
        "the runner must append, not clobber the baseline content: {body}"
    );
}
