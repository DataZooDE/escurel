//! #558 fast-follow: `update_page`/`delete_page`/`purge_page`/`move_page`
//! must not return `ok:true` until the write is durable in the lake, not
//! just in the local DuckDB `SingleFileStore::Always` wipes on every
//! writer boot. `admin_publish.rs` proves the ADMIN `publish_snapshot`
//! tool works; this proves the ORDINARY write tools are durable WITHOUT
//! ever calling it — the lake must already reflect every write the
//! moment the tool call returns.
//!
//! Offline harness, no Docker: same shape as `admin_publish.rs` (real
//! DuckDB, real `ducklake` extension, real Parquet — a DuckDB-file
//! catalog + local-directory `DATA_PATH`).

use std::collections::HashMap;

use duckdb::Connection;
use escurel_index::snapshot::{LakeConfig, ObjectStoreSecret};
use escurel_server::EscurelConfig;
use tempfile::TempDir;

const TENANT: &str = "acme";

const CUSTOMER_SKILL: (&str, &str) = (
    "markdown/skills/customer.md",
    "---\n\
     type: skill\n\
     id: customer\n\
     description: a customer\n\
     ---\n\
     # customer\n",
);

fn instance(id: &str) -> (String, String) {
    (
        format!("markdown/instances/customer/{id}.md"),
        format!("---\ntype: instance\nskill: customer\nid: {id}\n---\n# {id}\n\nA customer.\n"),
    )
}

fn env_map(pairs: Vec<(&str, String)>) -> HashMap<String, String> {
    pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()
}

fn source(map: HashMap<String, String>) -> impl Fn(&str) -> Option<String> {
    move |k: &str| map.get(k).cloned()
}

struct LakeDirs {
    lake_dir: TempDir,
}

fn fresh_lake_dirs() -> LakeDirs {
    let lake_dir = TempDir::new().unwrap();
    std::fs::create_dir_all(lake_dir.path().join("data")).unwrap();
    LakeDirs { lake_dir }
}

fn lake_config(dirs: &LakeDirs) -> LakeConfig {
    LakeConfig {
        catalog_dsn: dirs
            .lake_dir
            .path()
            .join("catalog.ducklake")
            .to_str()
            .unwrap()
            .to_owned(),
        data_path: dirs
            .lake_dir
            .path()
            .join("data")
            .to_str()
            .unwrap()
            .to_owned(),
        object_store: ObjectStoreSecret::None,
    }
}

/// A writer `EscurelConfig` with a real DuckLake attached, AND the
/// periodic publish disabled (`ESCUREL_SNAPSHOT_PUBLISH_SECS=0`) — the
/// whole point is proving durability with NO periodic publish in the
/// loop at all, so a passing test can only mean the synchronous path
/// did the work.
fn writer_cfg_no_periodic_publish(data_dir: &TempDir, dirs: &LakeDirs) -> EscurelConfig {
    let lake = lake_config(dirs);
    let pairs = vec![
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
        ("ESCUREL_INDEX_BACKEND", "ducklake".to_owned()),
        ("ESCUREL_ROLE", "writer".to_owned()),
        ("ESCUREL_DUCKLAKE_CATALOG_DSN", lake.catalog_dsn),
        ("ESCUREL_DUCKLAKE_DATA_PATH", lake.data_path),
        ("ESCUREL_SNAPSHOT_PUBLISH_SECS", "0".to_owned()),
    ];
    EscurelConfig::from_source(&source(env_map(pairs))).expect("writer config parses")
}

fn reader_conn(cfg: &LakeConfig) -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(&escurel_index::snapshot::install_load_sql(cfg))
        .unwrap();
    conn.execute_batch(&escurel_index::snapshot::attach_sql(cfg, true).unwrap())
        .unwrap();
    conn
}

fn lake_has_page(cfg: &LakeConfig, page_id: &str) -> bool {
    let conn = reader_conn(cfg);
    let count: i64 = conn
        .query_row(
            "SELECT count(*) FROM lake.pages WHERE page_id = ?",
            [page_id],
            |r| r.get(0),
        )
        .unwrap();
    count > 0
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
async fn update_page_is_durable_with_zero_periodic_publishes() {
    let data_dir = TempDir::new().unwrap();
    let dirs = fresh_lake_dirs();
    let lake = lake_config(&dirs);
    let cfg = writer_cfg_no_periodic_publish(&data_dir, &dirs);
    let booted = cfg.build().await.expect("writer boots");
    let base = format!("http://{}", booted.handle.local_addr);

    let (skill_path, skill_body) = CUSTOMER_SKILL;
    let resp = call(
        &base,
        "update_page",
        serde_json::json!({ "page_id": skill_path, "content": skill_body }),
    )
    .await;
    assert_eq!(resp["result"]["structuredContent"]["ok"], true, "{resp}");

    let (inst_path, inst_body) = instance("acme-corp");
    let resp = call(
        &base,
        "update_page",
        serde_json::json!({ "page_id": inst_path, "content": inst_body }),
    )
    .await;
    assert_eq!(resp["result"]["structuredContent"]["ok"], true, "{resp}");

    // No publish_snapshot call anywhere in this test. If the page is in
    // the lake, only the synchronous per-write path could have put it
    // there.
    assert!(
        lake_has_page(&lake, &inst_path),
        "update_page's ok:true must mean durable in the lake, with no periodic publish"
    );

    booted.handle.shutdown().await;
}

#[tokio::test]
async fn delete_page_removes_the_durable_lake_copy() {
    let data_dir = TempDir::new().unwrap();
    let dirs = fresh_lake_dirs();
    let lake = lake_config(&dirs);
    let cfg = writer_cfg_no_periodic_publish(&data_dir, &dirs);
    let booted = cfg.build().await.expect("writer boots");
    let base = format!("http://{}", booted.handle.local_addr);

    let (inst_path, inst_body) = instance("acme-corp");
    let resp = call(
        &base,
        "update_page",
        serde_json::json!({ "page_id": inst_path, "content": inst_body }),
    )
    .await;
    assert_eq!(resp["result"]["structuredContent"]["ok"], true, "{resp}");
    assert!(lake_has_page(&lake, &inst_path), "must be durable first");

    let resp = call(
        &base,
        "delete_page",
        serde_json::json!({ "page_id": inst_path }),
    )
    .await;
    assert_eq!(resp["result"]["structuredContent"]["ok"], true, "{resp}");

    assert!(
        !lake_has_page(&lake, &inst_path),
        "delete_page's ok:true must mean the lake no longer serves this page"
    );

    booted.handle.shutdown().await;
}

#[tokio::test]
async fn purge_page_after_delete_is_durable_too() {
    let data_dir = TempDir::new().unwrap();
    let dirs = fresh_lake_dirs();
    let lake = lake_config(&dirs);
    let cfg = writer_cfg_no_periodic_publish(&data_dir, &dirs);
    let booted = cfg.build().await.expect("writer boots");
    let base = format!("http://{}", booted.handle.local_addr);

    let (inst_path, inst_body) = instance("acme-corp");
    call(
        &base,
        "update_page",
        serde_json::json!({ "page_id": inst_path, "content": inst_body }),
    )
    .await;
    call(
        &base,
        "delete_page",
        serde_json::json!({ "page_id": inst_path }),
    )
    .await;

    let resp = call(
        &base,
        "purge_page",
        serde_json::json!({ "page_id": inst_path }),
    )
    .await;
    assert_eq!(resp["result"]["structuredContent"]["ok"], true, "{resp}");
    assert!(
        !lake_has_page(&lake, &inst_path),
        "purge_page must not resurrect a lake row delete_page already removed"
    );

    booted.handle.shutdown().await;
}

#[tokio::test]
async fn move_page_durably_relocates_in_the_lake() {
    let data_dir = TempDir::new().unwrap();
    let dirs = fresh_lake_dirs();
    let lake = lake_config(&dirs);
    let cfg = writer_cfg_no_periodic_publish(&data_dir, &dirs);
    let booted = cfg.build().await.expect("writer boots");
    let base = format!("http://{}", booted.handle.local_addr);

    let (from_path, from_body) = instance("acme-corp");
    let resp = call(
        &base,
        "update_page",
        serde_json::json!({ "page_id": from_path, "content": from_body }),
    )
    .await;
    assert_eq!(resp["result"]["structuredContent"]["ok"], true, "{resp}");
    assert!(lake_has_page(&lake, &from_path), "must be durable first");

    let (to_path, _) = instance("acme-holdings");
    let resp = call(
        &base,
        "move_page",
        serde_json::json!({ "from": from_path, "to": to_path }),
    )
    .await;
    assert_eq!(resp["result"]["structuredContent"]["ok"], true, "{resp}");

    assert!(
        !lake_has_page(&lake, &from_path),
        "the vacated source id must not linger in the lake"
    );
    assert!(
        lake_has_page(&lake, &to_path),
        "move_page's ok:true must mean the destination is already durable"
    );

    booted.handle.shutdown().await;
}
