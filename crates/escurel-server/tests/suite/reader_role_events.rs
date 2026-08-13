//! A ducklake reader's events tool gate, LIVE (DuckLake PR 9, Phase B): a
//! REAL Postgres testcontainer backs both the DuckLake catalog AND the
//! shared events table (`events_pg.escurel_events`) `EscurelConfig::
//! build` attaches at boot when the catalog is Postgres-style. Mirrors
//! `reader_role_chat.rs` (DuckLake PR 8) exactly, applied to
//! `capture_event` / `list_events` / `list_inbox` instead of
//! `append_message` / `list_messages`.
//!
//! Before this PR, `capture_event`/`assign_event`/`list_events`/
//! `list_inbox` were on the reader's static `UNSUPPORTED_ON_REPLICA_TOOLS`
//! list unconditionally — this test proves that gate is now dynamic: a
//! reader booted against a Postgres catalog serves them instead of
//! rejecting them.
//!
//! Opt-in: gated behind the `live-ducklake` feature (needs Docker). Run
//! with `cargo test -p escurel-server --features live-ducklake --test
//! reader_role_events`.

#![cfg(feature = "live-ducklake")]

use std::collections::HashMap;
use std::sync::Arc;

use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::snapshot::{LakeConfig, ObjectStoreSecret, publish_lake};
use escurel_index::{Indexer, Migrator};
use escurel_server::EscurelConfig;
use escurel_storage::{FsStore, LaneStore};
use tempfile::TempDir;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ContainerAsync;
use testcontainers_modules::testcontainers::runners::AsyncRunner;

const TENANT: &str = "acme";

const CUSTOMER_SKILL: (&str, &str) = (
    "markdown/skills/customer.md",
    "---\n\
     type: skill\n\
     id: customer\n\
     description: a customer\n\
     ---\n\
     # customer\n\
     \n\
     A customer is the unit of revenue.\n",
);

struct Harness {
    indexer: Arc<Indexer>,
    data_root: TempDir,
    lake_dir: TempDir,
    _db_dir: TempDir,
    _pg: ContainerAsync<Postgres>,
    catalog_dsn: String,
}

async fn fresh_harness() -> Harness {
    let pg = Postgres::default().start().await.expect("start postgres");
    let pg_port = pg.get_host_port_ipv4(5432).await.expect("pg port");
    let catalog_dsn =
        format!("host=127.0.0.1 port={pg_port} user=postgres password=postgres dbname=postgres");

    let data_root = TempDir::new().unwrap();
    let lake_dir = TempDir::new().unwrap();
    std::fs::create_dir_all(lake_dir.path().join("data")).unwrap();
    let db_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(data_root.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("harness.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Arc::new(Indexer::new(store, embedder, conn, TENANT).unwrap());
    Harness {
        indexer,
        data_root,
        lake_dir,
        _db_dir: db_dir,
        _pg: pg,
        catalog_dsn,
    }
}

fn lake_config(h: &Harness) -> LakeConfig {
    LakeConfig {
        catalog_dsn: h.catalog_dsn.clone(),
        data_path: h.lake_dir.path().join("data").to_str().unwrap().to_owned(),
        object_store: ObjectStoreSecret::None,
    }
}

async fn seed_and_publish(h: &Harness) {
    h.indexer
        .update_page(CUSTOMER_SKILL.0, CUSTOMER_SKILL.1)
        .await
        .unwrap();
    publish_lake(&h.indexer, &lake_config(h), None)
        .await
        .expect("publish");
}

fn env_map(pairs: Vec<(&str, String)>) -> HashMap<String, String> {
    pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()
}

fn source(map: HashMap<String, String>) -> impl Fn(&str) -> Option<String> {
    move |k: &str| map.get(k).cloned()
}

fn reader_cfg(h: &Harness) -> EscurelConfig {
    let lake = lake_config(h);
    EscurelConfig::from_source(&source(env_map(vec![
        (
            "ESCUREL_SERVER_DATA_DIR",
            h.data_root.path().to_str().unwrap().to_owned(),
        ),
        ("ESCUREL_SERVER_LISTEN_HTTP", "127.0.0.1:0".to_owned()),
        (
            "ESCUREL_OBSERVABILITY_METRICS_LISTEN",
            "127.0.0.1:0".to_owned(),
        ),
        ("ESCUREL_TENANT", TENANT.to_owned()),
        ("ESCUREL_EMBEDDING_PROVIDER", "zero".to_owned()),
        ("ESCUREL_INDEX_BACKEND", "ducklake".to_owned()),
        ("ESCUREL_ROLE", "reader".to_owned()),
        ("ESCUREL_DUCKLAKE_CATALOG_DSN", lake.catalog_dsn),
        ("ESCUREL_DUCKLAKE_DATA_PATH", lake.data_path),
        ("ESCUREL_SNAPSHOT_REFRESH_SECS", "30".to_owned()),
    ])))
    .expect("reader config parses")
}

async fn call(base: &str, name: &str, args: serde_json::Value) -> serde_json::Value {
    reqwest::Client::new()
        .post(format!("{base}/mcp"))
        .json(&serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": args }
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

#[tokio::test]
async fn reader_no_longer_rejects_events_tools_when_ducklake_configured() {
    let h = fresh_harness().await;
    seed_and_publish(&h).await;
    let booted = reader_cfg(&h)
        .build()
        .await
        .expect("reader boots with a Postgres-catalog lake");
    let base = format!("http://{}", booted.handle.local_addr);

    let capture_resp = call(
        &base,
        "capture_event",
        serde_json::json!({
            "source": "gmail",
            "mime": "message/rfc822",
            "label_skill": "email",
            "title": "hi from a reader",
            "body": "captured on a reader",
        }),
    )
    .await;
    assert!(
        capture_resp.get("error").is_none(),
        "capture_event on a Postgres-catalog reader must succeed, not \
         unsupported_on_replica: {capture_resp}"
    );

    let list_resp = call(&base, "list_inbox", serde_json::json!({})).await;
    assert!(
        list_resp.get("error").is_none(),
        "list_inbox on a Postgres-catalog reader must succeed: {list_resp}"
    );
    let events = &list_resp["result"]["structuredContent"]["events"];
    assert_eq!(
        events.as_array().map(|a| a.len()),
        Some(1),
        "the reader must read back the event it just captured: {list_resp}"
    );
    assert_eq!(events[0]["title"], "hi from a reader");

    booted.refresh_handle.unwrap().shutdown().await;
    booted.handle.shutdown().await;
}
