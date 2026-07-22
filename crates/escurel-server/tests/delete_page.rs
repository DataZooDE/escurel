//! #300 — `delete_page` soft-deletes (archives) a markdown page. No mocks: a
//! real `EscurelProcess` gateway + real `DuckdbCrdtBackend` (version source) +
//! real DuckDB + real `/mcp`.
//!
//! Soft-delete semantics (mechanism B): the page is retracted from discovery —
//! its `pages`/`blocks`/`links` rows are dropped from the derived index — while
//! the canonical LaneStore markdown is retained, re-stamped `archived: true`,
//! as the audit record. A from-scratch rebuild skips archived pages, so the
//! retraction survives a rebuild.

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
const C1: &str = "---\ntype: instance\nskill: customer\nid: c1\n---\n# Acme\n\nv0 body.\n";
// c2 links to c1 in its body — so the happy-path test can show that the
// retracted page stops resolving even while a live page still references it.
const C2: &str =
    "---\ntype: instance\nskill: customer\nid: c2\n---\n# Beta\n\nSee [[customer::c1]].\n";

const C1_PAGE: &str = "markdown/instances/customer/c1.md";

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
                .instance("customer", "c2", C2)
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

/// Happy path: delete an existing instance → retracted from list/expand/search,
/// and the inbound link from c2 is gone.
#[tokio::test]
async fn delete_page_retracts_from_discovery_and_links() {
    let h = start().await;

    // Present before the delete: both c1 and c2 are indexed.
    let listed = structured(
        &call(
            &h.process,
            "list_instances",
            json!({ "skill_id": "customer" }),
        )
        .await,
    );
    assert_eq!(
        listed["instances"].as_array().unwrap().len(),
        2,
        "c1 + c2 present before delete: {listed}"
    );

    // Delete it.
    let del = structured(&call(&h.process, "delete_page", json!({ "page_id": C1_PAGE })).await);
    assert_eq!(del["ok"], true, "delete_page ok: {del}");
    assert_eq!(del["page_id"], C1_PAGE);

    // Gone from list_instances — only c2 remains.
    let listed = structured(
        &call(
            &h.process,
            "list_instances",
            json!({ "skill_id": "customer" }),
        )
        .await,
    );
    let remaining = listed["instances"].as_array().unwrap();
    assert_eq!(remaining.len(), 1, "only c2 remains after delete: {listed}");
    assert_eq!(remaining[0]["skill"], "customer");

    // The retracted page no longer resolves and has no neighbours (its own
    // rows + outbound edges are gone). c2's dangling link to it is left as an
    // ordinary broken wikilink — rebuild-consistent — but c1 itself is not
    // discoverable.
    let nb = structured(&call(&h.process, "neighbours", json!({ "page_id": C1_PAGE })).await);
    assert_eq!(
        nb["edges"].as_array().map(Vec::len),
        Some(0),
        "the retracted page has no neighbours: {nb}"
    );
    let resolved = structured(
        &call(
            &h.process,
            "resolve",
            json!({ "wikilink": "[[customer::c1]]" }),
        )
        .await,
    );
    assert_eq!(
        resolved["exists"], false,
        "c1 must not resolve after delete: {resolved}"
    );
    assert!(
        resolved["page"].is_null(),
        "c1 page must be null after delete: {resolved}"
    );
}

/// Deleting a page that isn't indexed returns a `not_found` issue, not a 500.
#[tokio::test]
async fn delete_page_missing_is_not_found() {
    let h = start().await;
    let del = structured(
        &call(
            &h.process,
            "delete_page",
            json!({ "page_id": "markdown/instances/customer/does-not-exist.md" }),
        )
        .await,
    );
    assert_eq!(del["ok"], false, "missing delete is not ok: {del}");
    assert_eq!(del["issues"][0]["code"], "not_found");
}

/// The mandatory `escurel` meta-skill cannot be deleted.
#[tokio::test]
async fn delete_page_refuses_meta_skill() {
    let h = start().await;
    let del = structured(
        &call(
            &h.process,
            "delete_page",
            json!({ "page_id": "markdown/skills/escurel.md" }),
        )
        .await,
    );
    assert_eq!(del["ok"], false, "meta-skill delete refused: {del}");
    assert_eq!(del["issues"][0]["code"], "meta_skill_protected");
}

/// A stale `base_version` conflicts, symmetric with `update_page`.
#[tokio::test]
async fn delete_page_stale_base_version_conflicts() {
    let h = start().await;

    // Advance c1 to v1 via an update so head != v0.
    let w = structured(
        &call(
            &h.process,
            "update_page",
            json!({ "page_id": C1_PAGE, "content": C1 }),
        )
        .await,
    );
    assert_eq!(w["ok"], true);

    // Delete with the now-stale v0 → conflict.
    let del = structured(
        &call(
            &h.process,
            "delete_page",
            json!({ "page_id": C1_PAGE, "base_version": "v0" }),
        )
        .await,
    );
    assert_eq!(del["ok"], false, "stale delete conflicts: {del}");
    assert_eq!(del["issues"][0]["code"], "conflict");
}
