//! End-to-end tests for the `escurel-client` **admin** wrapper
//! ([`AdminClient`]) over HTTP (MCP).
//!
//! Real gateway via `escurel-test-support`, real HTTP transport, real
//! `OidcVerifier`, real tempdir-backed `FsTenantStore`, real `Indexer`
//! with a real DuckDB file. No mocks at the boundary the test
//! exercises (CLAUDE principle 2). Each test drives the production
//! admin code path verbatim through the typed client an operator
//! (CLI / dashboard) would use.

use std::path::PathBuf;
use std::sync::Arc;

use escurel_admin::{FsTenantStore, TenantStore};
use escurel_client::Error as ClientError;
use escurel_client::{
    AdminClient, AppendMessageRequest, AuditRequest, Client, DeleteChatHistoryRequest,
    HealthRequest, JSONRPC_ADMIN_REQUIRED, ListMessagesRequest, QuotaGetRequest, SecretString,
    TenantCreateRequest, TenantDeleteRequest, TenantGetRequest, TenantListRequest, TenantSpec,
    TenantUpdateRequest,
};
use escurel_quota::{QuotaConfig, QuotaManager};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use tempfile::TempDir;

const TENANT: &str = "acme";

const CUSTOMER_SKILL: &str = "---\ntype: skill\nid: customer\ndescription: x\n---\n# customer\n";

/// The per-minute budget the quota-wired gateway starts with. A fresh
/// tenant that has made no agent calls reports exactly these as
/// remaining.
const QUOTA_QUERIES: u32 = 100;
const QUOTA_WRITES: u32 = 50;
const QUOTA_EMBEDS: u32 = 25;

struct Harness {
    process: EscurelProcess,
    tenants_root: PathBuf,
    _tenants_dir: TempDir,
}

/// Spawn a gateway with a real tempdir-backed `FsTenantStore` so the
/// tenant-CRUD + audit tools are fully wired.
async fn start_with_store() -> Harness {
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
            gateway_version: Some("1.0.0-test".to_owned()),
            tenant_store: Some(tenant_store),
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

/// Spawn a gateway with quota enforcement wired so `quota_get` returns
/// a meaningful snapshot.
async fn start_with_quota() -> EscurelProcess {
    let quota = Arc::new(QuotaManager::new(QuotaConfig {
        queries_per_minute: QUOTA_QUERIES,
        writes_per_minute: QUOTA_WRITES,
        embeds_per_minute: QUOTA_EMBEDS,
        concurrent_sessions: 4,
        max_blob_bytes: 25 * 1024 * 1024,
    }));
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("customer", CUSTOMER_SKILL)
                .done(),
        ),
        config_overrides: ConfigOverrides {
            quota: Some(quota),
            ..Default::default()
        },
    })
    .await
}

async fn admin_client(p: &EscurelProcess) -> AdminClient {
    let token = p.mint_token(TENANT, Role::Admin);
    AdminClient::connect(p.base_url(), SecretString::from(token))
        .await
        .unwrap()
}

async fn agent_admin_client(p: &EscurelProcess) -> AdminClient {
    let token = p.mint_token(TENANT, Role::Agent);
    AdminClient::connect(p.base_url(), SecretString::from(token))
        .await
        .unwrap()
}

async fn agent_client(p: &EscurelProcess) -> Client {
    let token = p.mint_token(TENANT, Role::Agent);
    Client::connect(p.base_url(), SecretString::from(token))
        .await
        .unwrap()
}

fn spec(id: &str, name: &str) -> TenantSpec {
    TenantSpec {
        tenant_id: id.to_owned(),
        display_name: name.to_owned(),
        ..Default::default()
    }
}

// --- tests ---------------------------------------------------------

#[tokio::test]
async fn health_returns_configured_version() {
    let h = start_with_store().await;
    let client = admin_client(&h.process).await;
    let resp = client.health(HealthRequest::default()).await.unwrap();
    assert_eq!(resp.version, "1.0.0-test");
    assert!(!resp.status.is_empty());
    h.process.shutdown().await;
}

/// Full operator lifecycle through the typed client: create → get →
/// list → update → delete, with on-disk provisioning verified.
#[tokio::test]
async fn tenant_crud_lifecycle_round_trips() {
    let h = start_with_store().await;
    let client = admin_client(&h.process).await;

    let created = client
        .tenant_create(TenantCreateRequest {
            spec: Some(spec("globex", "Globex Corp")),
        })
        .await
        .unwrap();
    assert_eq!(created.spec.as_ref().unwrap().tenant_id, "globex");
    assert!(h.tenants_root.join("globex").join("tenant.json").is_file());
    assert!(
        h.tenants_root
            .join("globex")
            .join("db")
            .join("escurel.duckdb")
            .is_file()
    );

    let got = client
        .tenant_get(TenantGetRequest {
            tenant_id: "globex".to_owned(),
        })
        .await
        .unwrap();
    assert_eq!(got.spec.unwrap().display_name, "Globex Corp");

    let listed = client
        .tenant_list(TenantListRequest::default())
        .await
        .unwrap();
    assert!(listed.tenants.iter().any(|t| t.tenant_id == "globex"));

    let updated = client
        .tenant_update(TenantUpdateRequest {
            spec: Some(spec("globex", "Globex Renamed")),
        })
        .await
        .unwrap();
    assert_eq!(updated.spec.unwrap().display_name, "Globex Renamed");

    let deleted = client
        .tenant_delete(TenantDeleteRequest {
            tenant_id: "globex".to_owned(),
            confirm: Some("globex".to_owned()),
        })
        .await
        .unwrap();
    assert!(deleted.deleted);
    assert!(!h.tenants_root.join("globex").exists());

    h.process.shutdown().await;
}

#[tokio::test]
async fn audit_reports_clean_drift_for_seeded_tenant() {
    let h = start_with_store().await;
    let client = admin_client(&h.process).await;
    let resp = client
        .audit(AuditRequest {
            tenant_id: TENANT.to_owned(),
            scope: String::new(),
        })
        .await
        .unwrap();
    assert!(resp.markdown_not_in_duckdb.is_empty());
    assert!(resp.indexed_but_no_markdown.is_empty());
    h.process.shutdown().await;
}

#[tokio::test]
async fn quota_get_snapshots_remaining_budget() {
    let p = start_with_quota().await;
    let client = admin_client(&p).await;
    let snap = client
        .quota_get(QuotaGetRequest {
            tenant_id: TENANT.to_owned(),
        })
        .await
        .unwrap();
    // Fresh tenant, no agent calls made: full configured budget.
    assert_eq!(snap.queries_remaining, QUOTA_QUERIES);
    assert_eq!(snap.writes_remaining, QUOTA_WRITES);
    assert_eq!(snap.embeds_remaining, QUOTA_EMBEDS);
    p.shutdown().await;
}

/// Realistic GDPR flow: an agent writes chat history, the operator
/// erases that group through the admin client, and it's gone.
#[tokio::test]
async fn delete_chat_history_erases_a_group() {
    let p = start_with_quota().await;
    let agent = agent_client(&p).await;
    let admin = admin_client(&p).await;

    for content in ["first", "second"] {
        agent
            .append_message(AppendMessageRequest {
                chat_group_id: "room-gdpr".to_owned(),
                role: "user".to_owned(),
                content: content.to_owned(),
                author: "alice".to_owned(),
                embed: false,
                ..Default::default()
            })
            .await
            .unwrap();
    }

    let resp = admin
        .delete_chat_history(DeleteChatHistoryRequest {
            tenant_id: TENANT.to_owned(),
            chat_group_id: "room-gdpr".to_owned(),
            before_ts: String::new(),
            author: String::new(),
        })
        .await
        .unwrap();
    assert_eq!(resp.deleted, 2, "both messages should be erased");

    let after = agent
        .list_messages(ListMessagesRequest {
            chat_group_id: "room-gdpr".to_owned(),
            direction: "asc".to_owned(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        after.messages.is_empty(),
        "group should be empty post-erase"
    );
    p.shutdown().await;
}

#[tokio::test]
async fn agent_role_is_rejected_on_admin_tool() {
    let h = start_with_store().await;
    let client = agent_admin_client(&h.process).await;
    let err = client
        .tenant_list(TenantListRequest::default())
        .await
        .unwrap_err();
    match err {
        ClientError::JsonRpc { code, .. } => {
            assert_eq!(
                code, JSONRPC_ADMIN_REQUIRED,
                "agent role must be denied with the admin-required JSON-RPC code"
            );
        }
        other => panic!("expected Error::JsonRpc(admin required), got {other:?}"),
    }
    h.process.shutdown().await;
}
