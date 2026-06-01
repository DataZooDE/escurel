//! End-to-end tests for the `escurel admin` command group.
//!
//! Real gateway via `escurel-test-support` with a real tempdir-backed
//! `FsTenantStore` + quota wired, driven through the compiled binary.
//! Covers the unary admin commands and the streaming ones (rebuild,
//! tenant export/import).

use std::path::PathBuf;
use std::sync::Arc;

use assert_cmd::Command;
use escurel_admin::{FsTenantStore, TenantSpec, TenantStore};
use escurel_quota::{QuotaConfig, QuotaManager};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::Value;
use tempfile::TempDir;

const TENANT: &str = "acme";
const CUSTOMER_SKILL: &str = "---\ntype: skill\nid: customer\ndescription: x\n---\n# customer\n";

struct Harness {
    process: EscurelProcess,
    http_addr: String,
    admin_token: String,
    agent_token: String,
    tenants_root: PathBuf,
    _tenants_dir: TempDir,
}

async fn start() -> Harness {
    let tenants_dir = TempDir::new().unwrap();
    let tenants_root = tenants_dir.path().to_path_buf();
    let tenant_store: Arc<dyn TenantStore> = Arc::new(FsTenantStore::new(tenants_root.clone()));
    // Provision the JWT tenant in the FsTenantStore so the export RPC
    // (which walks `<root>/<tenant>/markdown`) has a directory to tar,
    // and mirror a markdown page into it so the tarball is non-trivial.
    tenant_store
        .create(&TenantSpec {
            tenant_id: TENANT.to_owned(),
            display_name: "Acme".to_owned(),
        })
        .await
        .unwrap();
    let md = tenants_root.join(TENANT).join("markdown").join("skills");
    std::fs::create_dir_all(&md).unwrap();
    std::fs::write(md.join("customer.md"), CUSTOMER_SKILL).unwrap();
    let quota = Arc::new(QuotaManager::new(QuotaConfig {
        queries_per_minute: 100,
        writes_per_minute: 50,
        embeds_per_minute: 25,
        concurrent_sessions: 4,
    }));
    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("customer", CUSTOMER_SKILL)
                .done(),
        ),
        config_overrides: ConfigOverrides {
            gateway_version: Some("1.0.0-test".to_owned()),
            tenant_store: Some(tenant_store),
            quota: Some(quota),
            ..Default::default()
        },
    })
    .await;
    let http_addr = process
        .base_url()
        .strip_prefix("http://")
        .unwrap()
        .to_owned();
    Harness {
        admin_token: process.mint_token(TENANT, Role::Admin),
        agent_token: process.mint_token(TENANT, Role::Agent),
        process,
        http_addr,
        tenants_root,
        _tenants_dir: tenants_dir,
    }
}

fn v(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| s.to_string()).collect()
}

async fn admin(h: &Harness, args: Vec<String>) -> std::process::Output {
    let addr = h.http_addr.clone();
    let token = h.admin_token.clone();
    tokio::task::spawn_blocking(move || {
        Command::cargo_bin("escurel")
            .unwrap()
            .env("ESCUREL_SERVER", format!("http://{addr}"))
            .env("ESCUREL_TOKEN", token)
            .args(&args)
            .assert()
            .success()
            .get_output()
            .clone()
    })
    .await
    .unwrap()
}

fn json(out: &std::process::Output) -> Value {
    serde_json::from_slice(&out.stdout).expect("stdout is JSON")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_health_reports_version() {
    let h = start().await;
    let out = admin(&h, v(&["admin", "health"])).await;
    assert_eq!(json(&out)["version"], "1.0.0-test");
    h.process.shutdown().await;
}

/// Tenant lifecycle through the CLI: create → get → list → update →
/// delete, with on-disk provisioning verified.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_tenant_crud_lifecycle() {
    let h = start().await;
    let created = admin(
        &h,
        v(&[
            "admin",
            "tenant",
            "create",
            "--id",
            "globex",
            "--name",
            "Globex Corp",
        ]),
    )
    .await;
    assert_eq!(json(&created)["tenant"]["tenant_id"], "globex");
    assert!(h.tenants_root.join("globex").join("tenant.json").is_file());

    let got = admin(&h, v(&["admin", "tenant", "get", "--id", "globex"])).await;
    assert_eq!(json(&got)["tenant"]["display_name"], "Globex Corp");

    let listed = admin(&h, v(&["admin", "tenant", "list"])).await;
    assert!(
        json(&listed)["tenants"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["tenant_id"] == "globex")
    );

    let updated = admin(
        &h,
        v(&[
            "admin", "tenant", "update", "--id", "globex", "--name", "Renamed",
        ]),
    )
    .await;
    assert_eq!(json(&updated)["tenant"]["display_name"], "Renamed");

    let deleted = admin(&h, v(&["admin", "tenant", "delete", "--id", "globex"])).await;
    assert_eq!(json(&deleted)["deleted"], true);
    assert!(!h.tenants_root.join("globex").exists());
    h.process.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_audit_and_quota() {
    let h = start().await;
    let audit = admin(&h, v(&["admin", "audit", "--tenant", TENANT])).await;
    let a = json(&audit);
    assert!(a["markdown_not_in_duckdb"].as_array().unwrap().is_empty());

    let quota = admin(&h, v(&["admin", "quota", "--tenant", TENANT])).await;
    assert_eq!(json(&quota)["queries_remaining"], 100);
    h.process.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_rebuild_reports_terminal_progress() {
    let h = start().await;
    // The MCP transport is one-shot: rebuild returns the terminal
    // `{done, total}` rather than a stream of progress chunks.
    let out = admin(&h, v(&["admin", "rebuild", "--tenant", TENANT])).await;
    let prog = json(&out);
    assert_eq!(prog["done"], prog["total"], "rebuild done == total: {prog}");
    h.process.shutdown().await;
}

/// Export a tenant to a file, then import it back into a fresh tenant.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_tenant_export_then_import() {
    let h = start().await;
    let tar = h.tenants_root.join("acme.tgz");
    let tar_str = tar.to_str().unwrap().to_owned();

    let exp = admin(
        &h,
        v(&[
            "admin", "tenant", "export", "--id", TENANT, "--out", &tar_str,
        ]),
    )
    .await;
    assert!(json(&exp)["bytes_exported"].as_u64().unwrap() > 0);
    assert!(tar.is_file());

    // Fresh destination tenant.
    admin(
        &h,
        v(&[
            "admin", "tenant", "create", "--id", "globex", "--name", "Globex",
        ]),
    )
    .await;

    let imp = admin(
        &h,
        v(&[
            "admin", "tenant", "import", "--id", "globex", "--in", &tar_str,
        ]),
    )
    .await;
    assert!(json(&imp)["bytes_imported"].as_u64().unwrap() > 0);
    h.process.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_rejects_agent_token() {
    let h = start().await;
    let addr = h.http_addr.clone();
    let token = h.agent_token.clone();
    let out = tokio::task::spawn_blocking(move || {
        Command::cargo_bin("escurel")
            .unwrap()
            .env("ESCUREL_SERVER", format!("http://{addr}"))
            .env("ESCUREL_TOKEN", token)
            .args(["admin", "tenant", "list"])
            .assert()
            .failure()
            .get_output()
            .clone()
    })
    .await
    .unwrap();
    let err: Value = serde_json::from_slice(&out.stderr).expect("stderr is JSON");
    // The admin tools live on the same `/mcp` endpoint; an agent-role
    // token reaches the dispatcher and gets the JSON-RPC `-32001`
    // ("admin role required for this tool") error, surfaced verbatim.
    assert!(
        err["error"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("admin role"),
        "got: {err}"
    );
    h.process.shutdown().await;
}
