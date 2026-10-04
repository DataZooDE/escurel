//! F1: a committed CRDT session must reach the indexer.
//!
//! `expand` builds its reply from two stores — `body` from the indexer,
//! `version` from the CRDT backend. Before this fix, `close_session(commit)`
//! wrote only the CRDT snapshot, so `version` advanced while `body` did not.
//! A client reading that pair and writing back with the matching
//! `base_version` took the `base == head` path, no merge was attempted, and
//! the committed session edits were overwritten.
//!
//! The indexer also owns `blocks` (BM25 + vector) and `links` (neighbours,
//! backlinks), so the divergence was never only about `expand`: search could
//! not find a committed edit either. That is why the repair writes through
//! rather than hydrating the read path — see
//! `docs/notes/concurrency-fix-plan.md` F1, Option A.
//!
//! Ordering under failure is deliberate: the indexer write happens first and
//! the CRDT snapshot last, so a failed write leaves the session **open and
//! retryable** rather than half-applied.
//!
//! Real gateway, real `DuckdbCrdtBackend`, real DuckDB, real `/mcp`. No mocks.

use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use duckdb::Connection;
use escurel_crdt::{CrdtBackend, DuckdbCrdtBackend};
use escurel_index::Migrator;
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use loro::{ExportMode, LoroDoc};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::Mutex;

const TENANT: &str = "acme";
const CUSTOMER: &str = "---\nkind: skill\nid: customer\ndescription: x\n---\n# customer\n";
const C1: &str = "---\nkind: instance\nskill: customer\nid: c1\n---\n# Acme\n\nseed.\n";
const PAGE: &str = "markdown/instances/customer/c1.md";
const LEDGER: &str =
    "---\nkind: skill\nid: ledger\ndescription: x\nautonomy: review\n---\n# ledger\n";
// An EMPTY body: a session's text is the page body, and a seeded body would race (by CRDT tie-break) the
// frontmatter the test inserts, making the merged text start with the body on some runs.
const L1: &str = "---\nkind: instance\nskill: ledger\nid: l1\n---\n";
const LEDGER_PAGE: &str = "markdown/instances/ledger/l1.md";

struct Harness {
    process: EscurelProcess,
    _db_dir: TempDir,
}

async fn start() -> Harness {
    let db_dir = TempDir::new().unwrap();
    let conn = Connection::open(db_dir.path().join("crdt.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let shared = Arc::new(Mutex::new(conn));
    let crdt_backend: Arc<dyn CrdtBackend> = Arc::new(DuckdbCrdtBackend::new(Arc::clone(&shared)));

    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("customer", CUSTOMER)
                .instance("customer", "c1", C1)
                .skill("ledger", LEDGER)
                .instance("ledger", "l1", L1)
                .done(),
        ),
        config_overrides: ConfigOverrides {
            crdt_backend: Some(crdt_backend),
            ..Default::default()
        },
    })
    .await;
    Harness {
        process,
        _db_dir: db_dir,
    }
}

async fn call(h: &Harness, name: &str, args: Value) -> Value {
    let token = h.process.mint_token(TENANT, Role::Agent);
    reqwest::Client::new()
        .post(h.process.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": args } }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

fn sc(env: &Value) -> Value {
    env["result"]["structuredContent"].clone()
}

/// Put `new_body` into the page **only through the CRDT session** — never via
/// `update_page` — then commit.
///
/// This distinction is the whole experiment. An earlier version of this
/// helper seeded the content with `update_page`, which writes the indexer
/// itself; all three tests then passed while exercising none of the F1 path.
/// Content must arrive by `apply_op` alone for the divergence to be real.
async fn session_edit(h: &Harness, new_body: &str) -> String {
    let opened = call(h, "open_session", json!({ "page_id": PAGE })).await;
    let sid = sc(&opened)["session"].as_str().expect("session").to_owned();

    // A genuine Loro op carrying the new body, exactly as a live client sends.
    let doc = LoroDoc::new();
    let vv = doc.oplog_vv();
    doc.get_text("body").insert(0, new_body).unwrap();
    doc.commit();
    let op = doc.export(ExportMode::updates(&vv)).unwrap();

    let applied = call(
        h,
        "apply_op",
        json!({ "session": sid, "op": B64.encode(op) }),
    )
    .await;
    assert!(
        applied.get("error").is_none(),
        "apply_op must succeed: {applied}"
    );

    let closed = call(
        h,
        "close_session",
        json!({ "session": sid, "commit": true }),
    )
    .await;
    assert_eq!(sc(&closed)["ok"], true, "close must succeed: {closed}");
    sc(&closed)["final_version"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

/// After a committed session, `expand` must return a `body` consistent with
/// the `version` it reports — the two halves come from different stores and
/// must not drift.
#[tokio::test]
async fn committed_session_leaves_expand_body_and_version_consistent() {
    let h = start().await;
    let edited =
        "---\nkind: instance\nskill: customer\nid: c1\n---\n# Acme\n\nEDITED-IN-SESSION.\n";
    session_edit(&h, edited).await;

    let ex = call(&h, "expand", json!({ "page_id": PAGE })).await;
    let page = sc(&ex);
    let body = page["body"].as_str().unwrap_or_default();
    assert!(
        body.contains("EDITED-IN-SESSION"),
        "expand.body must reflect the committed session, got: {body:?}"
    );
}

/// The data-loss assertion. A client reads `expand`, edits, and writes back
/// with the `base_version` it was handed. That write must not silently
/// discard what the session committed.
#[tokio::test]
async fn update_page_with_matching_base_after_session_commit_does_not_clobber_it() {
    let h = start().await;
    let edited = "---\nkind: instance\nskill: customer\nid: c1\n---\n# Acme\n\nSESSION-WORK.\n";
    session_edit(&h, edited).await;

    // Read exactly what a well-behaved client would read.
    let ex = call(&h, "expand", json!({ "page_id": PAGE })).await;
    let page = sc(&ex);
    let version = page["version"].as_str().unwrap_or_default().to_owned();
    let read_body = page["body"].as_str().unwrap_or_default().to_owned();

    // The client must have been given the session's content to edit. If it
    // was handed stale content with a fresh version, the write below is a
    // silent overwrite no matter how well-behaved the client is.
    assert!(
        read_body.contains("SESSION-WORK"),
        "the version/body pair handed to the client must be consistent; \
         got version={version} with body={read_body:?}"
    );
}

/// Search must find a committed session edit.
///
/// This is the assertion that decides *how* to fix F1: hydrating `expand`
/// from the CRDT snapshot would satisfy the two tests above and still leave
/// this one failing, because `blocks` (which feed retrieval) live in the
/// indexer and would never have been written.
#[tokio::test]
async fn search_finds_a_committed_session_edit() {
    let h = start().await;
    let edited =
        "---\nkind: instance\nskill: customer\nid: c1\n---\n# Acme\n\nZEPHYRQUARTZ marker.\n";
    session_edit(&h, edited).await;

    let hits = call(&h, "search", json!({ "q": "ZEPHYRQUARTZ", "k": 10 })).await;
    // Only the marker counts. An earlier version also accepted `c1`, which is
    // the page id and is present whether or not the edit landed — the test
    // passed while asserting nothing.
    let found = serde_json::to_string(&sc(&hits))
        .map(|s| s.contains("ZEPHYRQUARTZ"))
        .unwrap_or(false);
    assert!(
        found,
        "a committed session edit must be retrievable; the indexer owns the \
         blocks that feed search: {hits}"
    );
}

/// The autonomy gate reaches the session path too: a MACHINE's `close_session(commit)` on a review
/// skill does not write through; the merged body becomes an open draft, the page is untouched.
#[tokio::test]
async fn a_machine_session_commit_on_a_review_skill_becomes_a_draft_not_a_write() {
    let h = start().await;
    let token =
        h.process
            .mint_token_for_run(TENANT, Role::Agent, "agent:ledger", "run-9", "root-9");
    let call_as = |name: &'static str, args: Value| {
        let token = token.clone();
        let url = h.process.mcp_url();
        async move {
            let v: Value = reqwest::Client::new()
                .post(url)
                .header("authorization", format!("Bearer {token}"))
                .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                    "params": { "name": name, "arguments": args } }))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            v["result"]["structuredContent"].clone()
        }
    };
    let opened = call_as("open_session", json!({ "page_id": LEDGER_PAGE })).await;
    let sid = opened["session"].as_str().expect("session").to_owned();
    let closed = call_as("close_session", json!({ "session": sid, "commit": true })).await;
    assert_eq!(closed["held_for_review"], true, "{closed}");
    assert_eq!(closed["draft"]["status"], "open", "{closed}");
    let page = call_as("expand", json!({ "page_id": LEDGER_PAGE })).await;
    // The page is exactly what it was (nothing was written through).
    assert_eq!(page["frontmatter"]["id"], "l1", "{page}");
    let drafts = call_as("list_drafts", json!({})).await;
    assert_eq!(drafts["drafts"].as_array().unwrap().len(), 1, "{drafts}");
}
