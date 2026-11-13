//! The run transcript (#822): what a run's agent did — each tool call, its
//! result, its messages and errors — persisted and replayable after the
//! harness has exited.
//!
//! Real gateway (event ACL enforced), real runner binary in minted mode, real
//! echo harness emitting a scripted stream (`ESCUREL_ECHO_TRANSCRIPT`). The
//! stream carries a fake bearer JWT, an `sk-` key and the run's own token;
//! none of them may reach the stored bytes.

use std::time::Duration;

use base64::Engine as _;
use escurel_test_support::{
    AuthMode, ConfigOverrides, EscurelProcess, EventAclMode, FixtureBuilder, Opts, Role,
};
use serde_json::{Value, json};

use crate::support::{RunnerProcess, body_of, call};

const TENANT: &str = "acme";
const ALICE: &str = "consultant:alice";
const BOB: &str = "consultant:bob";
const SKILL_BODY: &str = "---\nkind: skill\nid: renewal\nautonomy: auto\nvisibility: owner\n\
    owner_field: owner\n---\n# renewal\n\nFold the event into the instance.\n";
const INSTANCE_BODY: &str = "---\nkind: instance\nid: c1\nskill: renewal\n\
    owner: \"consultant:alice\"\n---\n# C1\n\nBASELINE.\n";
const PAGE: &str = "markdown/instances/renewal/c1.md";
const FAKE_JWT: &str = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJtYWxsb3J5In0.FAKESIGNATURE";
const FAKE_KEY: &str = "sk-ant-api03-FAKEFAKEFAKEFAKEFAKE";

async fn gateway() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            event_acl: Some(EventAclMode::Enforce),
            ..Default::default()
        },
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("renewal", SKILL_BODY)
                .instance("renewal", "c1", INSTANCE_BODY)
                .done(),
        ),
    })
    .await
}

/// The scripted stream: a tool call whose args AND summary carry a fake
/// bearer, its result echoing the run's own token, a message, an error.
fn script() -> String {
    [
        json!({ "transcript": { "kind": "tool_call", "name": "update_page",
            "args": { "page_id": PAGE, "headers": { "authorization": format!("Bearer {FAKE_JWT}") } },
            "text": format!("calling with Bearer {FAKE_JWT}") } }),
        json!({ "transcript": { "kind": "tool_result", "name": "update_page",
            "text": "ok, saw {token} in the header" } }),
        json!({ "transcript": { "kind": "message", "text": "folded the renewal" } }),
        json!({ "transcript": { "kind": "error", "text": format!("upstream 401 for key {FAKE_KEY}") } }),
    ]
    .iter()
    .map(Value::to_string)
    .collect::<Vec<_>>()
    .join("\n")
}

/// `tools/call` as `token`, returning the whole JSON-RPC body (errors kept).
async fn rpc(gw: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    reqwest::Client::new()
        .post(gw.mcp_url())
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

fn decoded(fetched: &Value) -> String {
    let b64 = fetched["blob"]["bytes_base64"].as_str().expect("bytes");
    String::from_utf8(
        base64::engine::general_purpose::STANDARD
            .decode(b64)
            .expect("base64"),
    )
    .expect("utf-8")
}

#[tokio::test]
async fn a_finished_run_carries_a_redacted_transcript_its_readers_can_replay() {
    let gw = gateway().await;
    let dir = tempfile::tempdir().unwrap();
    let script_path = dir.path().join("script.jsonl");
    std::fs::write(&script_path, script()).unwrap();
    let admin = gw.mint_token(TENANT, Role::Admin);
    let runner = RunnerProcess::echo(&gw, TENANT)
        .minted()
        .env("ESCUREL_ECHO_TRANSCRIPT", script_path.display().to_string())
        .spawn();

    let captured = call(
        &gw,
        &admin,
        "capture_event",
        json!({ "source": "manual", "mime": "text/plain", "label_skill": "renewal",
                "instance_page_id": PAGE, "title": "renew", "body": "please renew" }),
    )
    .await;
    let event_id = captured["event_id"].as_str().unwrap().to_owned();
    let run = runner
        .wait_for_status(&event_id, &["processed"], Duration::from_secs(60))
        .await;
    let run_id = run["run_id"].as_str().unwrap().to_owned();

    // run-finished names the stored transcript. The ledger reaches its
    // terminal first; the event (and the upload before it) follows.
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let finished = loop {
        let events = call(&gw, &admin, "list_events", json!({ "run_id": run_id })).await;
        if let Some(e) = events["events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["title"] == "run-finished")
        {
            break e.clone();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no run-finished: {events}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let transcript = body_of(&finished)["transcript"].clone();
    assert!(
        transcript["blob_id"]
            .as_str()
            .is_some_and(|b| b.starts_with("sha256:")),
        "{transcript}"
    );
    assert_eq!(transcript["records"], 4, "{transcript}");

    // The page's owner replays it: the records in order, digests not args.
    let alice = gw.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let as_alice = call(&gw, &alice, "fetch_blob", json!({ "run_id": run_id })).await;
    assert_eq!(as_alice["blob"]["run_id"], run_id.as_str());
    assert_eq!(as_alice["blob"]["content_type"], "application/x-ndjson");
    let jsonl = decoded(&as_alice);
    assert_eq!(transcript["bytes"], jsonl.len(), "{transcript}");
    let records: Vec<Value> = jsonl
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let shape: Vec<(String, Option<String>)> = records
        .iter()
        .map(|r| {
            (
                r["kind"].as_str().unwrap().to_owned(),
                r["name"].as_str().map(str::to_owned),
            )
        })
        .collect();
    assert_eq!(
        shape,
        [
            ("tool_call".to_owned(), Some("update_page".to_owned())),
            ("tool_result".to_owned(), Some("update_page".to_owned())),
            ("message".to_owned(), None),
            ("error".to_owned(), None),
        ],
        "{jsonl}"
    );
    for r in &records {
        assert_eq!(r["attempt"], 1, "{r}");
        assert!(r["ts"].as_str().is_some_and(|t| t.ends_with('Z')), "{r}");
    }
    assert!(
        records[0]["args_digest"]
            .as_str()
            .is_some_and(|d| d.starts_with("sha256:")),
        "{}",
        records[0]
    );
    assert!(records[0].get("args").is_none(), "raw args never stored");
    assert_eq!(records[0]["summary"], "calling with Bearer [redacted]");
    assert_eq!(
        records[1]["summary"], "ok, saw [redacted] in the header",
        "the run's own bearer is scrubbed"
    );
    assert_eq!(records[2]["summary"], "folded the renewal");
    for secret in ["eyJhbGciOi", "FAKESIGNATURE", "sk-ant", "FAKEFAKE"] {
        assert!(!jsonl.contains(secret), "`{secret}` leaked: {jsonl}");
    }

    // An admin reads the same bytes.
    let as_admin = call(&gw, &admin, "fetch_blob", json!({ "run_id": run_id })).await;
    assert_eq!(decoded(&as_admin), jsonl);

    // A stranger who may not read the run's events is refused.
    let bob = gw.mint_token_with_sub(TENANT, Role::Agent, BOB);
    let refused = rpc(&gw, &bob, "fetch_blob", json!({ "run_id": run_id })).await;
    assert_eq!(
        refused["error"]["data"]["code"], "run_not_found",
        "{refused}"
    );

    // Only the run's own bearer may store its transcript: one for another
    // run is refused, and so is a plain agent bearer.
    let other_run =
        gw.mint_token_for_run(TENANT, Role::Agent, "agent:renewal", "OTHERRUN", &event_id);
    for token in [other_run, alice] {
        let put = rpc(
            &gw,
            &token,
            "put_run_transcript",
            json!({ "run_id": run_id, "records": "{\"kind\":\"message\"}\n" }),
        )
        .await;
        assert_eq!(
            put["error"]["data"]["code"], "transcript_not_your_run",
            "{put}"
        );
    }
    let still = call(&gw, &admin, "fetch_blob", json!({ "run_id": run_id })).await;
    assert_eq!(decoded(&still), jsonl, "a refused put changed nothing");
}
