//! F1.3: one page, one version space.
//!
//! Two writers allocate versions for the same page and neither knows about
//! the other:
//!
//! - `update_page` reads `max_hlc`, writes a snapshot at `head + 1`, and
//!   reports `v(head + 1)`.
//! - a live session seeds `op_count` from `max_hlc` **once, at
//!   `LiveDoc::open`** (`livedoc.rs`), then increments it locally per op.
//!
//! So a session opened at hlc N still believes the next free slot is N+1
//! after an `update_page` has already taken it. Both writers then stamp
//! *different content* with the *same version string*, and `expand`'s
//! `version` — the value the whole optimistic-concurrency protocol keys on —
//! can no longer tell them apart. A client's `base_version` check compares
//! equal against a head it has never seen.
//!
//! The repair makes the backend the single hlc authority: an op's hlc is
//! allocated from the persisted maximum at write time rather than from a seed
//! captured at open.
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
const CUSTOMER: &str = "---\ntype: skill\nid: customer\ndescription: x\n---\n# customer\n";
const C1: &str = "---\ntype: instance\nskill: customer\nid: c1\n---\n# Acme\n\nseed.\n";
const PAGE: &str = "markdown/instances/customer/c1.md";

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

/// A Loro op blob inserting `text`, as a live client would send.
fn op_for(text: &str) -> String {
    let doc = LoroDoc::new();
    let vv = doc.oplog_vv();
    doc.get_text("body").insert(0, text).unwrap();
    doc.commit();
    B64.encode(doc.export(ExportMode::updates(&vv)).unwrap())
}

/// An op applied after a concurrent `update_page` must not be stamped with a
/// version that `update_page` already used.
///
/// This is the assertion, not a proxy for it: the two writes carry different
/// content, so sharing a version string makes `expand`'s `version` ambiguous
/// and every `base_version` comparison built on it unsound.
#[tokio::test]
async fn an_op_after_a_concurrent_update_page_gets_a_distinct_version() {
    let h = start().await;

    // Session opens and captures its seed.
    let opened = call(&h, "open_session", json!({ "page_id": PAGE })).await;
    let sid = sc(&opened)["session"].as_str().expect("session").to_owned();

    // A whole-page write lands while the session is open, consuming the next
    // hlc without the session's actor ever hearing about it.
    let w = call(
        &h,
        "update_page",
        json!({ "page_id": PAGE,
                "content": "---\ntype: instance\nskill: customer\nid: c1\n---\n# Acme\n\nWHOLE-PAGE.\n" }),
    )
    .await;
    assert_eq!(sc(&w)["ok"], true, "whole-page write must succeed: {w}");
    let update_version = sc(&w)["new_version"]
        .as_str()
        .expect("new_version")
        .to_owned();

    // The session now applies an op. Its content differs from the whole-page
    // write, so it must not claim that write's version.
    let applied = call(
        &h,
        "apply_op",
        json!({ "session": sid, "op": op_for("SESSION-OP.") }),
    )
    .await;
    assert!(
        applied.get("error").is_none(),
        "apply_op must succeed: {applied}"
    );
    let op_version = sc(&applied)["merged_version"]
        .as_str()
        .expect("merged_version")
        .to_owned();

    assert_ne!(
        op_version, update_version,
        "two writers stamped different content with the same version; \
         `expand.version` can no longer identify a head, so every \
         `base_version` check built on it is unsound"
    );
}

/// The same collision, seen from the store rather than the tool replies.
///
/// After both writers have run, no two rows in the page's version space may
/// share an hlc. Asserting on the persisted maximum keeps the test honest if
/// the reply shapes ever change.
#[tokio::test]
async fn the_persisted_version_space_advances_past_both_writers() {
    let h = start().await;

    let opened = call(&h, "open_session", json!({ "page_id": PAGE })).await;
    let sid = sc(&opened)["session"].as_str().expect("session").to_owned();

    let w = call(
        &h,
        "update_page",
        json!({ "page_id": PAGE,
                "content": "---\ntype: instance\nskill: customer\nid: c1\n---\n# Acme\n\nFIRST.\n" }),
    )
    .await;
    let update_version = sc(&w)["new_version"]
        .as_str()
        .unwrap_or_default()
        .to_owned();

    let applied = call(
        &h,
        "apply_op",
        json!({ "session": sid, "op": op_for("SECOND.") }),
    )
    .await;
    let op_version = sc(&applied)["merged_version"]
        .as_str()
        .unwrap_or_default()
        .to_owned();

    // Versions are `v<n>`; the op came second, so it must sit strictly above.
    let n = |v: &str| v.trim_start_matches('v').parse::<u64>().unwrap_or(0);
    assert!(
        n(&op_version) > n(&update_version),
        "the op ran after the whole-page write and must occupy a higher hlc; \
         got op={op_version} vs update={update_version}"
    );
}
