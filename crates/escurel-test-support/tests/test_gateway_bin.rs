//! `escurel-test-gateway`: a real gateway that VERIFIES tokens, for harnesses that are not Rust.
//!
//! The VS Code extension's integration suite is TypeScript. Its runner can only mint the
//! per-run tokens that make the gateway stamp a run onto an agent's draft against a gateway
//! with a verifier, and until now the suite's gateway had none — so a changeset never joined
//! its run in the lineage and a promotion never cascaded. This binary hands any process the
//! same issuer the Rust suites use, so the claims cannot drift from what the gateway expects.
//!
//! Real binary, real gateway, real HTTP. No mocks.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use serde_json::{Value, json};

struct Running {
    child: Child,
    info: Value,
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn seed_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/seed")
}

fn start(extra: &[&str]) -> Running {
    let mut child = Command::new(env!("CARGO_BIN_EXE_escurel-test-gateway"))
        .args(["--tenant", "vsx", "--seed"])
        .arg(seed_dir())
        .args(extra)
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn escurel-test-gateway");
    let mut line = String::new();
    BufReader::new(child.stdout.take().expect("stdout"))
        .read_line(&mut line)
        .expect("the first line is the connection info");
    let info: Value = serde_json::from_str(line.trim())
        .unwrap_or_else(|e| panic!("first line must be JSON ({e}): {line:?}"));
    Running { child, info }
}

async fn call(r: &Running, bearer: Option<&str>, tool: &str, args: Value) -> reqwest::Response {
    let mut req = reqwest::Client::new()
        .post(format!(
            "{}/mcp",
            r.info["gateway_url"].as_str().expect("gateway_url")
        ))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": { "name": tool, "arguments": args } }));
    if let Some(b) = bearer {
        req = req.header("authorization", format!("Bearer {b}"));
    }
    req.send().await.expect("post")
}

/// The point of the binary: this gateway checks tokens. A gateway without a verifier answers
/// everyone, which is exactly why a static-bearer runner could never stamp a run.
#[tokio::test]
async fn the_gateway_it_starts_verifies_tokens() {
    let g = start(&[]);
    let bearer = g.info["bearer"].as_str().expect("a ready-made bearer");

    let anonymous = call(&g, None, "list_skills", json!({})).await;
    assert_eq!(anonymous.status(), 401, "no bearer must be refused");

    let forged = call(&g, Some("not.a.jwt"), "list_skills", json!({})).await;
    assert_eq!(
        forged.status(),
        401,
        "a bearer the issuer did not sign must be refused"
    );

    let ok = call(&g, Some(bearer), "list_skills", json!({})).await;
    assert_eq!(ok.status(), 200, "the issued bearer must be accepted");
}

/// What a minted-mode runner needs, and nothing it cannot use.
#[tokio::test]
async fn it_prints_what_a_minted_runner_needs() {
    let g = start(&[]);
    for key in [
        "gateway_url",
        "issuer_url",
        "kid",
        "signing_key",
        "bearer",
        "tenant",
    ] {
        assert!(
            g.info[key].as_str().is_some_and(|v| !v.is_empty()),
            "`{key}` must be a non-empty string: {}",
            g.info
        );
    }
    assert_eq!(g.info["tenant"], json!("vsx"));
    assert!(
        g.info["signing_key"]
            .as_str()
            .unwrap()
            .contains("PRIVATE KEY"),
        "the runner signs per-run tokens with this"
    );
}

/// The seed directory is replayed through the real write path, flat page ids and all: the
/// extension's integration suites address instances as `markdown/instances/<skill>__<id>.md`.
#[tokio::test]
async fn it_seeds_skills_and_flat_instances_from_a_directory() {
    let g = start(&[]);
    let bearer = g.info["bearer"].as_str().unwrap().to_owned();
    let resp = call(
        &g,
        Some(&bearer),
        "expand",
        json!({ "page_id": "markdown/instances/note__plan.md", "raw": true }),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    let content = body["result"]["structuredContent"]["content"]
        .as_str()
        .unwrap_or_default();
    assert!(
        content.contains("BASELINE."),
        "the seeded page must be readable: {body}"
    );
}

/// SIGTERM ends it cleanly. A harness that kills it must not leave a gateway, its port or its
/// data directory behind for the next run to trip over.
#[tokio::test]
async fn sigterm_shuts_it_down_cleanly() {
    let mut g = start(&[]);
    let pid = g.child.id().to_string();
    let status = Command::new("kill")
        .args(["-TERM", &pid])
        .status()
        .expect("send SIGTERM");
    assert!(status.success());
    let exit = g.child.wait().expect("wait");
    assert!(
        exit.success(),
        "a SIGTERM must be a clean exit, got {exit:?}"
    );
}

/// A bad invocation says what is wrong and exits non-zero, rather than starting a gateway
/// nobody asked for.
#[test]
fn it_refuses_a_missing_or_empty_seed() {
    let none = Command::new(env!("CARGO_BIN_EXE_escurel-test-gateway"))
        .args(["--tenant", "vsx"])
        .output()
        .expect("run");
    assert!(!none.status.success());
    assert!(String::from_utf8_lossy(&none.stderr).contains("--seed"));

    let empty = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_escurel-test-gateway"))
        .args(["--tenant", "vsx", "--seed"])
        .arg(empty.path())
        .output()
        .expect("run");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("no skills/ or instances/"));
}

/// Requeue, pause and resume are admin-only run controls, so a harness that proves them needs an
/// admin's token as well as a human's. The two must differ in exactly the way the gateway cares
/// about: the admin can call an admin-scope tool, the other cannot.
#[tokio::test]
async fn it_prints_an_admin_bearer_beside_the_ordinary_one() {
    let g = start(&[]);
    let admin = g.info["admin_bearer"].as_str().expect("an admin_bearer");
    let human = g.info["bearer"].as_str().expect("a bearer");
    assert_ne!(admin, human, "two different credentials");

    let refused: Value = call(&g, Some(human), "admin_quota", json!({}))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(
        refused["error"]["data"]["code"],
        json!("admin_required"),
        "the ordinary bearer must not be an admin: {refused}"
    );

    let allowed: Value = call(&g, Some(admin), "admin_quota", json!({}))
        .await
        .json()
        .await
        .unwrap();
    // Past the admin gate is all that is being asked. This gateway wires no quota manager, so
    // `admin_quota` itself answers with an unrelated error; what must NOT come back is the
    // admin refusal.
    assert_ne!(
        allowed["error"]["data"]["code"],
        json!("admin_required"),
        "the admin bearer must pass the admin gate: {allowed}"
    );
}

/// A demo outlasts a ten-minute token. `--bearer-file` keeps a file holding a CURRENT bearer for
/// whoever is signed in with it, rewritten in place by rename so a reader never sees half of it.
#[tokio::test]
async fn a_bearer_file_is_written_at_once_and_kept_fresh() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("bearer.json");
    let g = start(&[
        "--bearer-file",
        file.to_str().unwrap(),
        "--bearer-refresh-secs",
        "1",
    ]);

    let read =
        || -> Value { serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap() };
    let first = read();
    assert_eq!(
        first["bearer"], g.info["bearer"],
        "written before the line is printed"
    );
    assert_eq!(first["admin_bearer"], g.info["admin_bearer"]);

    let mut rotated = false;
    for _ in 0..40 {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        // Every read parses: the file is replaced, never truncated in place.
        let now = read();
        if now["bearer"] != first["bearer"] {
            rotated = true;
            break;
        }
    }
    assert!(
        rotated,
        "the bearer in the file must be replaced by a fresh one"
    );

    // The fresh one is accepted by the gateway.
    let now = read();
    let ok = call(&g, now["bearer"].as_str(), "list_skills", json!({})).await;
    assert_eq!(ok.status(), 200);
}

/// `mint_agent_token` is how the extension starts a skill in a terminal under a governed run. A
/// gateway with no signing identity answers it `unsupported`, so a harness could not exercise the
/// feature at all (found when the extension's terminal-start integration test got exactly that).
#[tokio::test]
async fn the_gateway_can_mint_an_agent_token() {
    let g = start(&[]);
    let bearer = g.info["bearer"].as_str().unwrap().to_owned();
    let resp = call(
        &g,
        Some(&bearer),
        "mint_agent_token",
        json!({ "skill": "note", "target_page_id": "markdown/instances/note__plan.md" }),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert!(
        body.get("error").is_none(),
        "a signing identity must be wired: {body}"
    );
    let minted = &body["result"]["structuredContent"];
    assert!(
        minted["token"].as_str().is_some_and(|t| !t.is_empty()),
        "a token: {body}"
    );
    assert!(
        minted["run_id"].as_str().is_some_and(|r| !r.is_empty()),
        "a run id: {body}"
    );
}
