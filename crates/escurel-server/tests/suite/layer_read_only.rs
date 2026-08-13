//! End-to-end tests for the `layer` model (REQ-LAYER-01..04): pinned
//! read-only **base** pages (imported from a subscribed skill pack) vs
//! editable **overlay** pages (the default — every page today).
//!
//! Real running gateway, real DuckDB, real `FsStore`, real reqwest over
//! `POST /mcp`. Base pages are seeded through `Indexer::seed_from_dir`
//! — the same canonical-markdown import path a pack import (and the
//! production `ESCUREL_SEED_DIR` boot seed) uses — because the public
//! `update_page` write path must never be able to create one.
//!
//! Covers:
//! * AT-LAYER-1 — `update_page` on a base-layer page (skill or
//!   instance) → `Issue(layer_read_only)`, page unchanged. Stripping
//!   the `layer:` field in the incoming draft is not an unlock: the
//!   guard keys off the STORED page's layer.
//! * spoof guard — `update_page` cannot fabricate a page carrying
//!   `layer: base@…` (base pages are only created by pack import).
//! * AT-LAYER-3 — pages without a base layer write exactly as today
//!   (no regression; a tenant with zero packs is unaffected).
//! * REQ-LAYER-04 — `list_skills` reports each skill's `layer`
//!   (`overlay` default, `base@<pack>@<version>` for imported skills).
//! * CRDT bypass — `open_session` on a base-layer page is rejected;
//!   the live co-authoring path must not circumvent the read-only
//!   guard.

use std::sync::Arc;

use duckdb::Connection;
use escurel_crdt::{CrdtBackend, DuckdbCrdtBackend};
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, LaneStore};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, Opts};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::Mutex;

const TENANT: &str = "acme";

const BASE_SKILL: &str = "---\n\
type: skill\n\
id: pallet-consolidation\n\
description: Consolidate partial pallets (firm-authored, from the logistics pack).\n\
layer: base@logistics-midmarket@v7\n\
---\n\
# pallet-consolidation\n\n\
Firm-authored canonical procedure.\n";

const BASE_INSTANCE: &str = "---\n\
type: instance\n\
skill: pallet-consolidation\n\
id: edge-mixed-carrier\n\
layer: base@logistics-midmarket@v7\n\
---\n\
# Edge case: mixed-carrier consolidation\n\n\
Template shipped with the pack.\n";

const PLAIN_SKILL: &str = "---\n\
type: skill\n\
id: local-notes\n\
description: Tenant-authored notes skill.\n\
---\n\
# local-notes\n";

struct Setup {
    process: EscurelProcess,
    _dirs: Vec<TempDir>,
}

async fn setup() -> Setup {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let seed_dir = TempDir::new().unwrap();

    // The seed tree mirrors what a subscribed pack lands on disk: base
    // pages carry `layer: base@<pack>@<version>`; tenant pages don't.
    std::fs::create_dir_all(seed_dir.path().join("skills")).unwrap();
    std::fs::create_dir_all(seed_dir.path().join("instances/pallet-consolidation")).unwrap();
    std::fs::write(
        seed_dir.path().join("skills/pallet-consolidation.md"),
        BASE_SKILL,
    )
    .unwrap();
    std::fs::write(
        seed_dir
            .path()
            .join("instances/pallet-consolidation/edge-mixed-carrier.md"),
        BASE_INSTANCE,
    )
    .unwrap();
    std::fs::write(seed_dir.path().join("skills/local-notes.md"), PLAIN_SKILL).unwrap();

    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    // Second connection to the SAME DuckDB instance for the CRDT
    // backend, cloned before `conn` moves into the indexer (the
    // production boot does the same — config.rs).
    let crdt_conn = conn.try_clone().unwrap();
    let indexer = Arc::new(Indexer::new(store, embedder, conn, TENANT).unwrap());
    indexer.seed_from_dir(seed_dir.path()).await.unwrap();

    let crdt_backend: Arc<dyn CrdtBackend> =
        Arc::new(DuckdbCrdtBackend::new(Arc::new(Mutex::new(crdt_conn))));

    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::Disabled,
        config_overrides: ConfigOverrides {
            indexer: Some(indexer),
            crdt_backend: Some(crdt_backend),
            ..Default::default()
        },
        ..Default::default()
    })
    .await;

    Setup {
        process,
        _dirs: vec![store_dir, db_dir, seed_dir],
    }
}

async fn call(p: &EscurelProcess, name: &str, args: Value) -> Value {
    reqwest::Client::new()
        .post(p.mcp_url())
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": name, "arguments": args },
        }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json")
}

#[tokio::test]
async fn update_page_on_base_layer_skill_rejected_layer_read_only() {
    // AT-LAYER-1. The draft strips the `layer:` field — that must NOT
    // unlock the page; the guard keys off the stored page's layer.
    let s = setup().await;
    let draft = "---\n\
        type: skill\n\
        id: pallet-consolidation\n\
        description: HIJACKED\n\
        ---\n\
        # pallet-consolidation\n\nTampered.\n";
    let body = call(
        &s.process,
        "update_page",
        json!({ "page_id": "markdown/skills/pallet-consolidation.md", "content": draft }),
    )
    .await;
    let r = &body["result"]["structuredContent"];
    assert_eq!(r["ok"], false, "base-layer write must be rejected: {body}");
    assert_eq!(r["issues"][0]["code"], "layer_read_only");
    assert_eq!(r["issues"][0]["severity"], "error");

    // Base page pristine: description and layer pin unchanged.
    let ex = call(
        &s.process,
        "expand",
        json!({ "page_id": "markdown/skills/pallet-consolidation.md" }),
    )
    .await;
    let fm = &ex["result"]["structuredContent"]["frontmatter"];
    assert_eq!(fm["layer"], "base@logistics-midmarket@v7", "{ex}");
    assert_eq!(
        fm["description"],
        "Consolidate partial pallets (firm-authored, from the logistics pack)."
    );

    s.process.shutdown().await;
}

#[tokio::test]
async fn update_page_on_base_layer_instance_rejected_layer_read_only() {
    // AT-LAYER-1 for the pack's edge-case instance library.
    let s = setup().await;
    let draft = "---\n\
        type: instance\n\
        skill: pallet-consolidation\n\
        id: edge-mixed-carrier\n\
        ---\n\
        # Tampered edge case\n";
    let body = call(
        &s.process,
        "update_page",
        json!({
            "page_id": "markdown/instances/pallet-consolidation/edge-mixed-carrier.md",
            "content": draft,
        }),
    )
    .await;
    let r = &body["result"]["structuredContent"];
    assert_eq!(r["ok"], false, "base-layer write must be rejected: {body}");
    assert_eq!(r["issues"][0]["code"], "layer_read_only");

    s.process.shutdown().await;
}

#[tokio::test]
async fn update_page_cannot_fabricate_a_base_layer_page() {
    // Spoof guard: `layer: base@…` is stamped by pack import only. An
    // agent fabricating one via update_page (e.g. to squat a page id a
    // future pack import would land on) is rejected.
    let s = setup().await;
    let forged = "---\n\
        type: skill\n\
        id: forged-base\n\
        description: not really from a pack\n\
        layer: base@evil-pack@v1\n\
        ---\n\
        # forged\n";
    let body = call(
        &s.process,
        "update_page",
        json!({ "page_id": "markdown/skills/forged-base.md", "content": forged }),
    )
    .await;
    let r = &body["result"]["structuredContent"];
    assert_eq!(r["ok"], false, "base-layer spoof must be rejected: {body}");
    assert_eq!(r["issues"][0]["code"], "layer_read_only");

    s.process.shutdown().await;
}

#[tokio::test]
async fn pages_without_base_layer_write_as_today() {
    // AT-LAYER-3 (INV-ISO): a tenant page with no `layer:` — and one
    // declaring the explicit default `layer: overlay` — writes fine.
    let s = setup().await;
    let edit = "---\n\
        type: skill\n\
        id: local-notes\n\
        description: Edited tenant notes skill.\n\
        ---\n\
        # local-notes\n\nStill editable.\n";
    let body = call(
        &s.process,
        "update_page",
        json!({ "page_id": "markdown/skills/local-notes.md", "content": edit }),
    )
    .await;
    let r = &body["result"]["structuredContent"];
    assert_eq!(r["ok"], true, "plain page must stay writable: {body}");

    let overlay = "---\n\
        type: instance\n\
        skill: local-notes\n\
        id: note-1\n\
        layer: overlay\n\
        ---\n\
        # note 1\n";
    let body = call(
        &s.process,
        "update_page",
        json!({ "page_id": "markdown/instances/local-notes/note-1.md", "content": overlay }),
    )
    .await;
    let r = &body["result"]["structuredContent"];
    assert_eq!(r["ok"], true, "explicit overlay layer must write: {body}");

    s.process.shutdown().await;
}

#[tokio::test]
async fn list_skills_reports_layer_and_pack_pin() {
    // REQ-LAYER-04: agents/operators can see what is stable vs editable.
    let s = setup().await;
    let result = call(&s.process, "list_skills", json!({})).await;
    let skills = result["result"]["structuredContent"]["skills"]
        .as_array()
        .expect("skills array");

    let base = skills
        .iter()
        .find(|sk| sk["id"] == "pallet-consolidation")
        .expect("pack skill listed");
    assert_eq!(base["layer"], "base@logistics-midmarket@v7", "{result}");

    let plain = skills
        .iter()
        .find(|sk| sk["id"] == "local-notes")
        .expect("tenant skill listed");
    assert_eq!(plain["layer"], "overlay", "{result}");

    s.process.shutdown().await;
}

#[tokio::test]
async fn open_session_on_base_layer_page_rejected() {
    // The live CRDT co-authoring path must not bypass the read-only
    // guard: without this, apply_op edits a base page update_page
    // refuses to touch.
    let s = setup().await;
    let body = call(
        &s.process,
        "open_session",
        json!({ "page_id": "markdown/skills/pallet-consolidation.md" }),
    )
    .await;
    let err = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        err.contains("layer_read_only"),
        "open_session on a base page must fail with layer_read_only: {body}"
    );

    s.process.shutdown().await;
}
