//! Run transcripts (#822) at the gateway: `put_run_transcript` with a run's
//! own bearer, `fetch_blob{run_id}` through the run's `run-finished` event,
//! and the retention sweep (`ESCUREL_RUN_TRANSCRIPT_RETENTION_DAYS`) that
//! deletes them. The sweep runs on the minted-run tick; the test shortens
//! both the tick and the retention so a transcript expires in seconds.

use std::time::{Duration, Instant};

use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "carl";
const RUN: &str = "01HRUNTRANSCRIPT0000000000";
const KEPT: &str = "01HRUNTRANSCRIPTKEPT000000";
const ROOT: &str = "01HROOTTRANSCRIPT000000000";
const SKILL: &str = "---\nkind: skill\nid: note\ndescription: d.\n---\n# note\n";

async fn start(retention: Option<Duration>) -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            minted_run_sweep: Some(Duration::from_millis(200)),
            run_transcript_retention: retention,
            ..Default::default()
        },
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("note", SKILL)
                .done(),
        ),
    })
    .await
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

/// Store `records` as `run`'s transcript with its own bearer, then file the
/// `run-finished` that names it, as the runner does.
async fn finish_with_transcript(p: &EscurelProcess, run: &str, records: &str) -> Value {
    let own = p.mint_token_for_run(TENANT, Role::Agent, "agent:note", run, ROOT);
    let stored = call(
        p,
        &own,
        "put_run_transcript",
        json!({ "run_id": run, "records": records }),
    )
    .await;
    let admin = p.mint_token(TENANT, Role::Admin);
    call(
        p,
        &admin,
        "capture_event",
        json!({ "event_id": format!("run:{run}:finished"), "kind": "system",
                "source": "escurel-runner", "mime": "application/json",
                "label_skill": "escurel:run", "title": "run-finished",
                "body": json!({ "status": "processed", "transcript": stored }).to_string(),
                "provenance": { "runner": { "run_id": run, "root_event_id": ROOT } } }),
    )
    .await;
    stored
}

#[tokio::test]
async fn a_transcript_past_its_retention_is_swept_and_then_answers_expired() {
    let p = start(Some(Duration::from_secs(1))).await;
    let admin = p.mint_token(TENANT, Role::Admin);
    let stored =
        finish_with_transcript(&p, RUN, "{\"kind\":\"message\",\"summary\":\"hi\"}\n").await;
    assert_eq!(stored["records"], 1, "{stored}");

    let fetched = call(&p, &admin, "fetch_blob", json!({ "run_id": RUN })).await;
    assert_eq!(fetched["blob"]["size"], stored["bytes"], "{fetched}");

    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let r = rpc(&p, &admin, "fetch_blob", json!({ "run_id": RUN })).await;
        if r["error"]["data"]["code"] == "transcript_expired" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the sweep never removed the transcript: {r}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test]
async fn transcripts_are_kept_forever_without_a_retention() {
    let p = start(None).await;
    let admin = p.mint_token(TENANT, Role::Admin);
    finish_with_transcript(&p, KEPT, "{\"kind\":\"message\"}\n").await;
    // Several sweep ticks.
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let fetched = call(&p, &admin, "fetch_blob", json!({ "run_id": KEPT })).await;
    assert!(fetched["blob"]["bytes_base64"].is_string(), "{fetched}");
}

#[tokio::test]
async fn fetch_blob_names_one_target_and_says_why_a_run_has_no_transcript() {
    let p = start(None).await;
    let admin = p.mint_token(TENANT, Role::Admin);

    for args in [
        json!({}),
        json!({ "run_id": RUN, "page_id": "markdown/skills/note.md" }),
    ] {
        let r = rpc(&p, &admin, "fetch_blob", args.clone()).await;
        assert_eq!(r["error"]["code"], -32602, "{args}: {r}");
    }
    let unknown = rpc(&p, &admin, "fetch_blob", json!({ "run_id": "nope" })).await;
    assert_eq!(
        unknown["error"]["data"]["code"], "run_not_found",
        "{unknown}"
    );

    call(
        &p,
        &admin,
        "capture_event",
        json!({ "event_id": format!("run:{RUN}:finished"), "kind": "system",
                "source": "escurel-runner", "mime": "application/json",
                "label_skill": "escurel:run", "title": "run-finished",
                "body": json!({ "status": "processed", "transcript": null }).to_string() }),
    )
    .await;
    let absent = rpc(&p, &admin, "fetch_blob", json!({ "run_id": RUN })).await;
    assert_eq!(
        absent["error"]["data"]["code"], "transcript_absent",
        "{absent}"
    );

    // Not JSONL, or over the cap: refused before anything is stored.
    let own = p.mint_token_for_run(TENANT, Role::Agent, "agent:note", RUN, ROOT);
    let bad = rpc(
        &p,
        &own,
        "put_run_transcript",
        json!({ "run_id": RUN, "records": "not json\n" }),
    )
    .await;
    assert_eq!(
        bad["error"]["data"]["code"], "transcript_malformed",
        "{bad}"
    );
    let big = "x".repeat(2 * 1024 * 1024 + 1);
    let over = rpc(
        &p,
        &own,
        "put_run_transcript",
        json!({ "run_id": RUN, "records": big }),
    )
    .await;
    assert_eq!(
        over["error"]["data"]["code"], "transcript_too_large",
        "{over}"
    );
    // A runner on a static admin bearer (no run claim) may store any run's.
    let stored = call(
        &p,
        &admin,
        "put_run_transcript",
        json!({ "run_id": RUN, "records": "{\"kind\":\"message\"}\n" }),
    )
    .await;
    assert_eq!(stored["records"], 1, "{stored}");
}

/// `fetch_blob{run_id}` reads the run's OWN transcript key, never a blob id the `run-finished` body
/// names (crew review PR-1, F6/F3): a body pointing at a document's blob serves nothing of it. And a
/// transcript is written once: a second, different upload for the run is `transcript_exists`.
#[tokio::test]
async fn a_run_finished_body_cannot_point_fetch_blob_at_a_document_and_a_transcript_is_written_once()
 {
    let p = start(None).await;
    let admin = p.mint_token(TENANT, Role::Admin);
    // Any blob id at all: the body is informational, so it never reaches the store.
    let document_blob = format!("sha256:{}", "ab".repeat(32));
    call(
        &p,
        &admin,
        "capture_event",
        json!({ "event_id": format!("run:{RUN}:finished"), "kind": "system",
                "source": "escurel-runner", "mime": "application/json",
                "label_skill": "escurel:run", "title": "run-finished",
                "body": json!({ "status": "processed",
                                "transcript": { "blob_id": document_blob, "records": 1, "bytes": 3 } })
                    .to_string() }),
    )
    .await;
    let r = rpc(&p, &admin, "fetch_blob", json!({ "run_id": RUN })).await;
    assert_eq!(r["error"]["data"]["code"], "transcript_expired", "{r}");

    let own = p.mint_token_for_run(TENANT, Role::Agent, "agent:note", RUN, ROOT);
    let first = "{\"kind\":\"message\"}\n";
    call(
        &p,
        &own,
        "put_run_transcript",
        json!({ "run_id": RUN, "records": first }),
    )
    .await;
    // The same bytes again: a no-op.
    call(
        &p,
        &own,
        "put_run_transcript",
        json!({ "run_id": RUN, "records": first }),
    )
    .await;
    let other = rpc(
        &p,
        &admin,
        "put_run_transcript",
        json!({ "run_id": RUN, "records": "{\"kind\":\"forged\"}\n" }),
    )
    .await;
    assert_eq!(
        other["error"]["data"]["code"], "transcript_exists",
        "{other}"
    );
    let fetched = call(&p, &admin, "fetch_blob", json!({ "run_id": RUN })).await;
    assert_eq!(fetched["blob"]["size"], first.len(), "{fetched}");
}
