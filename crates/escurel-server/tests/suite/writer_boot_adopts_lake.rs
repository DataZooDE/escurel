//! #563: a writer boot must adopt whatever the lake already durably
//! holds BEFORE it can serve or its periodic publish task's first tick
//! can fire — otherwise that tick's full-overwrite publish destroys
//! everything only the lake remembers, because a fresh writer's local
//! corpus starts as just the reseeded meta-skill page.
//!
//! This is the definitive regression test: it reproduces a real pod
//! restart (a second, otherwise-empty writer boot against the SAME
//! lake a first writer already published to) in an offline harness, no
//! Docker — the same DuckDB-file-catalog shape `synchronous_lake_durability.rs`
//! and `admin_publish.rs` already use.

use std::collections::HashMap;
use std::time::Duration;

use duckdb::Connection;
use escurel_index::snapshot::{LakeConfig, ObjectStoreSecret};
use escurel_server::EscurelConfig;
use tempfile::TempDir;

const TENANT: &str = "acme";

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

/// A writer `EscurelConfig`, own scratch `data_dir` (a fresh one per
/// call simulates a fresh pod), pointed at the SAME lake.
fn writer_cfg(data_dir: &TempDir, dirs: &LakeDirs, publish_secs: u64) -> EscurelConfig {
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
        ("ESCUREL_SNAPSHOT_PUBLISH_SECS", publish_secs.to_string()),
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

fn lake_page_count(cfg: &LakeConfig) -> i64 {
    let conn = reader_conn(cfg);
    conn.query_row("SELECT count(*) FROM lake.pages", [], |r| r.get(0))
        .unwrap()
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

/// The core #563 reproduction: writer A publishes a page and dies;
/// writer B boots fresh against the SAME lake and must serve that page
/// IMMEDIATELY — before any manual publish_snapshot call, before any
/// periodic tick (disabled here, `ESCUREL_SNAPSHOT_PUBLISH_SECS=0`), so
/// a pass can only mean the boot-time adoption did the work.
#[tokio::test]
async fn a_fresh_writer_serves_the_prior_writer_s_pages_immediately() {
    let dirs = fresh_lake_dirs();

    // Writer A: one page, durable via #414/#415's synchronous publish.
    let data_dir_a = TempDir::new().unwrap();
    let cfg_a = writer_cfg(&data_dir_a, &dirs, 0);
    let booted_a = cfg_a.build().await.expect("writer A boots");
    let base_a = format!("http://{}", booted_a.handle.local_addr);
    let (inst_path, inst_body) = instance("acme-corp");
    let resp = call(
        &base_a,
        "update_page",
        serde_json::json!({ "page_id": inst_path, "content": inst_body }),
    )
    .await;
    assert_eq!(resp["result"]["structuredContent"]["ok"], true, "{resp}");
    booted_a.handle.shutdown().await;

    // Writer B: a BRAND NEW data dir — the local DuckDB this pod boots
    // with has never seen this page. Periodic publish disabled, so
    // there is no tick that could coincidentally load anything either.
    let data_dir_b = TempDir::new().unwrap();
    let cfg_b = writer_cfg(&data_dir_b, &dirs, 0);
    let booted_b = cfg_b.build().await.expect("writer B boots");
    let base_b = format!("http://{}", booted_b.handle.local_addr);

    let resp = call(
        &base_b,
        "expand",
        serde_json::json!({ "page_id": inst_path }),
    )
    .await;
    let page = &resp["result"]["structuredContent"]["page"];
    assert!(
        !page.is_null(),
        "writer B must serve writer A's page immediately on boot, with \
         no manual publish and no periodic tick — got {resp}"
    );
    assert_eq!(page["page_id"], inst_path);

    booted_b.handle.shutdown().await;
}

/// The destructive half of #563, proven absent: with adoption in
/// place, a writer whose periodic publish DOES fire (short interval)
/// must not wipe the lake down to its own boot-time state — because
/// that boot-time state is now the full adopted corpus, not just the
/// meta-skill.
#[tokio::test]
async fn periodic_publish_after_adoption_does_not_shrink_the_lake() {
    let dirs = fresh_lake_dirs();
    let lake = lake_config(&dirs);

    let data_dir_a = TempDir::new().unwrap();
    let cfg_a = writer_cfg(&data_dir_a, &dirs, 0);
    let booted_a = cfg_a.build().await.expect("writer A boots");
    let base_a = format!("http://{}", booted_a.handle.local_addr);
    let (inst_path, inst_body) = instance("acme-corp");
    call(
        &base_a,
        "update_page",
        serde_json::json!({ "page_id": inst_path, "content": inst_body }),
    )
    .await;
    booted_a.handle.shutdown().await;

    let pages_before = lake_page_count(&lake);
    // Just the one instance page: the meta-skill is seeded via internal
    // bootstrapping, not the update_page MCP tool, so #414/#415's
    // synchronous per-write publish never covers it — only a full
    // publish_lake does, and none ran here (periodic publish is off).
    // Harmless: the meta-skill is deterministically reseeded on every
    // boot, so this is a resurrection, not a loss.
    assert_eq!(
        pages_before, 1,
        "the one instance page, synchronously published"
    );

    // Writer B: periodic publish ON with a short interval, so its first
    // (immediate-tick) publish fires during this test.
    let data_dir_b = TempDir::new().unwrap();
    let cfg_b = writer_cfg(&data_dir_b, &dirs, 1);
    let booted_b = cfg_b.build().await.expect("writer B boots");
    tokio::time::sleep(Duration::from_millis(1500)).await;

    let pages_after = lake_page_count(&lake);
    assert_eq!(
        pages_after, pages_before,
        "a periodic publish after boot-time adoption must not shrink the lake"
    );

    booted_b.handle.shutdown().await;
}

/// A brand-new cluster: the lake has never been published. A writer
/// must still boot successfully (adoption is a legitimate no-op here,
/// not a failure) and serve only its own freshly-seeded meta-skill.
#[tokio::test]
async fn first_ever_writer_boot_against_an_unpublished_lake_still_succeeds() {
    let dirs = fresh_lake_dirs();
    let data_dir = TempDir::new().unwrap();
    let cfg = writer_cfg(&data_dir, &dirs, 0);
    let booted = cfg
        .build()
        .await
        .expect("first writer boot must succeed on an empty lake");
    let base = format!("http://{}", booted.handle.local_addr);

    let resp = call(&base, "list_skills", serde_json::json!({})).await;
    let skills = resp["result"]["structuredContent"]["skills"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(skills.len(), 1, "only the mandatory meta-skill: {resp}");

    booted.handle.shutdown().await;
}
