//! The lab-cutover rehearsal of 2026-10-14, through the REAL `escurel-server` binary over a REAL
//! DuckLake (DuckDB-file catalog + local Parquet DATA_PATH; one process at a time, so no Postgres).
//!
//! The lane is migrated (`kind:`) but the lake still holds the `type:` corpus an older writer
//! published. Before the fix the next writer boot adopted that stale corpus over the migrated lane and
//! served `type:` frontmatter silently (`tenant_quarantined 0`); `migrate-kind --apply` left the lake
//! stale, so a restart reintroduced it. Now:
//!
//! 1. the writer boot QUARANTINES the tenant (it never serves the stale corpus);
//! 2. `migrate_kind --apply` re-derives the index from the lane, lifts the quarantine and PUBLISHES
//!    the lake itself (the report says so), so the lake holds no `type:` page;
//! 3. a restart over that lake serves the migrated corpus, not quarantined.

use std::io::{BufRead, BufReader, Read as _};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use duckdb::Connection;
use escurel_index::snapshot::{LakeConfig, ObjectStoreSecret, attach_lake};
use serde_json::{Value, json};
use tempfile::TempDir;

pub(crate) const SKILLS: usize = 2;
pub(crate) const INSTANCES: usize = 12;

pub(crate) fn seed_lane(data_dir: &Path) {
    let root = data_dir.join("tenants/default");
    for s in 0..SKILLS {
        std::fs::create_dir_all(root.join("markdown/skills")).unwrap();
        std::fs::write(
            root.join(format!("markdown/skills/s{s}.md")),
            format!("---\nkind: skill\nid: s{s}\ndescription: skill {s}\n---\n# s{s}\n"),
        )
        .unwrap();
        std::fs::create_dir_all(root.join(format!("markdown/instances/s{s}"))).unwrap();
    }
    for i in 0..INSTANCES {
        std::fs::write(
            root.join(format!("markdown/instances/s{}/i{i}.md", i % SKILLS)),
            format!(
                "---\nkind: instance\nskill: s{}\nid: i{i}\n---\n# Instance {i}\n\nBody {i}.\n",
                i % SKILLS
            ),
        )
        .unwrap();
    }
}

pub(crate) struct Server {
    child: Child,
    pub(crate) base: String,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[allow(clippy::zombie_processes)] // the child is kept in `Server`, whose Drop kills and waits
pub(crate) fn spawn(data_dir: &Path, lake: &LakeConfig) -> Server {
    use assert_cmd::cargo::CommandCargoExt as _;
    let mut child = Command::cargo_bin("escurel-server")
        .expect("locate escurel-server")
        .env("ESCUREL_SERVER_DATA_DIR", data_dir)
        .env("ESCUREL_SERVER_LISTEN_HTTP", "127.0.0.1:0")
        .env("ESCUREL_OBSERVABILITY_METRICS_LISTEN", "127.0.0.1:0")
        .env("ESCUREL_EMBEDDING_PROVIDER", "zero")
        .env("ESCUREL_INDEX_BACKEND", "ducklake")
        .env("ESCUREL_ROLE", "writer")
        .env("ESCUREL_DUCKLAKE_CATALOG_DSN", &lake.catalog_dsn)
        .env("ESCUREL_DUCKLAKE_DATA_PATH", &lake.data_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn escurel-server");
    let stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).expect("read stdout") == 0 {
            let _ = child.wait();
            let mut err = String::new();
            let _ = stderr.read_to_string(&mut err);
            panic!("escurel-server exited before listening:\n{err}");
        }
        if let Some(rest) = line.trim().strip_prefix("escurel-server listening http=") {
            std::thread::spawn(move || {
                let mut sink = String::new();
                while reader.read_line(&mut sink).map(|n| n > 0).unwrap_or(false) {
                    sink.clear();
                }
            });
            std::thread::spawn(move || {
                let mut sink = Vec::new();
                let _ = stderr.read_to_end(&mut sink);
            });
            return Server {
                child,
                base: format!("http://{rest}"),
            };
        }
    }
}

pub(crate) async fn call(base: &str, name: &str, args: Value) -> Result<Value, String> {
    let body: Value = reqwest::Client::new()
        .post(format!("{base}/mcp"))
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":args}}))
        .timeout(Duration::from_secs(300))
        .send()
        .await
        .map_err(|e| format!("transport: {e}"))?
        .json()
        .await
        .map_err(|e| format!("body: {e}"))?;
    if let Some(err) = body.get("error") {
        return Err(err.to_string());
    }
    Ok(body["result"]["structuredContent"].clone())
}

pub(crate) fn lake_count(cfg: &LakeConfig, sql: &str) -> i64 {
    let conn = Connection::open_in_memory().unwrap();
    attach_lake(&conn, cfg, true).unwrap();
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

#[tokio::test]
async fn a_stale_lake_never_serves_and_migrate_kind_apply_leaves_the_lake_consistent() {
    let data = TempDir::new().unwrap();
    let lake_dir = TempDir::new().unwrap();
    std::fs::create_dir_all(lake_dir.path().join("data")).unwrap();
    let lake = LakeConfig {
        catalog_dsn: lake_dir
            .path()
            .join("catalog.ducklake")
            .to_str()
            .unwrap()
            .to_owned(),
        data_path: lake_dir.path().join("data").to_str().unwrap().to_owned(),
        object_store: ObjectStoreSecret::None,
    };
    seed_lane(data.path());
    let total = (SKILLS + INSTANCES) as i64;

    // Boot 1 builds the corpus from the (`kind:`) lane and publishes it: a consistent lake.
    {
        let s = spawn(data.path(), &lake);
        call(&s.base, "publish_snapshot", json!({}))
            .await
            .expect("publish");
    }
    assert!(lake_count(&lake, "SELECT count(*) FROM lake.pages") >= total);
    // Make the lake STALE: the shape an older writer published.
    {
        let conn = Connection::open_in_memory().unwrap();
        attach_lake(&conn, &lake, false).unwrap();
        conn.execute_batch(
            "UPDATE lake.pages SET frontmatter = json_object('type', json_extract_string(frontmatter, '$.kind'), \
             'id', json_extract_string(frontmatter, '$.id')) WHERE json_extract(frontmatter, '$.kind') IS NOT NULL;",
        )
        .unwrap();
    }
    let stale = lake_count(
        &lake,
        "SELECT count(*) FROM lake.pages WHERE json_extract(frontmatter, '$.type') IS NOT NULL",
    );
    assert!(stale > 0, "precondition: the lake is stale");

    // Boot 2 adopts the stale lake: it must NOT serve it.
    {
        let s = spawn(data.path(), &lake);
        let err = call(&s.base, "list_skills", json!({}))
            .await
            .expect_err("a stale adopted lake must not be served");
        assert!(err.contains("tenant_quarantined"), "got: {err}");

        let report = call(&s.base, "migrate_kind", json!({"apply": true}))
            .await
            .expect("apply");
        assert_eq!(report["tenant_quarantined"], false, "{report}");
        assert!(
            report["lake_published"]["pages"].as_i64().unwrap_or(0) >= total,
            "the apply must publish the lake itself and say so: {report}"
        );
        call(&s.base, "list_skills", json!({}))
            .await
            .expect("serving again");
    }
    assert_eq!(
        lake_count(
            &lake,
            "SELECT count(*) FROM lake.pages WHERE json_extract(frontmatter, '$.type') IS NOT NULL"
        ),
        0,
        "no `type:` page may remain in the lake after the apply"
    );

    // Boot 3 over the repaired lake: not quarantined, serves the migrated corpus.
    {
        let s = spawn(data.path(), &lake);
        let skills = call(&s.base, "list_skills", json!({}))
            .await
            .expect("serves after restart");
        let ids: Vec<&str> = skills["skills"]
            .as_array()
            .map(|a| a.iter().filter_map(|s| s["id"].as_str()).collect())
            .unwrap_or_default();
        assert!(ids.contains(&"s0") && ids.contains(&"s1"), "{skills}");
    }
}

/// `publish_snapshot` through the real binary: a corpus far smaller than the lake it would overwrite
/// (here a lane that lost most of its pages, then an admin `rebuild`) is refused, and only an explicit
/// `allow_shrink` lets a deliberate mass delete through.
#[tokio::test]
async fn publish_snapshot_refuses_a_shrunk_corpus_unless_allow_shrink_is_passed() {
    let data = TempDir::new().unwrap();
    let lake_dir = TempDir::new().unwrap();
    std::fs::create_dir_all(lake_dir.path().join("data")).unwrap();
    let lake = LakeConfig {
        catalog_dsn: lake_dir
            .path()
            .join("catalog.ducklake")
            .to_str()
            .unwrap()
            .to_owned(),
        data_path: lake_dir.path().join("data").to_str().unwrap().to_owned(),
        object_store: ObjectStoreSecret::None,
    };
    seed_lane(data.path());
    {
        let s = spawn(data.path(), &lake);
        call(&s.base, "publish_snapshot", json!({}))
            .await
            .expect("publish");
    }
    let before = lake_count(&lake, "SELECT count(*) FROM lake.pages");
    // The lane loses 10 of its 12 instances (an interrupted restore, say).
    for i in 0..10 {
        let _ = std::fs::remove_file(data.path().join(format!(
            "tenants/default/markdown/instances/s{}/i{i}.md",
            i % SKILLS
        )));
    }
    let s = spawn(data.path(), &lake);
    call(&s.base, "rebuild", json!({}))
        .await
        .expect("rebuild from the shrunk lane");
    let err = call(&s.base, "publish_snapshot", json!({}))
        .await
        .expect_err("a corpus under half the lake must be refused");
    assert!(err.contains("allow_shrink"), "got: {err}");

    call(&s.base, "publish_snapshot", json!({"allow_shrink": true}))
        .await
        .expect("a deliberate shrink goes through");
    drop(s); // the DuckDB-file catalog is locked while the server runs
    assert!(lake_count(&lake, "SELECT count(*) FROM lake.pages") < before);
}
