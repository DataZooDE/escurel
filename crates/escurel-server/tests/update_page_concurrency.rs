//! #246 — `update_page` optimistic concurrency + monotonic versions + the
//! opt-in `page-edited` eager-improvement event. No mocks: a real
//! `EscurelProcess` gateway with a real `DuckdbCrdtBackend` (the version
//! source) + real DuckDB + real `/mcp`.

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

struct Harness {
    process: EscurelProcess,
    _db_dir: TempDir,
}

async fn start(emit_edit_events: bool) -> Harness {
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
            emit_edit_events,
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

fn body(v: &str) -> String {
    format!("---\ntype: instance\nskill: customer\nid: c1\n---\n# Acme\n\n{v}\n")
}

#[tokio::test]
async fn version_advances_and_stale_base_version_conflicts() {
    let h = start(false).await;

    // A read surfaces the current version (v0 — no writes through the version
    // path yet).
    let v0 = structured(&call(&h.process, "expand", json!({ "page_id": C1_PAGE })).await);
    assert_eq!(v0["version"], "v0");

    // First write (no base_version) → ok, version advances to v1.
    let w1 = structured(
        &call(
            &h.process,
            "update_page",
            json!({ "page_id": C1_PAGE, "content": body("v1 body.") }),
        )
        .await,
    );
    assert_eq!(w1["ok"], true);
    assert_eq!(w1["new_version"], "v1");
    // The read now reflects it.
    let v1 = structured(&call(&h.process, "expand", json!({ "page_id": C1_PAGE })).await);
    assert_eq!(v1["version"], "v1");

    // A STALE base_version (v0, but head is v1) → conflict + head_content.
    let stale = structured(
        &call(
            &h.process,
            "update_page",
            json!({ "page_id": C1_PAGE, "content": body("v2 body."), "base_version": "v0" }),
        )
        .await,
    );
    assert_eq!(stale["ok"], false);
    assert_eq!(stale["issues"][0]["code"], "conflict");
    assert!(
        stale["head_content"]
            .as_str()
            .is_some_and(|c| c.contains("v1 body")),
        "conflict carries head_content for re-draft: {stale}"
    );

    // The CORRECT base_version (v1) → ok, advances to v2.
    let w2 = structured(
        &call(
            &h.process,
            "update_page",
            json!({ "page_id": C1_PAGE, "content": body("v2 body."), "base_version": "v1" }),
        )
        .await,
    );
    assert_eq!(w2["ok"], true);
    assert_eq!(w2["new_version"], "v2");

    h.process.shutdown().await;
}

#[tokio::test]
async fn edit_event_fires_for_out_of_band_write_and_is_suppressed_for_runner() {
    let h = start(true).await;

    let inbox_count = |env: &Value| -> usize {
        structured(env)["events"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter(|e| e["label_skill"] == "page-edited")
                    .count()
            })
            .unwrap_or(0)
    };

    // Out-of-band edit (no provenance) → a page-edited event lands in the inbox.
    call(
        &h.process,
        "update_page",
        json!({ "page_id": C1_PAGE, "content": body("edited") }),
    )
    .await;
    let after_edit = inbox_count(&call(&h.process, "list_inbox", json!({})).await);
    assert!(
        after_edit >= 1,
        "out-of-band edit emits a page-edited event"
    );

    // Runner-orchestrated write (carries provenance) → suppressed (no new event).
    call(
        &h.process,
        "update_page",
        json!({
            "page_id": C1_PAGE,
            "content": body("runner edit"),
            "provenance": { "workflow": { "run": "r1", "wf_skill": "distill" } }
        }),
    )
    .await;
    let after_runner = inbox_count(&call(&h.process, "list_inbox", json!({})).await);
    assert_eq!(
        after_runner, after_edit,
        "a runner-provenance write does not emit a page-edited event (no storm)"
    );

    h.process.shutdown().await;
}
