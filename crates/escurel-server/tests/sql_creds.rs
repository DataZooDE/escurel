//! End-to-end tests for the SQL-view credential registry MCP tools
//! (`register_credential` / `list_credentials` / `delete_credential`).
//!
//! Real gateway, real DuckDB, real OIDC (TestIssuer JWKS), real reqwest
//! over `POST /mcp`. The load-bearing properties (REQ-SQL-05 / D10): only
//! an admin may touch the registry, and no response (register or list)
//! ever echoes the secret. The secret-free *corpus* property is proven at
//! the storage layer by `escurel-index/tests/credentials.rs`.

use std::path::PathBuf;
use std::sync::Arc;

use escurel_admin::{FsTenantStore, TenantStore};
use escurel_quota::{QuotaConfig, QuotaManager};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};
use tempfile::TempDir;

const TENANT: &str = "acme";
const CUSTOMER_SKILL: &str = "---\ntype: skill\nid: customer\ndescription: x\n---\n# customer\n";
const DSN: &str = "postgresql://svc:hunter2@crm.internal:5432/crm";

struct Harness {
    process: EscurelProcess,
    #[allow(dead_code)]
    tenants_root: PathBuf,
    _tenants_dir: TempDir,
}

async fn start() -> Harness {
    let tenants_dir = TempDir::new().unwrap();
    let tenants_root = tenants_dir.path().to_path_buf();
    let tenant_store: Arc<dyn TenantStore> = Arc::new(FsTenantStore::new(tenants_root.clone()));
    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("customer", CUSTOMER_SKILL)
                .done(),
        ),
        config_overrides: ConfigOverrides {
            tenant_store: Some(tenant_store),
            quota: Some(Arc::new(QuotaManager::new(QuotaConfig::defaults()))),
            ..Default::default()
        },
    })
    .await;
    Harness {
        process,
        tenants_root,
        _tenants_dir: tenants_dir,
    }
}

async fn call(p: &EscurelProcess, role: Role, name: &str, args: Value) -> Value {
    let token = p.mint_token(TENANT, role);
    reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": name, "arguments": args },
        }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json")
}

#[tokio::test]
async fn admin_registers_credential_and_responses_never_echo_secret() {
    let h = start().await;
    let p = &h.process;

    let reg = call(
        p,
        Role::Admin,
        "register_credential",
        json!({ "name": "crm_pg", "connector": "postgres", "secret": DSN }),
    )
    .await;
    assert!(reg.get("error").is_none(), "register error: {reg}");
    assert_eq!(reg["result"]["structuredContent"]["name"], "crm_pg");
    // The secret must never be echoed back, even to the admin who set it.
    assert!(
        !serde_json::to_string(&reg).unwrap().contains(DSN),
        "register response leaked the secret"
    );

    // list_credentials reports the name + connector but NEVER the secret.
    let listed = call(p, Role::Admin, "list_credentials", json!({})).await;
    assert!(listed.get("error").is_none(), "list error: {listed}");
    let creds = listed["result"]["structuredContent"]["credentials"]
        .as_array()
        .expect("credentials array");
    assert_eq!(creds.len(), 1);
    assert_eq!(creds[0]["name"], "crm_pg");
    assert_eq!(creds[0]["connector"], "postgres");
    assert!(creds[0].get("secret").is_none(), "list leaked secret key");
    assert!(
        !serde_json::to_string(&listed).unwrap().contains(DSN),
        "list response leaked the secret value"
    );
    // (The secret-free *corpus* property — no DSN in the markdown store /
    // tenant_export — is proven directly at the storage layer by
    // escurel-index/tests/credentials.rs.)

    h.process.shutdown().await;
}

#[tokio::test]
async fn agent_cannot_touch_the_credential_registry() {
    let h = start().await;
    let p = &h.process;

    for (name, args) in [
        (
            "register_credential",
            json!({ "name": "x", "connector": "postgres", "secret": DSN }),
        ),
        ("list_credentials", json!({})),
        ("delete_credential", json!({ "name": "x" })),
    ] {
        let body = call(p, Role::Agent, name, args).await;
        assert!(
            body.get("error").is_some(),
            "agent must be rejected for {name}, got {body}"
        );
    }

    // And nothing the agent attempted leaked into the registry.
    let listed = call(p, Role::Admin, "list_credentials", json!({})).await;
    let creds = listed["result"]["structuredContent"]["credentials"]
        .as_array()
        .unwrap();
    assert!(
        creds.is_empty(),
        "agent should not have registered anything"
    );

    h.process.shutdown().await;
}
