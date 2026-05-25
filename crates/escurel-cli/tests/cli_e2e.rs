//! End-to-end test for the `escurel` CLI.
//!
//! Spins up the real gateway via `escurel-test-support` and
//! exercises every CLI subcommand via the compiled binary
//! (`assert_cmd::cargo_bin`). No mocks at the CLI boundary; the
//! support crate's in-process JWKS issuer stands in for a real
//! OIDC realm.

use assert_cmd::Command;
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::Value;

const TENANT: &str = "acme";

const CUSTOMER_SKILL: &str = "---\n\
type: skill\n\
id: customer\n\
description: A buying organisation.\n\
required_frontmatter: [id, name]\n\
optional_frontmatter: [tier]\n\
---\n\
# customer\n";

const ACME_INSTANCE: &str = "---\n\
type: instance\n\
skill: customer\n\
id: acme\n\
name: Acme Corp\n\
tier: gold\n\
---\n\
# Acme Corp\n\nKey account. See [[customer::initech]].\n";

const INITECH_INSTANCE: &str = "---\n\
type: instance\n\
skill: customer\n\
id: initech\n\
name: Initech\n\
---\n\
# Initech\n";

struct Harness {
    process: EscurelProcess,
    grpc_addr: String,
    bearer: String,
}

async fn start() -> Harness {
    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("customer", CUSTOMER_SKILL)
                .instance("customer", "acme", ACME_INSTANCE)
                .instance("customer", "initech", INITECH_INSTANCE)
                .done(),
        ),
        config_overrides: ConfigOverrides {
            gateway_version: Some("1.0.0-test".to_owned()),
            ..Default::default()
        },
    })
    .await;
    let grpc_addr = process
        .grpc_endpoint()
        .expect("grpc endpoint")
        .strip_prefix("http://")
        .unwrap()
        .to_owned();
    let bearer = process.mint_token(TENANT, Role::Agent);
    Harness {
        process,
        grpc_addr,
        bearer,
    }
}

fn cli(h: &Harness) -> Command {
    let mut c = Command::cargo_bin("escurel").expect("escurel binary built");
    c.env("ESCUREL_SERVER", format!("http://{}", h.grpc_addr))
        .env("ESCUREL_TOKEN", &h.bearer);
    c
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_skills_emits_seeded_skill() {
    let h = start().await;
    let assert = tokio::task::spawn_blocking({
        let addr = h.grpc_addr.clone();
        let bearer = h.bearer.clone();
        move || {
            Command::cargo_bin("escurel")
                .unwrap()
                .env("ESCUREL_SERVER", format!("http://{addr}"))
                .env("ESCUREL_TOKEN", bearer)
                .args(["list-skills"])
                .assert()
                .success()
        }
    })
    .await
    .unwrap();
    let out: Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    let skills = out["skills"].as_array().unwrap();
    assert!(skills.iter().any(|s| s["id"] == "customer"));
    h.process.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resolve_emits_existing_page() {
    let h = start().await;
    let assert = tokio::task::spawn_blocking({
        let mut c = cli(&h);
        c.args(["resolve", "[[customer::acme]]"]);
        move || c.assert().success()
    })
    .await
    .unwrap();
    let out: Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!(out["exists"], true);
    assert_eq!(out["page"]["skill"], "customer");
    assert_eq!(out["page"]["slug"], "acme");
    h.process.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expand_emits_body_and_wikilinks() {
    let h = start().await;
    // Use list-instances to find the page_id.
    let inst_out = tokio::task::spawn_blocking({
        let mut c = cli(&h);
        c.args(["list-instances", "--skill", "customer"]);
        move || c.assert().success()
    })
    .await
    .unwrap();
    let inst: Value = serde_json::from_slice(&inst_out.get_output().stdout).unwrap();
    let acme = inst["instances"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["page_id"].as_str().unwrap().contains("acme"))
        .unwrap()
        .clone();
    let page_id = acme["page_id"].as_str().unwrap().to_owned();

    let expand_out = tokio::task::spawn_blocking({
        let mut c = cli(&h);
        c.args(["expand", page_id.as_str()]);
        move || c.assert().success()
    })
    .await
    .unwrap();
    let out: Value = serde_json::from_slice(&expand_out.get_output().stdout).unwrap();
    assert!(out["body"].as_str().unwrap().contains("Acme Corp"));
    assert!(
        out["wikilinks_out"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["id"] == "initech")
    );
    h.process.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn search_emits_hits() {
    let h = start().await;
    let assert = tokio::task::spawn_blocking({
        let mut c = cli(&h);
        c.args(["search", "Acme", "--k", "5"]);
        move || c.assert().success()
    })
    .await
    .unwrap();
    let out: Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    let hits = out["hits"].as_array().unwrap();
    assert!(!hits.is_empty());
    assert_eq!(out["granularity"], "block");
    h.process.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn update_page_via_stdin_round_trips() {
    let h = start().await;
    let body = "---\n\
                type: instance\n\
                skill: customer\n\
                id: globex\n\
                name: Globex\n\
                ---\n\
                # Globex\n";
    let assert = tokio::task::spawn_blocking({
        let mut c = cli(&h);
        c.args(["update-page", "markdown/instances/customer/globex.md"]);
        c.write_stdin(body);
        move || c.assert().success()
    })
    .await
    .unwrap();
    let out: Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!(out["ok"], true);
    h.process.shutdown().await;
}

/// When the server is configured without an OidcVerifier (dev /
/// on-host mode), the CLI must still work without a token —
/// don't gate on `ESCUREL_TOKEN` before even attempting the call.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unauthenticated_mode_works_without_token() {
    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::Disabled,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("customer", CUSTOMER_SKILL)
                .instance("customer", "acme", ACME_INSTANCE)
                .instance("customer", "initech", INITECH_INSTANCE)
                .done(),
        ),
        config_overrides: ConfigOverrides {
            gateway_version: Some("1.0.0-test".to_owned()),
            ..Default::default()
        },
    })
    .await;
    let grpc_addr = process
        .grpc_endpoint()
        .unwrap()
        .strip_prefix("http://")
        .unwrap()
        .to_owned();

    let assert = tokio::task::spawn_blocking(move || {
        Command::cargo_bin("escurel")
            .unwrap()
            .env("ESCUREL_SERVER", format!("http://{grpc_addr}"))
            .env_remove("ESCUREL_TOKEN")
            .args(["list-skills"])
            .assert()
            .success()
    })
    .await
    .unwrap();
    let out: Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert!(
        out["skills"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["id"] == "customer")
    );
    process.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_token_against_authed_server_returns_unauthenticated() {
    let h = start().await;
    let output = tokio::task::spawn_blocking({
        let addr = h.grpc_addr.clone();
        move || {
            Command::cargo_bin("escurel")
                .unwrap()
                .env("ESCUREL_SERVER", format!("http://{addr}"))
                .env_remove("ESCUREL_TOKEN")
                .args(["list-skills"])
                .assert()
                .failure()
        }
    })
    .await
    .unwrap();
    let stderr = String::from_utf8_lossy(&output.get_output().stderr).to_string();
    assert!(
        stderr.to_lowercase().contains("unauthenticated")
            || stderr.to_lowercase().contains("missing")
            || stderr.contains("ESCUREL_TOKEN"),
        "expected an auth-related error in stderr, got: {stderr}"
    );
    h.process.shutdown().await;
}
