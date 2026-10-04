//! `escurel admin migrate-kind --apply` through the REAL `escurel` CLI against the REAL
//! `escurel-server` process, on a tenant whose migration takes longer than the CLI's old fixed 60 s
//! request timeout.
//!
//! Round-2 review finding: at ~8,000 legacy pages the CLI died at exactly 60.0 s with
//! `transport error`, the server dropped the request mid-snapshot stage, the tenant stayed
//! quarantined and a second run timed out the same way. The properties pinned here:
//!
//! * the apply runs in a SPAWNED server task: a client that gives up (`--timeout-secs`) or is killed
//!   never cancels a half-done migration; the tenant ends migrated and serving;
//! * the CLI names the situation (the migration continues on the server) instead of a bare
//!   `transport error`;
//! * a slow tenant needs no flag: the default has no total deadline for this command.
//!
//! A server-side test knob (`ESCUREL_TEST_MIGRATE_KIND_PAGE_DELAY_MS`) slows each page so a CI-sized
//! tenant has a long migration window.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

const TENANT: &str = "default";
const PAGES: usize = 40;
const DELAY_MS: &str = "150";

fn lane_root(data_dir: &Path) -> PathBuf {
    data_dir.join("tenants").join(TENANT)
}

fn seed_legacy_lane(data_dir: &Path) {
    let root = lane_root(data_dir);
    let skills = root.join("markdown/skills");
    std::fs::create_dir_all(&skills).unwrap();
    std::fs::write(
        skills.join("s0.md"),
        "---\ntype: skill\nid: s0\ndescription: skill\n---\n# s0\n",
    )
    .unwrap();
    let inst = root.join("markdown/instances/s0");
    std::fs::create_dir_all(&inst).unwrap();
    for i in 0..PAGES {
        std::fs::write(
            inst.join(format!("i{i}.md")),
            format!("---\ntype: instance\nskill: s0\nid: i{i}\n---\n# Instance {i}\n"),
        )
        .unwrap();
    }
}

fn legacy_pages(data_dir: &Path) -> usize {
    let dir = lane_root(data_dir).join("markdown");
    let mut n = 0;
    let mut stack = vec![dir];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if let Ok(t) = std::fs::read_to_string(&p)
                && t.lines()
                    .any(|l| l == "type: instance" || l == "type: skill")
            {
                n += 1;
            }
        }
    }
    n
}

#[allow(clippy::zombie_processes)]
fn spawn_server(data_dir: &Path) -> (Child, String) {
    use assert_cmd::cargo::CommandCargoExt as _;
    let mut child = Command::cargo_bin("escurel-server")
        .expect("locate escurel-server")
        .env("ESCUREL_SERVER_DATA_DIR", data_dir)
        .env("ESCUREL_SERVER_LISTEN_HTTP", "127.0.0.1:0")
        .env("ESCUREL_OBSERVABILITY_METRICS_LISTEN", "127.0.0.1:0")
        .env("ESCUREL_EMBEDDING_PROVIDER", "zero")
        .env("ESCUREL_TEST_MIGRATE_KIND_PAGE_DELAY_MS", DELAY_MS)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn escurel-server");
    let mut reader = BufReader::new(child.stdout.take().expect("piped stdout"));
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).expect("read server stdout") == 0 {
            panic!("escurel-server exited before it listened");
        }
        if let Some(rest) = line.trim().strip_prefix("escurel-server listening http=") {
            std::thread::spawn(move || {
                let mut sink = String::new();
                while reader.read_line(&mut sink).map(|n| n > 0).unwrap_or(false) {
                    sink.clear();
                }
            });
            return (child, format!("http://{rest}"));
        }
    }
}

/// The compiled `escurel` CLI: it lives next to `escurel-server` in the same target dir.
fn cli_path() -> PathBuf {
    use assert_cmd::cargo::CommandCargoExt as _;
    let server = Command::cargo_bin("escurel-server").unwrap();
    let dir = Path::new(server.get_program()).parent().unwrap().to_owned();
    let cli = dir.join("escurel");
    if !cli.exists() {
        let status = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
            .args(["build", "-p", "escurel-cli", "--bin", "escurel"])
            .status()
            .expect("cargo build escurel-cli");
        assert!(status.success(), "building the escurel CLI failed");
    }
    assert!(cli.exists(), "escurel CLI not found at {}", cli.display());
    cli
}

async fn quarantined(base: &str) -> bool {
    let v: serde_json::Value = reqwest::get(format!("{base}/readyz"))
        .await
        .expect("GET /readyz")
        .json()
        .await
        .expect("readyz json");
    v.to_string().contains("\"quarantined\":true")
}

async fn wait_until_served(base: &str, within: Duration) {
    let deadline = Instant::now() + within;
    while quarantined(base).await {
        assert!(Instant::now() < deadline, "tenant still quarantined");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn cli(base: &str, args: &[&str]) -> Command {
    let mut c = Command::new(cli_path());
    c.env("ESCUREL_SERVER", base)
        .env_remove("ESCUREL_TOKEN")
        .args(["admin", "migrate-kind", "--tenant", TENANT, "--apply"])
        .args(args);
    c
}

#[tokio::test]
async fn a_client_that_gives_up_does_not_cancel_the_migration() {
    let dir = TempDir::new().unwrap();
    seed_legacy_lane(dir.path());
    let (mut server, base) = spawn_server(dir.path());
    assert!(quarantined(&base).await, "legacy tenant boots quarantined");

    let started = Instant::now();
    let out = cli(&base, &["--timeout-secs", "2"]).output().unwrap();
    assert!(!out.status.success(), "the CLI gave up: non-zero exit");
    assert!(started.elapsed() < Duration::from_secs(20));
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        err.contains("still running") || err.contains("continues on the server"),
        "the CLI must say the migration continues server-side, got: {err}"
    );

    wait_until_served(&base, Duration::from_secs(120)).await;
    assert_eq!(legacy_pages(dir.path()), 0, "every page was migrated");
    let _ = server.kill();
    let _ = server.wait();
}

#[tokio::test]
async fn killing_the_cli_mid_apply_still_completes_the_migration() {
    let dir = TempDir::new().unwrap();
    seed_legacy_lane(dir.path());
    let (mut server, base) = spawn_server(dir.path());

    let mut child = cli(&base, &[])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    // Let the apply start, then kill the client.
    let marker = lane_root(dir.path()).join("meta/migrate-kind.pending");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !marker.exists() {
        assert!(Instant::now() < deadline, "migration never started");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    child.kill().unwrap();
    let _ = child.wait();

    wait_until_served(&base, Duration::from_secs(120)).await;
    assert_eq!(legacy_pages(dir.path()), 0);
    assert!(!marker.exists(), "marker cleared by the finished migration");
    let _ = server.kill();
    let _ = server.wait();
}

#[tokio::test]
async fn a_slow_tenant_needs_no_timeout_flag() {
    let dir = TempDir::new().unwrap();
    seed_legacy_lane(dir.path());
    let (mut server, base) = spawn_server(dir.path());
    let out = cli(&base, &[]).output().unwrap();
    assert!(
        out.status.success(),
        "apply failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("\"tenant_quarantined\": false"));
    let _ = server.kill();
    let _ = server.wait();
}
