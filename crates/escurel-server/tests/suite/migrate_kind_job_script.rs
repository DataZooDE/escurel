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
    let server = Command::cargo_bin("escurel-server").unwrap();
    let dir = Path::new(server.get_program()).parent().unwrap().to_owned();
    if !dir.join("escurel").exists() {
        let st = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
            .args(["build", "-p", "escurel-cli", "--bin", "escurel"])
            .status()
            .unwrap();
        assert!(st.success());
    }
    dir
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
    let started = Instant::now();
    let mut c = Command::new("sh");
    c.arg(repo_root().join("scripts/migrate-kind-job.sh"))
        .env("ESCUREL_BIN_DIR", bin_dir())
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
    assert!(String::from_utf8_lossy(&out.stderr).contains("still quarantined"));
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
