//! Typed `search()` over a tenant whose only hits are `sql_view`
//! candidates — the wire-contract regression for the null-`anchor` bug.
//!
//! A `sql_view` instance is a page-grain hit with no block anchor; the
//! live gateway emits an explicit `"anchor": null` for it. The typed
//! client used to fail decoding that (`invalid type: null, expected a
//! string`), silently making `search()` unusable for any result set
//! containing sql_view hits — consumers (e.g. the datazoo-agent-template)
//! had to fall back to `call_raw`. Real gateway, real DuckDB, real
//! `FsStore`, real MCP-over-HTTP; no mocks (CLAUDE principle 2).

use std::sync::Arc;

use duckdb::Connection;
use escurel_client::{Client, SearchRequest, SecretString};
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::backend::{SqlConnector, SqlViewBackend, SqlViewBinding};
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, LaneStore};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, Opts};
use tempfile::TempDir;

const TENANT: &str = "acme";

fn skill_md(data_dir: &str) -> String {
    format!(
        "---\n\
         type: skill\n\
         id: customers\n\
         description: EU customers, mirrored read-only.\n\
         backend:\n\
        \x20 kind: sql_view\n\
        \x20 source:\n\
        \x20   connector: json_dir\n\
        \x20   relation: {data_dir}\n\
        \x20 project:\n\
        \x20   name: name\n\
        \x20 search_text: [name]\n\
         ---\n\
         # customers\n"
    )
}

/// A real gateway over a real indexer with one materialised sql_view
/// instance (the `sql_view_tools.rs` setup, minus the parts this test
/// doesn't need).
async fn start() -> (EscurelProcess, Vec<TempDir>) {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let data_dir = TempDir::new().unwrap();
    std::fs::write(
        data_dir.path().join("a.json"),
        br#"{"name":"Zephyrware","tier":"gold"}"#,
    )
    .unwrap();

    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Arc::new(Indexer::new(store, embedder, conn, TENANT).unwrap());

    indexer
        .update_page(
            "markdown/skills/customers.md",
            &skill_md(data_dir.path().to_str().unwrap()),
        )
        .await
        .unwrap();
    let binding = SqlViewBinding {
        connector: SqlConnector::JsonDir,
        attach: None,
        relation: data_dir.path().to_str().unwrap().to_owned(),
        filter: None,
        project: [("name".to_owned(), "name".to_owned())]
            .into_iter()
            .collect(),
        search_text: vec!["name".to_owned()],
    };
    SqlViewBackend::new(Arc::clone(&indexer))
        .create_instance("customers", &binding, "eu", "# EU customers\n")
        .await
        .unwrap();

    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::Disabled,
        config_overrides: ConfigOverrides {
            indexer: Some(indexer),
            ..Default::default()
        },
        ..Default::default()
    })
    .await;
    (process, vec![store_dir, db_dir, data_dir])
}

#[tokio::test]
async fn typed_search_decodes_sql_view_hits() {
    let (p, _dirs) = start().await;
    let client = Client::connect(p.base_url(), SecretString::from("dev".to_owned()))
        .await
        .unwrap();

    // "Zephyrware" only matches the sql_view's search_text column, so the
    // result set is sql_view-only. Page granularity drops the block anchor
    // (`search.rs`: page-level hits carry `anchor: None`), so every hit on
    // the wire is an explicit `"anchor": null` — the exact shape that used
    // to fail the typed decode.
    let resp = client
        .search(SearchRequest {
            q: "Zephyrware".to_owned(),
            k: 3,
            granularity: "page".to_owned(),
            ..Default::default()
        })
        .await
        .expect("typed search must decode sql_view hits (null anchor)");

    let hit = resp
        .hits
        .iter()
        .find(|h| h.skill == "customers")
        .expect("the sql_view instance must be hit");
    assert_eq!(hit.anchor, "", "page-grain hit has no block anchor");
    assert_eq!(
        hit.frontmatter_excerpt["backend_ref"]["kind"], "sql_view",
        "hit must be recognisably external: {hit:?}"
    );

    p.shutdown().await;
}
