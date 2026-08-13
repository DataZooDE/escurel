//! #246 follow-up — `update_page` CRDT **three-way auto-merge** on a stale
//! `base_version`. No mocks: a real `EscurelProcess` gateway with a real
//! `DuckdbCrdtBackend` (the version + snapshot source) + real DuckDB + real
//! `/mcp`.
//!
//! Auto-merge needs a snapshot at the base the client branched from. Every
//! `update_page` write snapshots the whole page at its version's hlc, so the
//! pristine `v0` (which lives only in the lane, never snapshotted) has no base
//! to merge against — the merge path is exercised from `v1` onward, which is
//! what a real read→edit→write client does anyway.

use std::sync::Arc;

use duckdb::Connection;
use escurel_crdt::{CrdtBackend, DuckdbCrdtBackend};
use escurel_index::Migrator;
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::Mutex;

const TENANT: &str = "acme";
const CUSTOMER: &str = "---\ntype: skill\nid: customer\ndescription: x\n---\n# customer\n";
const C1: &str = "---\ntype: instance\nskill: customer\nid: c1\n---\n# Acme\n\nseed.\n";

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

async fn call(p: &EscurelProcess, name: &str, args: Value) -> Value {
    let token = p.mint_token(TENANT, Role::Agent);
    reqwest::Client::new()
        .post(p.mcp_url())
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

fn structured(env: &Value) -> Value {
    env["result"]["structuredContent"].clone()
}

const C1_PAGE: &str = "markdown/instances/customer/c1.md";

/// A page with a `stage` frontmatter field and two independently-editable
/// paragraphs (`p1` / `p3`) around a fixed middle.
fn page(stage: &str, p1: &str, p3: &str) -> String {
    format!(
        "---\ntype: instance\nskill: customer\nid: c1\nstage: {stage}\n---\n# Acme\n\n{p1}\n\nMiddle stays put.\n\n{p3}\n"
    )
}

async fn write(p: &EscurelProcess, content: &str, base: Option<&str>) -> Value {
    let mut args = json!({ "page_id": C1_PAGE, "content": content });
    if let Some(b) = base {
        args["base_version"] = json!(b);
    }
    structured(&call(p, "update_page", args).await)
}

async fn read_version(p: &EscurelProcess) -> String {
    structured(&call(p, "expand", json!({ "page_id": C1_PAGE })).await)["version"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

async fn read_content(p: &EscurelProcess) -> String {
    // `expand` returns the rendered page; assert against its content field.
    let s = structured(&call(p, "expand", json!({ "page_id": C1_PAGE })).await);
    s["content"]
        .as_str()
        .or_else(|| s["body"].as_str())
        .unwrap_or_default()
        .to_owned()
}

#[tokio::test]
async fn disjoint_concurrent_edits_auto_merge_and_both_survive() {
    let h = start().await;
    let p = &h.process;

    // Seed a v1 snapshot (base for the concurrent branches).
    let seed = page("draft", "Alpha.", "Gamma.");
    let w1 = write(p, &seed, None).await;
    assert_eq!(w1["ok"], true);
    assert_eq!(w1["new_version"], "v1");

    // "Server" branch: edit the FIRST paragraph → v2 (head advances).
    let server = page("draft", "Alpha SERVER-EDIT.", "Gamma.");
    let w2 = write(p, &server, None).await;
    assert_eq!(w2["ok"], true);
    assert_eq!(w2["new_version"], "v2");

    // "Client" branch: branched from v1 (now stale — head is v2), edits the
    // LAST paragraph. Disjoint from the server edit → clean CRDT auto-merge.
    let client = page("draft", "Alpha.", "Gamma CLIENT-EDIT.");
    let w3 = write(p, &client, Some("v1")).await;
    assert_eq!(w3["ok"], true, "stale but mergeable → ok: {w3}");
    assert_eq!(w3["auto_merged"], true, "flagged as auto-merged: {w3}");
    assert_eq!(w3["new_version"], "v3");

    // Both concurrent edits survive in the persisted page.
    let content = read_content(p).await;
    assert!(
        content.contains("Alpha SERVER-EDIT."),
        "server edit kept: {content}"
    );
    assert!(
        content.contains("Gamma CLIENT-EDIT."),
        "client edit kept: {content}"
    );
    assert!(
        content.contains("Middle stays put."),
        "untouched middle kept: {content}"
    );
    assert_eq!(read_version(p).await, "v3");

    h.process.shutdown().await;
}

#[tokio::test]
async fn conflicting_frontmatter_edits_fall_back_to_conflict() {
    let h = start().await;
    let p = &h.process;

    // Seed v1.
    let w1 = write(p, &page("draft", "Alpha.", "Gamma."), None).await;
    assert_eq!(w1["new_version"], "v1");

    // Server moves `stage: draft -> review` → v2.
    let w2 = write(p, &page("review", "Alpha.", "Gamma."), None).await;
    assert_eq!(w2["new_version"], "v2");

    // Client (branched from v1) moves the SAME field `draft -> archived`.
    // Both sides changed the same frontmatter key differently → the merge
    // can't keep either side's frontmatter intact → conflict, not a
    // silently-garbled stage.
    let w3 = write(p, &page("archived", "Alpha.", "Gamma."), Some("v1")).await;
    assert_eq!(
        w3["ok"], false,
        "both-sided frontmatter change → conflict: {w3}"
    );
    assert_eq!(w3["issues"][0]["code"], "conflict");
    assert!(
        w3["head_content"]
            .as_str()
            .is_some_and(|c| c.contains("stage: review")),
        "conflict carries head_content for re-draft: {w3}"
    );
    // Head is untouched by the rejected write.
    assert_eq!(read_version(p).await, "v2");

    h.process.shutdown().await;
}
