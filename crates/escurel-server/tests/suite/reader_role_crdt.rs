//! A ducklake reader's CRDT/session tool gate, LIVE (DuckLake PR 10,
//! Phase B): a REAL Postgres testcontainer backs both the DuckLake
//! catalog AND the shared CRDT op-log/snapshot tables
//! (`crdt_pg.escurel_crdt_ops` / `crdt_pg.escurel_crdt_snapshots`)
//! `EscurelConfig::build` attaches at boot when the catalog is
//! Postgres-style. Mirrors `reader_role_chat.rs` (DuckLake PR 8) /
//! `reader_role_events.rs` (DuckLake PR 9) exactly, applied to
//! `open_session` / `apply_op` / `close_session` / `list_snapshots`
//! instead.
//!
//! Before this PR, those four tools were on the reader's static
//! `UNSUPPORTED_ON_REPLICA_TOOLS` list unconditionally — this test proves
//! that gate is now dynamic: a reader booted against a Postgres catalog
//! serves them instead of rejecting them.
//!
//! Scope: this test proves the durable STORAGE is reachable from a
//! reader — a session opened FRESH on a reader works, and `list_snapshots`
//! sees history the WRITER wrote. It deliberately does NOT attempt (and
//! DuckLake PR 10 does not build) mid-session hot failover of one live
//! editing session between replicas — `SessionManager` still runs one
//! `LiveDoc` actor per page in-process; there is no ingress-affinity
//! mechanism routing a page's `apply_op` calls back to whichever replica
//! opened its session.
//!
//! Opt-in: gated behind the `live-ducklake` feature (needs Docker). Run
//! with `cargo test -p escurel-server --features live-ducklake --test
//! reader_role_crdt`.

#![cfg(feature = "live-ducklake")]

use std::collections::HashMap;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::snapshot::{LakeConfig, ObjectStoreSecret, publish_lake};
use escurel_index::{Indexer, Migrator};
use escurel_server::EscurelConfig;
use escurel_storage::{FsStore, LaneStore};
use loro::{ExportMode, LoroDoc};
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

/// One Loro op blob (`insert("hi") at 0`), base64-encoded — the exact
/// shape `apply_op` expects, mirroring `mcp_session_tools.rs`'s `Client`.
fn one_insert_op_b64() -> String {
    let doc = LoroDoc::new();
    let vv = doc.oplog_vv();
    doc.get_text("body").insert(0, "hi from a reader").unwrap();
    doc.commit();
    let update = doc.export(ExportMode::updates(&vv)).unwrap();
    B64.encode(update)
}

#[tokio::test]
async fn reader_no_longer_rejects_session_tools_when_ducklake_configured() {
    let h = fresh_harness().await;
    seed_and_publish(&h).await;
    let booted = reader_cfg(&h)
        .build()
        .await
        .expect("reader boots with a Postgres-catalog lake");
    let base = format!("http://{}", booted.handle.local_addr);

    // open_session on a page that has never been touched anywhere —
    // proves a session opened FRESH on a reader works end to end.
    let opened = call(
        &base,
        "open_session",
        serde_json::json!({ "page_id": "reader-native-page" }),
    )
    .await;
    assert!(
        opened.get("error").is_none(),
        "open_session on a Postgres-catalog reader must succeed, not \
         unsupported_on_replica: {opened}"
    );
    let session = opened["result"]["structuredContent"]["session"]
        .as_str()
        .expect("session id")
        .to_owned();

    let apply_resp = call(
        &base,
        "apply_op",
        serde_json::json!({ "session": session, "op": one_insert_op_b64() }),
    )
    .await;
    assert!(
        apply_resp.get("error").is_none(),
        "apply_op on a Postgres-catalog reader must succeed: {apply_resp}"
    );
    assert_eq!(
        apply_resp["result"]["structuredContent"]["merged_version"],
        "v1"
    );

    let closed = call(
        &base,
        "close_session",
        serde_json::json!({ "session": session, "commit": true }),
    )
    .await;
    assert!(
        closed.get("error").is_none(),
        "close_session on a Postgres-catalog reader must succeed: {closed}"
    );

    // list_snapshots must see the snapshot this reader just took —
    // proves list_snapshots routes through the shared table too, not
    // just the live-session backend.
    let snaps = call(
        &base,
        "list_snapshots",
        serde_json::json!({ "page_id": "reader-native-page" }),
    )
    .await;
    assert!(
        snaps.get("error").is_none(),
        "list_snapshots on a Postgres-catalog reader must succeed: {snaps}"
    );
    let snapshots = &snaps["result"]["structuredContent"]["snapshots"];
    assert_eq!(
        snapshots.as_array().map(|a| a.len()),
        Some(1),
        "list_snapshots must read back the snapshot just committed: {snaps}"
    );

    booted.refresh_handle.unwrap().shutdown().await;
    booted.handle.shutdown().await;
}
