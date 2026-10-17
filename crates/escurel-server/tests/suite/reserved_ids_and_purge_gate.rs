//! Two gaps the 2026-10-17 security pass found at the doors that are NOT the MCP `capture_event`:
//!
//! * `/ingest` forwarded a caller's `event_id` to `capture_event` without the reserved-prefix check, so a
//!   member could squat `runner-status:<runner>` (hiding a runner's health behind an inbox row) or
//!   `write-back:<draft>:applied` (a forged witness: the next promote skips the upstream write).
//! * `admin_purge_events` received no caller, so a runner-minted ADMIN run token (every harness gets one)
//!   could wipe the write-back witnesses and the purge's own audit row.
//!
//! Real gateway, real DuckDB, real verifier, raw JSON-RPC and reqwest. No mocks.

use std::sync::Arc;

use bytes::Bytes;
use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, LaneStore};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, Opts, Role};
use reqwest::StatusCode;
use serde_json::{Value, json};
use tempfile::TempDir;

const TENANT: &str = "acme";
const MEMO_SKILL: &str = "\
---
kind: skill
id: memo
description: Text memos ingested as documents.
backend:
  kind: document
  accepts: [text/plain]
  chunk: { max_chars: 800, overlap: 100 }
  retrieval: duckdb
---
# memo
";

struct Setup {
    process: EscurelProcess,
    indexer: Arc<Indexer>,
    store: Arc<dyn LaneStore>,
    _dirs: Vec<TempDir>,
}

async fn setup() -> Setup {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Arc::new(Indexer::new(Arc::clone(&store), embedder, conn, TENANT).unwrap());
    indexer
        .update_page("markdown/skills/memo.md", MEMO_SKILL)
        .await
        .unwrap();
    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            indexer: Some(Arc::clone(&indexer)),
            ..Default::default()
        },
        ..Default::default()
    })
    .await;
    Setup {
        process,
        indexer,
        store,
        _dirs: vec![store_dir, db_dir],
    }
}

async fn deposit(s: &Setup, body: &'static str) -> String {
    s.store
        .put_inbox_blob(TENANT, Bytes::from(body.as_bytes().to_vec()), None)
        .await
        .unwrap()
        .as_str()
        .to_owned()
}

async fn post_ingest(
    p: &EscurelProcess,
    token: &str,
    blob: &str,
    event_id: &str,
) -> (StatusCode, Value) {
    let resp = reqwest::Client::new()
        .post(format!("{}/ingest", p.base_url()))
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({ "blob_id": blob, "content_type": "text/plain", "event_id": event_id }))
        .send()
        .await
        .unwrap();
    let status = resp.status();
    (status, resp.json().await.unwrap_or_default())
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

#[tokio::test]
async fn ingest_refuses_the_gateway_s_reserved_event_ids_and_still_takes_an_ordinary_one() {
    let s = setup().await;
    let person = s.process.mint_token(TENANT, Role::Agent);
    for (n, reserved) in [
        "runner-status:r9",
        "runner-status:",
        "write-back:draft-1:applied",
        "write-back:draft-1:1:applying",
    ]
    .into_iter()
    .enumerate()
    {
        let blob = deposit(&s, "a memo").await;
        let (st, body) = post_ingest(&s.process, &person, &blob, reserved).await;
        assert!(
            st.is_client_error(),
            "#{n} `{reserved}` must be refused, got {st}: {body}"
        );
        assert!(
            body.to_string().contains("reserved"),
            "#{n} `{reserved}`: the refusal says why: {body}"
        );
        assert!(
            s.indexer.get_event(reserved).await.unwrap().is_none(),
            "#{n} `{reserved}` must not exist as an event"
        );
    }

    // An ordinary caller-chosen id is still an idempotency key that works.
    let blob = deposit(&s, "another memo").await;
    let (st, body) = post_ingest(&s.process, &person, &blob, "my-own-key-1").await;
    assert_eq!(st, StatusCode::ACCEPTED, "{body}");
    assert!(
        s.indexer.get_event("my-own-key-1").await.unwrap().is_some(),
        "{body}"
    );
    s.process.shutdown().await;
}

async fn seed_witness(s: &Setup) {
    // A write-back witness row, as the gateway files it, under the protected label.
    let admin = s.process.mint_token(TENANT, Role::Admin);
    for n in 0..3 {
        let r = rpc(
            &s.process,
            &admin,
            "capture_event",
            json!({ "kind": "system", "source": "escurel-runner", "mime": "application/json",
                    "label_skill": "escurel:runner-status", "title": "heartbeat",
                    "at": format!("2026-09-{:02}T10:00:00Z", n + 1), "body": "{}" }),
        )
        .await;
        assert!(r.get("error").is_none(), "{r}");
    }
}

async fn status_rows(s: &Setup) -> usize {
    let admin = s.process.mint_token(TENANT, Role::Admin);
    let r = rpc(
        &s.process,
        &admin,
        "list_events",
        json!({ "label_skill": "escurel:runner-status", "limit": 1000, "include_system": true }),
    )
    .await;
    r["result"]["structuredContent"]["events"]
        .as_array()
        .unwrap()
        .len()
}

#[tokio::test]
async fn a_runner_minted_admin_token_cannot_purge_events_a_person_still_can() {
    let s = setup().await;
    seed_witness(&s).await;
    assert_eq!(status_rows(&s).await, 3);

    // The runner mints its harnesses ADMIN run tokens: they carry a run id, so they are machines.
    let machine =
        s.process
            .mint_token_for_run(TENANT, Role::Admin, "agent:harness", "run-1", "root-1");
    let apply = json!({ "label_skill": "escurel:runner-status", "keep_latest": 0, "apply": true });
    let r = rpc(&s.process, &machine, "admin_purge_events", apply.clone()).await;
    assert!(
        r["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("machine caller")),
        "a machine's applied purge is refused: {r}"
    );
    // Protected labels with allow_protected, even as a dry run, are a machine's no-go as well.
    let r = rpc(
        &s.process,
        &machine,
        "admin_purge_events",
        json!({ "label_skill": "escurel:write-back", "keep_latest": 0, "allow_protected": true }),
    )
    .await;
    assert!(
        r.get("error").is_some(),
        "a machine may not reach for the audit trail: {r}"
    );
    assert_eq!(
        status_rows(&s).await,
        3,
        "nothing was deleted by the refused calls"
    );

    // A person's admin token (no run / actor / skill claims) purges as before.
    let admin = s.process.mint_token(TENANT, Role::Admin);
    let r = rpc(&s.process, &admin, "admin_purge_events", apply).await;
    assert!(r.get("error").is_none(), "{r}");
    assert_eq!(status_rows(&s).await, 0);
    s.process.shutdown().await;
}
