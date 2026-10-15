//! `scripts/migrate-kind-job.sh` (the one-shot migration job of docs/deploy/kind-migration.md §3)
//! against the REAL `escurel-server` + `escurel` binaries. The round-2 review found the old inline
//! script reported success when the migration had failed (no `set -e`, exit code of `wait $S`) and
//! looped forever when the server died at boot. Three outcomes are pinned.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use tempfile::TempDir;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn bin_dir() -> PathBuf {
    use assert_cmd::cargo::CommandCargoExt as _;
    // A different package: `cargo test -p escurel-server` does not rebuild the CLI, and a stale
    // binary left by an earlier build would be tested instead. Build it (a no-op when fresh).
    static BUILT: std::sync::Once = std::sync::Once::new();
    BUILT.call_once(|| {
        let st = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
            .args(["build", "-p", "escurel-cli", "--bin", "escurel"])
            .status()
            .unwrap();
        assert!(st.success());
    });
    let server = Command::cargo_bin("escurel-server").unwrap();
    Path::new(server.get_program()).parent().unwrap().to_owned()
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn seed(data: &Path, extra: &[(&str, &str)]) {
    let root = data.join("tenants/default/markdown");
    std::fs::create_dir_all(root.join("skills")).unwrap();
    std::fs::create_dir_all(root.join("instances/s0")).unwrap();
    std::fs::write(
        root.join("skills/s0.md"),
        "---\ntype: skill\nid: s0\ndescription: d\n---\n# s0\n",
    )
    .unwrap();
    std::fs::write(
        root.join("instances/s0/a.md"),
        "---\ntype: instance\nskill: s0\nid: a\n---\n# a\n",
    )
    .unwrap();
    for (name, body) in extra {
        std::fs::write(root.join("instances/s0").join(name), body).unwrap();
    }
}

fn job(data: &Path, deadline: u64, env: &[(&str, &str)]) -> (std::process::Output, Duration) {
    let bin = bin_dir(); // build the CLI BEFORE the clock starts
    let started = Instant::now();
    let mut c = Command::new("sh");
    c.arg(repo_root().join("scripts/migrate-kind-job.sh"))
        .env("ESCUREL_BIN_DIR", bin)
        .env("ESCUREL_TENANT", "default")
        .env("ESCUREL_JOB_PORT", free_port().to_string())
        .env("ESCUREL_JOB_DEADLINE_SECS", deadline.to_string())
        .env("ESCUREL_SERVER_DATA_DIR", data)
        .env("ESCUREL_EMBEDDING_PROVIDER", "zero");
    for (k, v) in env {
        c.env(k, v);
    }
    let out = c.output().unwrap();
    (out, started.elapsed())
}

#[test]
fn success_exits_zero_and_the_tenant_is_migrated() {
    let dir = TempDir::new().unwrap();
    seed(dir.path(), &[]);
    let (out, _) = job(dir.path(), 300, &[]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let page = std::fs::read_to_string(
        dir.path()
            .join("tenants/default/markdown/instances/s0/a.md"),
    )
    .unwrap();
    assert!(page.contains("kind: instance") && !page.contains("type: instance"));
}

#[test]
fn a_both_keys_conflict_exits_non_zero() {
    let dir = TempDir::new().unwrap();
    seed(
        dir.path(),
        &[(
            "both.md",
            "---\ntype: instance\nkind: reseller\nskill: s0\nid: both\n---\n# b\n",
        )],
    );
    let (out, _) = job(dir.path(), 300, &[]);
    assert!(
        !out.status.success(),
        "a conflict leaves the tenant quarantined: the job must fail\nstdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    // The dry run now catches it BEFORE the apply (nothing is rewritten for a tenant that cannot finish).
    assert!(String::from_utf8_lossy(&out.stderr).contains("dry run reports conflicts"));
}

#[test]
fn a_server_that_dies_at_boot_fails_fast_instead_of_looping() {
    let dir = TempDir::new().unwrap();
    seed(dir.path(), &[]);
    let (out, took) = job(dir.path(), 60, &[("ESCUREL_EGRESS_TIMEOUT_MS", "5s")]);
    assert!(!out.status.success(), "boot failure must fail the job");
    assert!(took < Duration::from_secs(30), "took {took:?}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("exited during boot"));
}

// ---- DuckLake: the job keeps the LAKE consistent (2026-10-14 rehearsal) ----

use crate::stale_lake_boot::{INSTANCES, SKILLS, call, lake_count, seed_lane, spawn};
use duckdb::Connection;
use escurel_index::snapshot::{LakeConfig, ObjectStoreSecret, attach_lake};

fn local_lake(dir: &Path) -> LakeConfig {
    std::fs::create_dir_all(dir.join("data")).unwrap();
    LakeConfig {
        catalog_dsn: dir.join("catalog.ducklake").to_str().unwrap().to_owned(),
        data_path: dir.join("data").to_str().unwrap().to_owned(),
        object_store: ObjectStoreSecret::None,
    }
}

/// Rewrite every lane page back to the removed `type:` key (the shape a pre-cutover lane has).
fn make_lane_legacy(data: &Path) {
    fn walk(dir: &Path) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(&p);
            } else if p.extension().is_some_and(|x| x == "md") {
                let body = std::fs::read_to_string(&p).unwrap();
                std::fs::write(&p, body.replacen("kind:", "type:", 1)).unwrap();
            }
        }
    }
    walk(&data.join("tenants/default/markdown"));
}

/// A lake published by the OLD engine: `type:` frontmatter and manifest schema 9.
fn make_lake_old(cfg: &LakeConfig, schema_version: i64) {
    let conn = Connection::open_in_memory().unwrap();
    attach_lake(&conn, cfg, false).unwrap();
    conn.execute_batch(&format!(
        "UPDATE lake.pages SET frontmatter = json_object('type', json_extract_string(frontmatter, '$.kind'), \
         'id', json_extract_string(frontmatter, '$.id')) WHERE json_extract(frontmatter, '$.kind') IS NOT NULL; \
         UPDATE lake.escurel_manifest SET schema_version = {schema_version};"
    ))
    .unwrap();
}

async fn publish_a_lake_from_a_kind_lane(data: &Path, cfg: &LakeConfig) {
    seed_lane(data);
    let s = spawn(data, cfg);
    call(&s.base, "publish_snapshot", serde_json::json!({}))
        .await
        .expect("publish");
}

fn lake_job(data: &Path, cfg: &LakeConfig) -> std::process::Output {
    job(
        data,
        900,
        &[
            ("ESCUREL_INDEX_BACKEND", "ducklake"),
            ("ESCUREL_ROLE", "writer"),
            ("ESCUREL_DUCKLAKE_CATALOG_DSN", &cfg.catalog_dsn),
            ("ESCUREL_DUCKLAKE_DATA_PATH", &cfg.data_path),
        ],
    )
    .0
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_ducklake_job_upgrades_the_lake_migrates_the_lane_and_leaves_the_lake_consistent() {
    let data = TempDir::new().unwrap();
    let lake_dir = TempDir::new().unwrap();
    let cfg = local_lake(lake_dir.path());
    publish_a_lake_from_a_kind_lane(data.path(), &cfg).await;
    make_lake_old(&cfg, 9);
    make_lane_legacy(data.path());

    let out = tokio::task::spawn_blocking({
        let (d, c) = (data.path().to_owned(), cfg.clone());
        move || lake_job(&d, &c)
    })
    .await
    .unwrap();
    assert!(
        out.status.success(),
        "the lake job must succeed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("\"upgraded\""),
        "lake-upgrade ran"
    );
    assert_eq!(
        lake_count(
            &cfg,
            "SELECT count(*) FROM lake.pages WHERE json_extract(frontmatter, '$.type') IS NOT NULL"
        ),
        0,
        "no `type:` page may remain in the lake"
    );
    assert!(lake_count(&cfg, "SELECT count(*) FROM lake.pages") >= (SKILLS + INSTANCES) as i64);
    assert_eq!(
        lake_count(&cfg, "SELECT schema_version FROM lake.escurel_manifest"),
        10
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_ducklake_job_refuses_a_lake_newer_than_the_build_and_migrates_nothing() {
    let data = TempDir::new().unwrap();
    let lake_dir = TempDir::new().unwrap();
    let cfg = local_lake(lake_dir.path());
    publish_a_lake_from_a_kind_lane(data.path(), &cfg).await;
    make_lake_old(&cfg, 99);
    make_lane_legacy(data.path());
    let page = data.path().join("tenants/default/markdown/skills/s0.md");

    let out = tokio::task::spawn_blocking({
        let (d, c) = (data.path().to_owned(), cfg.clone());
        move || lake_job(&d, &c)
    })
    .await
    .unwrap();
    assert!(!out.status.success(), "a newer lake must fail the job");
    assert!(String::from_utf8_lossy(&out.stderr).contains("lake-upgrade failed"));
    assert!(
        std::fs::read_to_string(page).unwrap().contains("type:"),
        "nothing may be migrated when the lake step refuses"
    );
}

/// The lab cutover of 2026-10-15: the Job's data dir (also its `HOME`) is a FRESH emptyDir path that does not
/// exist yet, and the first step (`lake-upgrade`) died with `Can't find the home directory`. The job must get
/// past that first step on a volume nothing has created.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_ducklake_job_gets_past_lake_upgrade_on_a_fresh_volume() {
    let seed_dir = TempDir::new().unwrap();
    let lake_dir = TempDir::new().unwrap();
    let cfg = local_lake(lake_dir.path());
    publish_a_lake_from_a_kind_lane(seed_dir.path(), &cfg).await;
    make_lake_old(&cfg, 9);

    let fresh = TempDir::new().unwrap();
    let data = fresh.path().join("escurel");
    assert!(!data.exists());
    let out = tokio::task::spawn_blocking({
        let (d, c) = (data.clone(), cfg.clone());
        move || {
            job(
                &d,
                900,
                &[
                    ("ESCUREL_INDEX_BACKEND", "ducklake"),
                    ("ESCUREL_ROLE", "writer"),
                    ("ESCUREL_DUCKLAKE_CATALOG_DSN", &c.catalog_dsn),
                    ("ESCUREL_DUCKLAKE_DATA_PATH", &c.data_path),
                    ("HOME", d.to_str().unwrap()),
                ],
            )
            .0
        }
    })
    .await
    .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("Can't find the home directory"),
        "the job tripped on the missing home on a fresh volume\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("\"upgraded\""),
        "lake-upgrade must have run and upgraded the lake\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert_eq!(
        lake_count(&cfg, "SELECT schema_version FROM lake.escurel_manifest"),
        10
    );
}
