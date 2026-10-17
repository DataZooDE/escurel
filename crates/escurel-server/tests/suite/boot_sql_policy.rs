//! A SQL-source binding already in storage is judged by the operator's policy when the writer REBUILDS
//! its views at boot, not only when the binding is first written.
//!
//! The credential/egress policy used to be installed on the served indexer in `serve()`, after
//! `Config::build` had already rebuilt every `sql_view` (the fresh-boot rebuild, and the lake adopt's
//! rebuild). A binding written before the policy existed, or under a wider one (the operator tightened
//! `ESCUREL_SQL_FILE_DIRS` since), was therefore re-attached with no check: a directory view over a path
//! the operator no longer exposes stayed queryable for the life of the process.
//!
//! Real DuckDB, real `FsStore`, real `Config::build` and gateway, real reqwest. No mocks.

use std::collections::HashMap;
use std::sync::Arc;

use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::backend::{SqlConnector, SqlViewBackend, SqlViewBinding};
use escurel_index::snapshot::{LakeConfig, ObjectStoreSecret, publish_lake};
use escurel_index::{Indexer, Migrator};
use escurel_server::EscurelConfig;
use escurel_storage::{FsStore, LaneStore};
use serde_json::{Value, json};
use tempfile::TempDir;

const TENANT: &str = "acme";

const SKILL_CUSTOMERS: &str = "\
---
kind: skill
id: customers
description: EU customers, mirrored read-only.
backend:
  kind: sql_view
  source: { connector: json_dir, relation: /unused }
---
# customers
";

const SKILL_QUERY: &str = "\
---
kind: skill
id: query
description: Reusable parameterised reads.
---
# query
";

const QUERY_ALL: &str = "\
---
kind: instance
skill: query
id: all-customers
target: \"[[customers::eu]]\"
sql: \"SELECT name FROM {{target}}\"
---
# all-customers
";

/// A tenant whose lane already holds a `sql_view` instance over `exposed_dir`, written with NO operator
/// policy installed (a bare indexer: what an older gateway, a pack import or an earlier, wider
/// `ESCUREL_SQL_FILE_DIRS` allowed).
async fn seed(data_dir: &TempDir, exposed_dir: &TempDir) -> Arc<Indexer> {
    std::fs::write(
        exposed_dir.path().join("a.json"),
        br#"{"name":"Acme","amount":30}"#,
    )
    .unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(data_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::new(768));
    let db_dir = TempDir::new().unwrap();
    let conn = Connection::open(db_dir.path().join("seed.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Arc::new(Indexer::new(store, embedder, conn, TENANT).unwrap());
    indexer
        .update_page("markdown/skills/customers.md", SKILL_CUSTOMERS)
        .await
        .unwrap();
    let binding = SqlViewBinding {
        connector: SqlConnector::JsonDir,
        attach: None,
        relation: exposed_dir.path().to_str().unwrap().to_owned(),
        filter: None,
        project: Default::default(),
        search_text: vec![],
    };
    SqlViewBackend::new(Arc::clone(&indexer))
        .create_instance("customers", &binding, "eu", "# EU customers")
        .await
        .unwrap();
    indexer
        .update_page("markdown/skills/query.md", SKILL_QUERY)
        .await
        .unwrap();
    indexer
        .update_page("markdown/instances/query/all-customers.md", QUERY_ALL)
        .await
        .unwrap();
    // Keep the seed DB alive for the caller's publish; the TempDir lives as long as the process here.
    std::mem::forget(db_dir);
    indexer
}

fn cfg(data_dir: &TempDir, extra: Vec<(&str, String)>) -> EscurelConfig {
    let mut map: HashMap<String, String> = [
        (
            "ESCUREL_SERVER_DATA_DIR",
            data_dir.path().to_str().unwrap().to_owned(),
        ),
        ("ESCUREL_SERVER_LISTEN_HTTP", "127.0.0.1:0".to_owned()),
        (
            "ESCUREL_OBSERVABILITY_METRICS_LISTEN",
            "127.0.0.1:0".to_owned(),
        ),
        ("ESCUREL_TENANT", TENANT.to_owned()),
        ("ESCUREL_EMBEDDING_PROVIDER", "zero".to_owned()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v))
    .collect();
    map.extend(extra.into_iter().map(|(k, v)| (k.to_owned(), v)));
    EscurelConfig::from_source(&move |k: &str| map.get(k).cloned()).expect("config parses")
}

/// Read the exposed directory through the managed view, the way an agent does: a `query_instance` over
/// the `[[customers::eu]]` view.
async fn view_rows(base: &str) -> Value {
    reqwest::Client::new()
        .post(format!("{base}/mcp"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": { "name": "query_instance",
                                   "arguments": { "ref": "[[query::all-customers]]" } } }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

#[tokio::test]
async fn a_stored_directory_binding_outside_the_policy_is_not_rebuilt_at_boot() {
    let data_dir = TempDir::new().unwrap();
    let exposed = TempDir::new().unwrap();
    let elsewhere = TempDir::new().unwrap();
    seed(&data_dir, &exposed).await;

    // The operator now exposes `elsewhere` only: the stored binding's directory is out of policy.
    let booted = cfg(
        &data_dir,
        vec![(
            "ESCUREL_SQL_FILE_DIRS",
            elsewhere.path().to_str().unwrap().to_owned(),
        )],
    )
    .build()
    .await
    .expect("the writer boots");
    let base = format!("http://{}", booted.handle.local_addr);
    let r = view_rows(&base).await;
    assert!(
        r.get("error").is_some(),
        "the out-of-policy view must not exist after boot, but it answered: {r}"
    );
    booted.handle.shutdown().await;
}

#[tokio::test]
async fn the_same_binding_inside_the_policy_is_rebuilt_and_readable() {
    // The control: the harness itself works, and the check is the policy, not a broken boot.
    let data_dir = TempDir::new().unwrap();
    let exposed = TempDir::new().unwrap();
    seed(&data_dir, &exposed).await;

    let booted = cfg(
        &data_dir,
        vec![(
            "ESCUREL_SQL_FILE_DIRS",
            exposed.path().to_str().unwrap().to_owned(),
        )],
    )
    .build()
    .await
    .expect("the writer boots");
    let base = format!("http://{}", booted.handle.local_addr);
    let r = view_rows(&base).await;
    assert!(r.get("error").is_none(), "{r}");
    assert_eq!(
        r["result"]["structuredContent"]["rows"][0]["name"], "Acme",
        "{r}"
    );
    booted.handle.shutdown().await;
}

#[tokio::test]
async fn a_lake_writer_adopt_applies_the_policy_before_it_rebuilds_the_views() {
    let data_dir = TempDir::new().unwrap();
    let exposed = TempDir::new().unwrap();
    let elsewhere = TempDir::new().unwrap();
    let lake_dir = TempDir::new().unwrap();
    std::fs::create_dir_all(lake_dir.path().join("data")).unwrap();
    let catalog = lake_dir
        .path()
        .join("catalog.ducklake")
        .to_str()
        .unwrap()
        .to_owned();
    let data_path = lake_dir.path().join("data").to_str().unwrap().to_owned();

    let seeded = seed(&data_dir, &exposed).await;
    publish_lake(
        &seeded,
        &LakeConfig {
            catalog_dsn: catalog.clone(),
            data_path: data_path.clone(),
            object_store: ObjectStoreSecret::None,
        },
        None,
    )
    .await
    .expect("seed publish");

    let booted = cfg(
        &data_dir,
        vec![
            ("ESCUREL_INDEX_BACKEND", "ducklake".to_owned()),
            ("ESCUREL_ROLE", "writer".to_owned()),
            ("ESCUREL_DUCKLAKE_CATALOG_DSN", catalog),
            ("ESCUREL_DUCKLAKE_DATA_PATH", data_path),
            (
                "ESCUREL_SQL_FILE_DIRS",
                elsewhere.path().to_str().unwrap().to_owned(),
            ),
        ],
    )
    .build()
    .await
    .expect("the lake writer boots");
    let base = format!("http://{}", booted.handle.local_addr);
    let r = view_rows(&base).await;
    assert!(
        r.get("error").is_some(),
        "the out-of-policy view must not exist after the lake adopt, but it answered: {r}"
    );
    booted.handle.shutdown().await;
}
