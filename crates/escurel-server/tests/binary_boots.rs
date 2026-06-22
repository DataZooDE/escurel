//! Integration tests for the deployable `escurel-server` binary and
//! its `EscurelConfig` surface.
//!
//! Two layers, no mocks:
//!
//! * `EscurelConfig::from_source` over an in-memory env map — the
//!   12-factor mapping from `ESCUREL_*` vars to resolved config. Uses
//!   an injected map rather than `std::env::set_var` (process-global,
//!   races concurrent test threads).
//! * A real spawned `escurel-server` child process bound to a random
//!   loopback port over a real on-disk DuckDB + FsStore, dialled back
//!   with a real reqwest client — the "single-binary on a laptop"
//!   acceptance.
//! * An in-process `config.build()` exercising the degraded-embedder
//!   start: the test binary lacks the `gemini` feature, so selecting
//!   `provider=gemini` degrades to a `ZeroEmbedder` placeholder and
//!   `/readyz` must report `embedder: false` while `/healthz` stays
//!   `200`.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::Duration;

use escurel_server::EscurelConfig;
use escurel_server::config::{EmbeddingProvider, StorageBackend};
use tempfile::TempDir;

/// Build an `EnvSource` closure from a map of overrides.
fn env_map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

fn source(map: HashMap<String, String>) -> impl Fn(&str) -> Option<String> {
    move |k: &str| map.get(k).cloned()
}

#[test]
fn from_env_builds_fs_config_with_defaults() {
    // Empty environment → all documented defaults.
    let cfg = EscurelConfig::from_source(&source(env_map(&[]))).unwrap();
    assert_eq!(cfg.storage_backend, StorageBackend::Fs);
    assert!(cfg.s3.is_none());
    assert_eq!(cfg.data_dir.to_str().unwrap(), "/data");
    assert_eq!(cfg.listen_http, "0.0.0.0:8080");
    assert_eq!(cfg.tenant, "default");
    assert_eq!(cfg.version, "0.0.0-dev");
    assert_eq!(cfg.embedding_provider, EmbeddingProvider::Zero);
    assert_eq!(cfg.embedding_dim, 768);
    assert!(cfg.auth.is_none());
}

#[test]
fn from_env_selects_s3_backend_when_configured() {
    // The exact var names the substrate Kamal deploy contract pins.
    let cfg = EscurelConfig::from_source(&source(env_map(&[
        ("ESCUREL_STORAGE_BACKEND", "s3"),
        ("ESCUREL_STORAGE_S3_BUCKET", "datazoo-substrate-app-nonprod"),
        ("ESCUREL_STORAGE_S3_ENDPOINT", "https://s3.example.com"),
        ("ESCUREL_STORAGE_S3_PREFIX", "dz/escurel/lanes/tenants/"),
        ("ESCUREL_STORAGE_S3_PATH_STYLE", "true"),
        ("ESCUREL_STORAGE_S3_ACCESS_KEY_ID", "ak"),
        ("ESCUREL_STORAGE_S3_SECRET_ACCESS_KEY", "sk"),
    ])))
    .unwrap();
    assert_eq!(cfg.storage_backend, StorageBackend::S3);
    let s3 = cfg.s3.expect("s3 config present");
    assert_eq!(s3.bucket, "datazoo-substrate-app-nonprod");
    assert_eq!(s3.endpoint, "https://s3.example.com");
    assert_eq!(s3.prefix, "dz/escurel/lanes/tenants/");
    assert_eq!(s3.access_key_id, "ak");
    assert_eq!(s3.secret_access_key, "sk");
}

#[test]
fn from_env_missing_s3_bucket_is_an_error() {
    let err = EscurelConfig::from_source(&source(env_map(&[("ESCUREL_STORAGE_BACKEND", "s3")])))
        .unwrap_err();
    assert!(
        err.to_string().contains("ESCUREL_STORAGE_S3_BUCKET"),
        "error should name the missing var: {err}"
    );
}

#[test]
fn from_env_unauthenticated_when_no_oidc_issuer() {
    // No issuer → dev mode, verifier disabled.
    let cfg = EscurelConfig::from_source(&source(env_map(&[]))).unwrap();
    assert!(cfg.auth.is_none(), "no issuer → unauthenticated");

    // Issuer present → auth config populated with jobspec claim names.
    let cfg = EscurelConfig::from_source(&source(env_map(&[
        (
            "ESCUREL_AUTH_OIDC_ISSUER",
            "https://auth.example.com/realms/main",
        ),
        ("ESCUREL_AUTH_OIDC_AUDIENCE", "escurel"),
        ("ESCUREL_AUTH_TENANT_CLAIM", "escurel_tenant"),
        ("ESCUREL_AUTH_ADMIN_ROLE_CLAIM", "roles"),
        ("ESCUREL_AUTH_ADMIN_ROLE_VALUE", "escurel:admin"),
        ("ESCUREL_AUTH_JWKS_REFRESH_SECS", "120"),
    ])))
    .unwrap();
    let auth = cfg.auth.expect("auth config present");
    assert_eq!(auth.issuer, "https://auth.example.com/realms/main");
    assert_eq!(auth.tenant_claim, "escurel_tenant");
    assert_eq!(auth.admin_role_value, "escurel:admin");
    assert_eq!(auth.jwks_refresh, Duration::from_secs(120));
}

#[test]
fn from_env_second_issuer_is_additive_and_optional() {
    // No `_2` → single issuer, additional_issuers empty (back-compat).
    let cfg = EscurelConfig::from_source(&source(env_map(&[(
        "ESCUREL_AUTH_OIDC_ISSUER",
        "https://triton.example/",
    )])))
    .unwrap();
    assert!(
        cfg.auth.unwrap().additional_issuers.is_empty(),
        "absent ISSUER_2 → single issuer"
    );

    // `_2` set → a second trust entry with its explicit JWKS URI. This
    // is the substrate's dz-escurel shape (Triton + Carl).
    let cfg = EscurelConfig::from_source(&source(env_map(&[
        ("ESCUREL_AUTH_OIDC_ISSUER", "https://triton.example/"),
        (
            "ESCUREL_AUTH_OIDC_ISSUER_2",
            "http://dz-carl.nonprod.int.data-zoo.de",
        ),
        (
            "ESCUREL_AUTH_JWKS_URI_2",
            "http://dz-carl.nonprod.int.data-zoo.de/jwks.json",
        ),
    ])))
    .unwrap();
    let auth = cfg.auth.expect("auth present");
    assert_eq!(
        auth.additional_issuers,
        vec![(
            "http://dz-carl.nonprod.int.data-zoo.de".to_owned(),
            Some("http://dz-carl.nonprod.int.data-zoo.de/jwks.json".to_owned()),
        )]
    );
}

#[test]
fn from_env_third_issuer_is_additive() {
    // The full dz-escurel substrate shape: Triton (primary) + Carl
    // (issuer 2, dashboard self-mint) + the escurel-explore BFF
    // (issuer 3, browser auth bridge). All share the audience + tenant
    // claim; only `iss` + JWKS differ. The verifier already supports N
    // additional issuers — this asserts the env layer wires the third.
    let cfg = EscurelConfig::from_source(&source(env_map(&[
        ("ESCUREL_AUTH_OIDC_ISSUER", "https://triton.example/"),
        (
            "ESCUREL_AUTH_OIDC_ISSUER_2",
            "http://dz-carl.nonprod.int.data-zoo.de",
        ),
        (
            "ESCUREL_AUTH_JWKS_URI_2",
            "http://dz-carl.nonprod.int.data-zoo.de/jwks.json",
        ),
        (
            "ESCUREL_AUTH_OIDC_ISSUER_3",
            "http://dz-escurel-explore.nonprod.int.data-zoo.de",
        ),
        (
            "ESCUREL_AUTH_JWKS_URI_3",
            "http://dz-escurel-explore.nonprod.int.data-zoo.de/jwks.json",
        ),
    ])))
    .unwrap();
    let auth = cfg.auth.expect("auth present");
    assert_eq!(
        auth.additional_issuers,
        vec![
            (
                "http://dz-carl.nonprod.int.data-zoo.de".to_owned(),
                Some("http://dz-carl.nonprod.int.data-zoo.de/jwks.json".to_owned()),
            ),
            (
                "http://dz-escurel-explore.nonprod.int.data-zoo.de".to_owned(),
                Some("http://dz-escurel-explore.nonprod.int.data-zoo.de/jwks.json".to_owned()),
            ),
        ]
    );
}

#[test]
fn from_env_third_issuer_without_second_is_skipped() {
    // ISSUER_3 is only consulted as the slot after ISSUER_2; a gap
    // (ISSUER_3 set but ISSUER_2 absent) must not silently promote it.
    // We treat the additional issuers as a contiguous _2.._N sequence.
    let cfg = EscurelConfig::from_source(&source(env_map(&[
        ("ESCUREL_AUTH_OIDC_ISSUER", "https://triton.example/"),
        (
            "ESCUREL_AUTH_OIDC_ISSUER_3",
            "http://dz-escurel-explore.nonprod.int.data-zoo.de",
        ),
    ])))
    .unwrap();
    let auth = cfg.auth.expect("auth present");
    assert!(
        auth.additional_issuers.is_empty(),
        "ISSUER_3 without ISSUER_2 is a misconfiguration → not trusted; got {:?}",
        auth.additional_issuers
    );
}

/// Boot the real binary, dial `/healthz`, then `/version`, then stop.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn binary_boots_and_serves_healthz() {
    let data_dir = TempDir::new().unwrap();

    let mut child = cargo_bin("escurel-server")
        .env("ESCUREL_SERVER_DATA_DIR", data_dir.path())
        // Random loopback port; the binary prints the resolved addr.
        .env("ESCUREL_SERVER_LISTEN_HTTP", "127.0.0.1:0")
        // Random loopback metrics port so parallel test binaries
        // don't fight over the default :9090.
        .env("ESCUREL_OBSERVABILITY_METRICS_LISTEN", "127.0.0.1:0")
        .env("VERSION", "9.9.9-bin")
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn escurel-server");

    // Read the bound HTTP address off stdout.
    let addr = read_listen_addr(&mut child);
    let base = format!("http://{addr}");

    let client = reqwest::Client::new();
    let health = client
        .get(format!("{base}/healthz"))
        .send()
        .await
        .expect("GET /healthz");
    assert_eq!(health.status(), 200);
    assert_eq!(health.text().await.unwrap(), "OK");

    let version = client
        .get(format!("{base}/version"))
        .send()
        .await
        .expect("GET /version");
    assert_eq!(version.text().await.unwrap(), "9.9.9-bin");

    terminate(child);
}

/// Degraded embedder start: select a provider whose feature this
/// build lacks; the server must still serve `/healthz` 200 and report
/// `embedder: false` on `/readyz`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn degraded_embedder_start_boots_with_readyz_false() {
    let data_dir = TempDir::new().unwrap();
    // gemini provider with no `gemini` feature compiled into the test
    // build → load fails → degraded ZeroEmbedder, embedder=false.
    let cfg = EscurelConfig::from_source(&source(env_map(&[
        ("ESCUREL_SERVER_DATA_DIR", data_dir.path().to_str().unwrap()),
        ("ESCUREL_SERVER_LISTEN_HTTP", "127.0.0.1:0"),
        ("ESCUREL_OBSERVABILITY_METRICS_LISTEN", "127.0.0.1:0"),
        ("ESCUREL_EMBEDDING_PROVIDER", "gemini"),
    ])))
    .unwrap();

    let booted = cfg.build().await.expect("server boots degraded");
    assert!(
        !booted.embedder.is_loaded(),
        "embedder should be degraded (not loaded)"
    );
    let base = format!("http://{}", booted.handle.local_addr);

    let client = reqwest::Client::new();
    // Liveness is dependency-free → always 200.
    let health = client.get(format!("{base}/healthz")).send().await.unwrap();
    assert_eq!(health.status(), 200);

    // Readiness reflects the degraded embedder → 503 + embedder=false.
    let ready = client.get(format!("{base}/readyz")).send().await.unwrap();
    assert_eq!(ready.status(), 503);
    let body: serde_json::Value = ready.json().await.unwrap();
    assert_eq!(body["ready"], false);
    assert_eq!(body["components"]["embedder"], false);
    assert_eq!(
        body["components"]["lane_store"], true,
        "FsStore should be reachable"
    );

    booted.handle.shutdown().await;
}

/// codex pre-v1 review (P2): a configured tenant with a path
/// separator must be rejected before it is joined into a
/// filesystem path, not silently used to escape the tenant root.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn build_rejects_path_traversal_tenant() {
    let data_dir = TempDir::new().unwrap();
    let cfg = EscurelConfig::from_source(&source(env_map(&[
        ("ESCUREL_SERVER_DATA_DIR", data_dir.path().to_str().unwrap()),
        ("ESCUREL_SERVER_LISTEN_HTTP", "127.0.0.1:0"),
        ("ESCUREL_OBSERVABILITY_METRICS_LISTEN", "127.0.0.1:0"),
        ("ESCUREL_TENANT", "../escape"),
    ])))
    .unwrap();
    let err = match cfg.build().await {
        Ok(_) => panic!("must reject bad tenant id"),
        Err(e) => e,
    };
    assert!(
        err.to_string().contains("ESCUREL_TENANT"),
        "error must name ESCUREL_TENANT; got: {err}"
    );
}

/// codex pre-v1 review (P1): on a fresh DuckDB whose LaneStore
/// already holds canonical markdown (cattle-node-loss / wiped
/// volume), the boot path must rebuild the index from that
/// markdown rather than serving an empty corpus.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fresh_duckdb_rebuilds_index_from_surviving_markdown() {
    let data_dir = TempDir::new().unwrap();
    // Seed canonical markdown into the FsStore lane the way a
    // surviving host volume would have it: {root}/tenants/{tenant}/markdown/...
    let md = data_dir
        .path()
        .join("tenants")
        .join("default")
        .join("markdown")
        .join("skills");
    std::fs::create_dir_all(&md).unwrap();
    std::fs::write(
        md.join("customer.md"),
        "---\ntype: skill\nid: customer\ndescription: a buyer\n---\n# customer\n",
    )
    .unwrap();

    // No DuckDB file exists yet → `fresh` boot → must rebuild.
    let cfg = EscurelConfig::from_source(&source(env_map(&[
        ("ESCUREL_SERVER_DATA_DIR", data_dir.path().to_str().unwrap()),
        ("ESCUREL_SERVER_LISTEN_HTTP", "127.0.0.1:0"),
        ("ESCUREL_OBSERVABILITY_METRICS_LISTEN", "127.0.0.1:0"),
    ])))
    .unwrap();
    let booted = cfg.build().await.expect("server boots");
    let base = format!("http://{}", booted.handle.local_addr);

    // Unauthenticated (no OIDC issuer configured) → /mcp is open.
    // list_skills must show the rebuilt-from-markdown skill.
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/mcp"))
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": "list_skills", "arguments": {} }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let skills = body["result"]["structuredContent"]["skills"]
        .as_array()
        .expect("skills array");
    assert!(
        skills.iter().any(|s| s["id"] == "customer"),
        "fresh boot must rebuild the seeded skill from markdown; got: {body}"
    );

    booted.handle.shutdown().await;
}

// --- helpers ---------------------------------------------------

/// `assert_cmd`'s `cargo_bin`, wrapped so the binary-spawn test does
/// not depend on the workspace target layout.
fn cargo_bin(name: &str) -> Command {
    use assert_cmd::cargo::CommandCargoExt as _;
    Command::cargo_bin(name).expect("locate escurel-server binary")
}

/// Read the `escurel-server listening http=<addr>` line off the
/// child's stdout and return the parsed address. Panics if the child
/// exits before printing it (its stderr is inherited for diagnosis).
fn read_listen_addr(child: &mut std::process::Child) -> String {
    let stdout = child.stdout.take().expect("child stdout piped");
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).expect("read child stdout");
        assert!(
            n != 0,
            "escurel-server exited before printing its listen address"
        );
        if let Some(rest) = line.trim().strip_prefix("escurel-server listening http=") {
            return rest.to_owned();
        }
    }
}

/// Send SIGTERM and reap, asserting a clean (0) exit within a grace
/// window. Falls back to `kill` if the graceful path stalls.
fn terminate(mut child: std::process::Child) {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        // SIGTERM via libc-free path: `nix`-free, use kill(2) through
        // std by re-spawning `kill`? Simpler: send SIGTERM with the
        // raw syscall via `libc` is unavailable, so use the `kill`
        // command which every CI image ships.
        let pid = child.id();
        let _ = Command::new("kill")
            .arg("-TERM")
            .arg(pid.to_string())
            .status();
        // Give graceful shutdown a moment.
        for _ in 0..50 {
            match child.try_wait().expect("try_wait") {
                Some(status) => {
                    assert!(
                        status.success() || status.signal() == Some(15),
                        "escurel-server exited uncleanly: {status:?}"
                    );
                    return;
                }
                None => std::thread::sleep(Duration::from_millis(100)),
            }
        }
        let _ = child.kill();
        let _ = child.wait();
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// Regression (DataZooDE/escurel chat-history loss): chat appends must survive
/// a server RESTART. The CRDT backend used to be a SECOND `Connection::open`
/// on the same DuckDB file (an independent instance); its checkpoints clobbered
/// the indexer's committed `chat_messages` writes, so appends were visible
/// in-process but LOST after a restart (deployed nonprod symptom: chat_messages
/// empty for the agent AND carl). Fix: the CRDT connection is a `try_clone` of
/// the indexer's connection (same instance). This boots the real config path,
/// appends, shuts down, reboots on the SAME data dir, and asserts persistence.
#[tokio::test]
async fn chat_history_survives_server_restart() {
    let data_dir = TempDir::new().unwrap();
    let env = env_map(&[
        ("ESCUREL_SERVER_DATA_DIR", data_dir.path().to_str().unwrap()),
        ("ESCUREL_SERVER_LISTEN_HTTP", "127.0.0.1:0"),
        ("ESCUREL_OBSERVABILITY_METRICS_LISTEN", "127.0.0.1:0"),
        ("ESCUREL_TENANT", "default"),
    ]);
    let chat = "agent:dev-user";
    let client = reqwest::Client::new();
    let call = |base: String, name: &'static str, args: serde_json::Value| {
        let client = client.clone();
        async move {
            client
                .post(format!("{base}/mcp"))
                .json(&serde_json::json!({
                    "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                    "params": { "name": name, "arguments": args }
                }))
                .send()
                .await
                .unwrap()
                .json::<serde_json::Value>()
                .await
                .unwrap()
        }
    };

    // Boot #1: append two messages, confirm same-process round-trip.
    let cfg = EscurelConfig::from_source(&source(env.clone())).unwrap();
    let booted = cfg.build().await.expect("boot 1");
    let base = format!("http://{}", booted.handle.local_addr);
    for (role, body) in [("user", "remember-me"), ("assistant", "ok")] {
        let r = call(base.clone(), "append_message",
            serde_json::json!({ "chat_group_id": chat, "role": role, "content": body, "embed": false })).await;
        assert!(r.get("error").is_none(), "append: {r}");
    }
    let r = call(
        base.clone(),
        "list_messages",
        serde_json::json!({ "chat_group_id": chat }),
    )
    .await;
    let n1 = r["result"]["structuredContent"]["messages"]
        .as_array()
        .map(|a| a.len())
        .unwrap_or(0);
    assert_eq!(n1, 2, "same-process round-trip: {r}");
    booted.handle.shutdown().await;

    // Boot #2 on the SAME data dir: the messages must still be there.
    let cfg2 = EscurelConfig::from_source(&source(env)).unwrap();
    let booted2 = cfg2.build().await.expect("boot 2");
    let base2 = format!("http://{}", booted2.handle.local_addr);
    let r2 = call(
        base2,
        "list_messages",
        serde_json::json!({ "chat_group_id": chat }),
    )
    .await;
    let n2 = r2["result"]["structuredContent"]["messages"]
        .as_array()
        .map(|a| a.len())
        .unwrap_or(0);
    booted2.handle.shutdown().await;
    assert_eq!(
        n2, 2,
        "chat history must SURVIVE a restart (was 0 before the try_clone fix): {r2}"
    );
}
