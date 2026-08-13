//! DuckLake PR 2 — the `IndexStore` seam.
//!
//! `SingleFileStore::open()` must reproduce today's single-file boot
//! sequence exactly: a gateway booted through the seam serves the same
//! MCP surface (write a page, search it back), and a SECOND boot
//! against the same data dir (the restart / non-fresh path: no
//! `Migrator::up`, no rebuild) still serves the previously written
//! corpus. Real server, real DuckDB file, real FsStore — no mocks.

use std::path::Path;
use std::sync::Arc;

use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::snapshot::{IndexStore, IndexerHandle, SingleFileStore};
use escurel_storage::{FsStore, LaneStore};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, Opts};
use serde_json::{Value, json};

const TENANT: &str = "acme";

const SKILL_CUSTOMER_PAGE: &str = "markdown/skills/customer.md";
const SKILL_CUSTOMER_BODY: &str = "---\n\
     type: skill\n\
     id: customer\n\
     description: A buying entity.\n\
     ---\n\
     # customer\n";

const INSTANCE_ACME_PAGE: &str = "markdown/instances/customer/acme-corp.md";
const INSTANCE_ACME_BODY: &str = "---\n\
     type: instance\n\
     skill: customer\n\
     id: acme-corp\n\
     ---\n\
     # Acme Corp\n\
     \n\
     Acme ships anvils to distributors.\n";

/// The production single-file recipe, minus the server-config plumbing:
/// FsStore lane + ZeroEmbedder over `<data_dir>/tenants/<tenant>/`.
fn single_file_store(data_dir: &Path) -> SingleFileStore {
    let lane: Arc<dyn LaneStore> = Arc::new(FsStore::new(data_dir.to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::new(768));
    SingleFileStore {
        tenant_dir: data_dir.join("tenants").join(TENANT),
        rebuild_on_boot: false,
        store: lane,
        embedder,
        tenant: TENANT.to_owned(),
        contextualize: escurel_index::backend::ContextualizeMode::Structural,
        attach_retrieval: None,
        seed_dir: None,
    }
}

async fn call_tool(p: &EscurelProcess, name: &str, args: Value) -> Value {
    let resp = reqwest::Client::new()
        .post(p.mcp_url())
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": name, "arguments": args },
        }))
        .send()
        .await
        .expect("post");
    assert_eq!(resp.status(), 200, "http status");
    let body: Value = resp.json().await.unwrap();
    if body.get("error").is_some() {
        panic!("tool {name} returned error: {body}");
    }
    body["result"]["structuredContent"].clone()
}

async fn search_hits_acme(p: &EscurelProcess) -> Vec<Value> {
    let result = call_tool(p, "search", json!({ "q": "anvils", "k": 5 })).await;
    result["hits"].as_array().expect("hits array").clone()
}

#[tokio::test]
async fn single_file_backend_boots_and_serves_unchanged() {
    let data_dir = tempfile::TempDir::new().expect("data dir");

    // --- Boot 1: fresh open through the IndexStore seam.
    let opened = single_file_store(data_dir.path())
        .open()
        .await
        .expect("fresh single-file open");
    assert!(
        opened.crdt_conn.is_some(),
        "single-file open returns the cloned CRDT connection"
    );
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::Disabled,
        fixtures: None,
        config_overrides: ConfigOverrides {
            indexer: Some(Arc::clone(&opened.indexer)),
            ..Default::default()
        },
    })
    .await;

    // A fresh tenant carries the mandatory meta-skill (ensure_meta_skill
    // ran inside open()).
    let skills = call_tool(&p, "list_skills", json!({})).await;
    let ids: Vec<&str> = skills["skills"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s["id"].as_str())
        .collect();
    assert!(ids.contains(&"escurel"), "meta-skill present; got {ids:?}");

    // Write a skill + instance through the public MCP surface, then
    // search the instance back.
    call_tool(
        &p,
        "update_page",
        json!({ "page_id": SKILL_CUSTOMER_PAGE, "content": SKILL_CUSTOMER_BODY }),
    )
    .await;
    call_tool(
        &p,
        "update_page",
        json!({ "page_id": INSTANCE_ACME_PAGE, "content": INSTANCE_ACME_BODY }),
    )
    .await;
    let hits = search_hits_acme(&p).await;
    assert!(!hits.is_empty(), "freshly written page must be searchable");

    // Release the DuckDB file: server first, then every handle from the
    // first open (indexer + cloned CRDT connection).
    p.shutdown().await;
    drop(opened);

    // --- Boot 2: same data dir — the restart (non-fresh) path. No
    // `Migrator::up`, no rebuild; the on-disk index must serve as-is.
    let reopened = single_file_store(data_dir.path())
        .open()
        .await
        .expect("restart single-file open");
    let p2 = EscurelProcess::spawn(Opts {
        auth: AuthMode::Disabled,
        fixtures: None,
        config_overrides: ConfigOverrides {
            indexer: Some(Arc::clone(&reopened.indexer)),
            ..Default::default()
        },
    })
    .await;

    let hits = search_hits_acme(&p2).await;
    assert!(
        !hits.is_empty(),
        "page written before the restart must still be served"
    );
    // The written page is still readable verbatim.
    let expanded = call_tool(&p2, "expand", json!({ "page_id": INSTANCE_ACME_PAGE })).await;
    assert!(
        expanded.to_string().contains("Acme Corp"),
        "expand must return the pre-restart page; got {expanded}"
    );

    p2.shutdown().await;
    drop(reopened);
}

/// `IndexerHandle` is the hot-swap seam: `swap` returns the previous
/// indexer and `current()` observes the new one immediately.
#[tokio::test]
async fn indexer_handle_swap_returns_old_and_current_sees_new() {
    let dir_a = tempfile::TempDir::new().unwrap();
    let dir_b = tempfile::TempDir::new().unwrap();
    let a = single_file_store(dir_a.path())
        .open()
        .await
        .expect("open a")
        .indexer;
    let b = single_file_store(dir_b.path())
        .open()
        .await
        .expect("open b")
        .indexer;

    let handle = IndexerHandle::fixed(Arc::clone(&a));
    assert!(Arc::ptr_eq(&handle.current(), &a));

    let old = handle.swap(Arc::clone(&b));
    assert!(Arc::ptr_eq(&old, &a), "swap returns the previous indexer");
    assert!(
        Arc::ptr_eq(&handle.current(), &b),
        "current() sees the swapped-in indexer"
    );
}
