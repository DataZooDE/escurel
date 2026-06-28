//! INV-ACL-FUSION regression test (PR-2d, spike S3 — the security must-fix).
//!
//! The dispatcher fuses two retrieval lanes — native blocks + late-materialised
//! SQL-view hits — and MUST apply the fail-closed ACL predicate to EVERY lane
//! BEFORE fusion. Spike S3 showed that the naive path (SQL hits unioned
//! unfiltered) leaks cross-owner rows. This test pins:
//!
//! 1. zero cross-owner leakage: owner A's search never surfaces owner B's
//!    SQL-view instance, even though B's view matches the query;
//! 2. default-deny: a SQL-view instance whose owner cannot be resolved is
//!    denied to a non-admin;
//! 3. markdown no-regression: the native lane still returns a public hit.
//!
//! Real gateway, real DuckDB, real OIDC (TestIssuer), offline json_dir.

use std::sync::Arc;

use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::backend::{SqlConnector, SqlViewBackend, SqlViewBinding};
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, LaneStore};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, Opts, Role};
use serde_json::{Value, json};
use tempfile::TempDir;

const TENANT: &str = "acme";

// Owner-gated SQL-view skill: instances are readable only by their owner
// (visibility: owner) — the page-grain ACL that rides the overlay.
const DEAL_SKILL: &str = "\
---
type: skill
id: deal
description: Owner-private deals mirrored from the CRM.
backend:
  kind: sql_view
  source:
    connector: json_dir
    relation: /unused-skill-level
  search_text: [title]
visibility: owner
owner_field: owner_principal
---
# deal
";

// A public markdown skill, for the native-lane no-regression check.
const NOTE_SKILL: &str = "---\ntype: skill\nid: note\ndescription: public notes\n---\n# note\n";

fn deal_dir_with_title(title: &str) -> TempDir {
    let d = TempDir::new().unwrap();
    std::fs::write(
        d.path().join("row.json"),
        format!(r#"{{"title":"{title}"}}"#).into_bytes(),
    )
    .unwrap();
    d
}

/// Materialise a SQL-view `deal` instance over its own json dir, then set the
/// owner on its overlay (create_instance writes only type/skill/id/backend_ref;
/// owner-gating needs the owner_field in the overlay frontmatter).
async fn seed_deal(
    indexer: &Arc<Indexer>,
    id: &str,
    title: &str,
    owner_principal: &str,
    keep: &mut Vec<TempDir>,
) {
    let dir = deal_dir_with_title(title);
    let binding = SqlViewBinding {
        connector: SqlConnector::JsonDir,
        attach: None,
        relation: dir.path().to_str().unwrap().to_owned(),
        filter: None,
        project: Default::default(),
        search_text: vec!["title".to_owned()],
    };
    let m = SqlViewBackend::new(Arc::clone(indexer))
        .create_instance("deal", &binding, id, "# deal overlay")
        .await
        .unwrap();
    keep.push(dir);

    // Overwrite the overlay with the owner set, keeping the backend_ref.view.
    let content = format!(
        "---\n\
         type: instance\n\
         skill: deal\n\
         id: {id}\n\
         owner_principal: {owner_principal}\n\
         backend_ref:\n\
        \x20 kind: sql_view\n\
        \x20 view: {}\n\
         ---\n\
         # deal overlay\n",
        m.view
    );
    indexer
        .update_page(&format!("markdown/instances/deal/{id}.md"), &content)
        .await
        .unwrap();
}

async fn search(p: &EscurelProcess, sub: &str, q: &str) -> Vec<String> {
    search_args(p, sub, json!({ "q": q, "k": 25 })).await
}

/// Multi-query search: pass the `queries` plural with no scalar `q`.
async fn search_multi(p: &EscurelProcess, sub: &str, queries: &[&str]) -> Vec<String> {
    search_args(p, sub, json!({ "queries": queries, "k": 25 })).await
}

async fn search_args(p: &EscurelProcess, sub: &str, arguments: Value) -> Vec<String> {
    let token = p.mint_token_with_sub(TENANT, Role::Agent, sub);
    let body: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "search", "arguments": arguments },
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    body["result"]["structuredContent"]["hits"]
        .as_array()
        .expect("hits array")
        .iter()
        .filter_map(|h| h["page_id"].as_str().map(str::to_owned))
        .collect()
}

#[tokio::test]
async fn fused_search_never_leaks_cross_owner_sql_hits() {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Arc::new(Indexer::new(store, embedder, conn, TENANT).unwrap());

    let mut keep = Vec::new();
    indexer
        .update_page("markdown/skills/deal.md", DEAL_SKILL)
        .await
        .unwrap();
    indexer
        .update_page("markdown/skills/note.md", NOTE_SKILL)
        .await
        .unwrap();

    // Three deals whose views all match "widget": alice's, bob's, and an
    // orphan whose owner is an unresolvable wikilink (default-deny).
    seed_deal(
        &indexer,
        "alice-deal",
        "widget alpha",
        "alice-sub",
        &mut keep,
    )
    .await;
    seed_deal(&indexer, "bob-deal", "widget beta", "bob-sub", &mut keep).await;
    seed_deal(
        &indexer,
        "orphan-deal",
        "widget gamma",
        "[[ghost::nobody]]",
        &mut keep,
    )
    .await;

    // A public markdown note that also matches "widget" (native lane).
    indexer
        .update_page(
            "markdown/instances/note/public.md",
            "---\ntype: instance\nskill: note\nid: public\n---\n# Public widget note\n",
        )
        .await
        .unwrap();
    indexer.refresh_fts().await.unwrap();

    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            indexer: Some(Arc::clone(&indexer)),
            ..Default::default()
        },
        ..Default::default()
    })
    .await;

    let alice_hits = search(&p, "alice-sub", "widget").await;

    // 1. Zero cross-owner leakage + default-deny: alice sees her own SQL deal,
    //    never bob's, never the unresolved-owner orphan.
    assert!(
        alice_hits.iter().any(|p| p.contains("deal/alice-deal")),
        "alice must see her own SQL deal; got {alice_hits:?}"
    );
    assert!(
        !alice_hits.iter().any(|p| p.contains("deal/bob-deal")),
        "CROSS-OWNER LEAK: alice saw bob's SQL deal; got {alice_hits:?}"
    );
    assert!(
        !alice_hits.iter().any(|p| p.contains("deal/orphan-deal")),
        "DEFAULT-DENY VIOLATION: alice saw the unresolved-owner deal; got {alice_hits:?}"
    );

    // 3. Markdown no-regression: the public native-lane hit still surfaces.
    assert!(
        alice_hits.iter().any(|p| p.contains("note/public")),
        "native-lane public hit must still surface; got {alice_hits:?}"
    );

    // Symmetric: bob sees his own SQL deal, never alice's.
    let bob_hits = search(&p, "bob-sub", "widget").await;
    assert!(bob_hits.iter().any(|p| p.contains("deal/bob-deal")));
    assert!(
        !bob_hits.iter().any(|p| p.contains("deal/alice-deal")),
        "CROSS-OWNER LEAK: bob saw alice's SQL deal; got {bob_hits:?}"
    );

    // Multi-query (#217 Part 2): alice issues two phrasings in one call.
    // The variants are fused (union) and EACH variant's contribution is
    // ACL-filtered before fusion — INV-ACL-FUSION extends to the plural
    // path, so still no cross-owner / unresolved-owner leakage.
    let alice_multi = search_multi(&p, "alice-sub", &["widget", "alpha deal"]).await;
    assert!(
        alice_multi.iter().any(|p| p.contains("deal/alice-deal")),
        "multi-query must surface alice's own deal; got {alice_multi:?}"
    );
    assert!(
        alice_multi.iter().any(|p| p.contains("note/public")),
        "multi-query must fuse the public native hit; got {alice_multi:?}"
    );
    assert!(
        !alice_multi.iter().any(|p| p.contains("deal/bob-deal")),
        "CROSS-OWNER LEAK (multi-query): alice saw bob's deal; got {alice_multi:?}"
    );
    assert!(
        !alice_multi.iter().any(|p| p.contains("deal/orphan-deal")),
        "DEFAULT-DENY (multi-query): alice saw the unresolved-owner deal; got {alice_multi:?}"
    );

    p.shutdown().await;
    drop(keep);
}
