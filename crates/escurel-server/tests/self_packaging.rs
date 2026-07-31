//! ADR-0011 increment 2 — the real end-to-end: pack a markdown corpus
//! into a copy of the built `escurel-server`, then run that bundled
//! binary and confirm it seeds + serves the corpus. No mocks: the actual
//! release binary, packed and re-executed.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::TempDir;

/// Path to the freshly-built `escurel-server` (cargo sets this for the
/// crate's own binaries).
const BIN: &str = env!("CARGO_BIN_EXE_escurel-server");

#[tokio::test]
async fn pack_then_run_seeds_and_serves_the_bundled_corpus() {
    let work = TempDir::new().unwrap();

    // A tiny corpus: one skill.
    let corpus = work.path().join("corpus");
    std::fs::create_dir_all(corpus.join("skills")).unwrap();
    std::fs::write(
        corpus.join("skills/demo.md"),
        "---\ntype: skill\nid: demo\ndescription: seeded via self-packaging.\n---\n# demo\n",
    )
    .unwrap();

    // 1. pack it into a copy of the server binary.
    let bundled = work.path().join("escurel-server-demo");
    let out = Command::new(BIN)
        .args([
            "pack",
            "--in",
            corpus.to_str().unwrap(),
            "--out",
            bundled.to_str().unwrap(),
        ])
        .output()
        .expect("run pack");
    assert!(
        out.status.success(),
        "pack failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // 2. `info` on the bundled binary lists the corpus.
    let info = Command::new(&bundled)
        .arg("info")
        .output()
        .expect("run info");
    let info_s = String::from_utf8_lossy(&info.stdout);
    assert!(info_s.contains("skills/demo.md"), "info output: {info_s}");

    // 3. run the bundled binary — it must seed `demo` at boot (dev mode: no
    //    OIDC, ephemeral ports) and serve it.
    let data = work.path().join("data");
    let mut child = Command::new(&bundled)
        .env("ESCUREL_SERVER_DATA_DIR", &data)
        .env("ESCUREL_SERVER_LISTEN_HTTP", "127.0.0.1:0")
        .env("ESCUREL_OBSERVABILITY_METRICS_LISTEN", "127.0.0.1:0")
        .env("ESCUREL_TENANT", "demo")
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn bundled server");

    // Read stdout on a thread: capture the bound address, then keep
    // draining so the pipe never blocks the server.
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Some(rest) = line.strip_prefix("escurel-server listening http=") {
                let _ = tx.send(rest.trim().to_string());
            }
        }
    });
    let addr = rx
        .recv_timeout(Duration::from_secs(90))
        .expect("server printed its listen address");

    // 4. list_skills over /mcp (dev mode → no bearer) must include `demo`.
    let resp: Value = reqwest::Client::new()
        .post(format!("http://{addr}/mcp"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "list_skills", "arguments": {} },
        }))
        .send()
        .await
        .expect("post list_skills")
        .json()
        .await
        .expect("json");
    let ids: Vec<String> = resp["result"]["structuredContent"]["skills"]
        .as_array()
        .expect("skills array")
        .iter()
        .filter_map(|s| s["id"].as_str().map(str::to_owned))
        .collect();
    assert!(
        ids.iter().any(|i| i == "demo"),
        "the packed skill was seeded + served: {ids:?}"
    );

    let _ = child.kill();
    let _ = child.wait();
}
