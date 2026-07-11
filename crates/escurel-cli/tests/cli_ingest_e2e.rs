//! End-to-end test for `escurel ingest` + `escurel page blob`.
//!
//! Real running gateway (real DuckDB indexer + FsStore, seeded with a
//! `document`-backend skill) driving the compiled `escurel` binary.
//! `ingest` uploads a real file through `/ingest/upload`; the document
//! worker materialises a searchable instance; `page blob` fetches the
//! retained original bytes back and we assert they round-trip. No mocks:
//! born-digital text via the offline `PlainTextExtractor`.

use assert_cmd::Command;
use base64::Engine as _;
use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::Value;

const TENANT: &str = "acme";

// A `document`-backend skill that accepts text/plain and chunks small.
const MEMO_SKILL: &str = "---\n\
type: skill\n\
id: memo\n\
description: Text memos ingested as documents.\n\
backend:\n\
\x20 kind: document\n\
\x20 accepts: [text/plain]\n\
\x20 chunk: { max_chars: 40, overlap: 8 }\n\
---\n\
# memo\n";

struct Harness {
    process: EscurelProcess,
    addr: String,
    bearer: String,
}

async fn start() -> Harness {
    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("memo", MEMO_SKILL)
                .done(),
        ),
        config_overrides: Default::default(),
    })
    .await;
    let addr = process
        .base_url()
        .strip_prefix("http://")
        .unwrap()
        .to_owned();
    let bearer = process.mint_token(TENANT, Role::Agent);
    Harness {
        process,
        addr,
        bearer,
    }
}

async fn run(h: &Harness, args: Vec<String>) -> std::process::Output {
    let addr = h.addr.clone();
    let bearer = h.bearer.clone();
    tokio::task::spawn_blocking(move || {
        Command::cargo_bin("escurel")
            .unwrap()
            .env("ESCUREL_SERVER", format!("http://{addr}"))
            .env("ESCUREL_TOKEN", bearer)
            .args(&args)
            .output()
            .unwrap()
    })
    .await
    .unwrap()
}

fn json(out: &std::process::Output) -> Value {
    assert!(
        out.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("stdout is JSON")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ingest_a_file_then_fetch_its_blob_round_trips() {
    let h = start().await;

    let body = "The zephyr proposal covers Q3 logistics across three regions.";
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("memo.txt");
    std::fs::write(&file, body).unwrap();

    // ingest → the document worker materialises a searchable instance.
    // The handling skill is resolved from the MIME (text/plain → memo);
    // pinning it with --skill would trigger the per-skill create-ACL.
    let ingested = json(
        &run(
            &h,
            vec![
                "ingest".into(),
                file.to_string_lossy().into_owned(),
                "--content-type".into(),
                "text/plain".into(),
            ],
        )
        .await,
    );
    assert_eq!(ingested["status"], "materialised", "ingest: {ingested}");
    let page_id = ingested["page_id"].as_str().expect("page_id").to_owned();
    assert!(
        ingested["chunk_count"].as_u64().unwrap_or(0) >= 1,
        "at least one chunk: {ingested}"
    );

    // page blob → the retained original bytes come back, base64 + MIME,
    // nested under `blob`.
    let out = json(&run(&h, vec!["page".into(), "blob".into(), page_id.clone()]).await);
    let blob = &out["blob"];
    assert_eq!(blob["content_type"], "text/plain", "blob mime: {out}");
    let b64 = blob["bytes_base64"]
        .as_str()
        .unwrap_or_else(|| panic!("blob payload missing bytes_base64: {out}"));
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .expect("blob is base64");
    assert_eq!(
        decoded,
        body.as_bytes(),
        "fetched blob must equal the ingested bytes"
    );

    h.process.shutdown().await;
}
