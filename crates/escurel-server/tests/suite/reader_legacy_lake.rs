//! A DuckLake READER next to a WRITER whose tenant still holds legacy `type:` pages.
//!
//! A reader has no lane and no local index: it adopts the newest snapshot a writer PUBLISHED into the
//! lake, so the legacy quarantine (a writer-boot scan of the lane) cannot protect it directly. What
//! protects it is that a quarantined writer must not publish: its index was never, or only partly,
//! derived from the lane, and a snapshot of it would hand readers an empty or stale corpus that looks
//! healthy. Two REAL `escurel-server` processes over a real DuckDB-file catalog and a local Parquet
//! DATA_PATH (the offline DuckLake shape; no Docker, no mocks):
//!
//! 1. the quarantined writer ticks its periodic publish for several seconds and publishes NOTHING:
//!    a reader pointed at the lake refuses to boot ("never been published") instead of serving an
//!    empty corpus;
//! 2. after `migrate_kind --apply` the writer publishes the migrated corpus and the reader serves
//!    exactly that, none of it quarantined.

use std::io::{BufRead, BufReader, Read as _};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tempfile::TempDir;

const SKILLS: usize = 3;
const INSTANCES: usize = 60;

fn seed_legacy_lane(data_dir: &Path) {
    let root = data_dir.join("tenants/default");
    for s in 0..SKILLS {
        std::fs::create_dir_all(root.join("markdown/skills")).unwrap();
        std::fs::write(
            root.join(format!("markdown/skills/s{s}.md")),
            format!("---\ntype: skill\nid: s{s}\ndescription: skill {s}\n---\n# s{s}\n"),
        )
        .unwrap();
        std::fs::create_dir_all(root.join(format!("markdown/instances/s{s}"))).unwrap();
    }
    for i in 0..INSTANCES {
        std::fs::write(
            root.join(format!("markdown/instances/s{}/i{i}.md", i % SKILLS)),
            format!(
                "---\ntype: instance\nskill: s{}\nid: i{i}\n---\n# Instance {i}\n\nBody {i}.\n",
                i % SKILLS
            ),
        )
        .unwrap();
    }
}

struct Server {
    child: Child,
    base: String,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Spawn the real binary; `Err(stderr)` when it exits before it listens (a refused boot).
fn try_spawn(data_dir: &Path, role: &str, lake: &Path) -> Result<Server, String> {
    use assert_cmd::cargo::CommandCargoExt as _;
    let mut child = Command::cargo_bin("escurel-server")
        .expect("locate escurel-server")
        .env("ESCUREL_SERVER_DATA_DIR", data_dir)
        .env("ESCUREL_SERVER_LISTEN_HTTP", "127.0.0.1:0")
        .env("ESCUREL_OBSERVABILITY_METRICS_LISTEN", "127.0.0.1:0")
        .env("ESCUREL_EMBEDDING_PROVIDER", "zero")
        .env("ESCUREL_INDEX_BACKEND", "ducklake")
        .env("ESCUREL_ROLE", role)
        .env(
            "ESCUREL_DUCKLAKE_CATALOG_DSN",
            lake.join("catalog.ducklake"),
        )
        .env("ESCUREL_DUCKLAKE_DATA_PATH", lake.join("data"))
        .env("ESCUREL_SNAPSHOT_PUBLISH_SECS", "1")
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
        let n = reader.read_line(&mut line).expect("read server stdout");
        if n == 0 {
            let _ = child.wait();
            let mut err = String::new();
            let _ = stderr.read_to_string(&mut err);
            return Err(err);
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
            return Ok(Server {
                child,
                base: format!("http://{rest}"),
            });
        }
    }
}

async fn tool(base: &str, name: &str, args: Value) -> Result<Value, String> {
    let body: Value = reqwest::Client::new()
        .post(format!("{base}/mcp"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": args },
        }))
        .timeout(Duration::from_secs(600))
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

async fn served_counts(base: &str) -> Result<(usize, usize), String> {
    let skills = tool(base, "list_skills", json!({})).await?;
    let ids: Vec<String> = skills["skills"]
        .as_array()
        .ok_or("no skills array")?
        .iter()
        .filter_map(|s| s["id"].as_str().map(str::to_owned))
        .filter(|id| id.starts_with('s') && id[1..].chars().all(|c| c.is_ascii_digit()))
        .collect();
    let mut total = 0;
    for id in &ids {
        let page = tool(
            base,
            "list_instances",
            json!({ "skill_id": id, "limit": 1000 }),
        )
        .await?;
        total += page["instances"].as_array().map_or(0, Vec::len);
    }
    Ok((ids.len(), total))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_quarantined_writer_publishes_nothing_so_a_reader_never_serves_an_empty_or_legacy_corpus()
{
    let writer_dir = TempDir::new().unwrap();
    let reader_dir = TempDir::new().unwrap();
    let lake = TempDir::new().unwrap();
    std::fs::create_dir_all(lake.path().join("data")).unwrap();
    seed_legacy_lane(writer_dir.path());

    // The writer boots over the legacy lane: quarantined, and its periodic publish (every second)
    // starts ticking.
    let writer = try_spawn(writer_dir.path(), "writer", lake.path())
        .unwrap_or_else(|e| panic!("the writer must boot (quarantined): {e}"));
    let ready: Value = reqwest::get(format!("{}/readyz", writer.base))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        ready.to_string().contains("\"quarantined\":true"),
        "{ready}"
    );

    // Let several publish ticks pass. A reader must still find NOTHING to adopt.
    tokio::time::sleep(Duration::from_secs(6)).await;
    match try_spawn(reader_dir.path(), "reader", lake.path()) {
        Ok(reader) => {
            let counts = served_counts(&reader.base).await;
            panic!(
                "a reader booted from a lake published by a QUARANTINED writer and serves {counts:?}: \
                 an empty or stale corpus that looks healthy"
            );
        }
        Err(stderr) => assert!(
            stderr.contains("never been published"),
            "the reader should refuse with 'never been published', got:\n{stderr}"
        ),
    }

    // Migrate on the writer; once the migrated corpus is published, a reader serves exactly it.
    let applied = tool(&writer.base, "migrate_kind", json!({ "apply": true }))
        .await
        .expect("migrate_kind --apply");
    assert_eq!(applied["tenant_quarantined"], json!(false), "{applied}");

    let deadline = Instant::now() + Duration::from_secs(120);
    let reader = loop {
        match try_spawn(reader_dir.path(), "reader", lake.path()) {
            Ok(r) => break r,
            Err(e) => {
                assert!(
                    e.contains("never been published"),
                    "unexpected reader boot failure:\n{e}"
                );
                assert!(
                    Instant::now() < deadline,
                    "the migrated corpus was never published"
                );
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    };
    let counts = loop {
        match served_counts(&reader.base).await {
            Ok(c) if c == (SKILLS, INSTANCES) => break c,
            other => {
                assert!(
                    Instant::now() < deadline,
                    "the reader never served the full migrated corpus: {other:?}"
                );
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    };
    assert_eq!(counts, (SKILLS, INSTANCES));
    let ready: Value = reqwest::get(format!("{}/readyz", reader.base))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        !ready.to_string().contains("\"quarantined\":true"),
        "{ready}"
    );
}
