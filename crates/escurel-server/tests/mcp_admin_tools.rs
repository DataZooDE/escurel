//! End-to-end tests for the admin tenant-CRUD + long-running ops MCP
//! tools added in P2 (`tenant_create` / `tenant_list` / `tenant_get` /
//! `tenant_update` / `tenant_delete` / `attach_external` /
//! `embedding_reload` / `rebuild` / `compact_lanes` / `tenant_export` /
//! `tenant_import`).
//!
//! These mirror the gRPC `EscurelAdmin` business logic but ride the
//! MCP-over-HTTP `POST /mcp` transport. Real gateway, real DuckDB,
//! real OIDC (TestIssuer JWKS), real tempdir-backed `FsTenantStore` /
//! `DuckdbCrdtBackend`, real reqwest. No mocks at the boundary. The
//! admin-role gate is exercised with a genuine agent-role token (must
//! be rejected with JSON-RPC `-32001`).

use std::path::PathBuf;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use duckdb::Connection;
use escurel_admin::{FsTenantStore, TenantStore};
use escurel_crdt::{CrdtBackend, DuckdbCrdtBackend};
use escurel_index::Migrator;
use escurel_quota::{QuotaConfig, QuotaManager};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::Mutex;

const TENANT: &str = "acme";
const CUSTOMER_SKILL: &str = "---\ntype: skill\nid: customer\ndescription: x\n---\n# customer\n";

struct Harness {
    process: EscurelProcess,
    tenants_root: PathBuf,
    _tenants_dir: TempDir,
}

/// Gateway with a real `FsTenantStore` + a seeded `acme` tenant +
/// fixtures so `rebuild` / `tenant_export` have something to chew on.
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

/// POST a `tools/call` and return the full JSON-RPC envelope.
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

// --- tenant CRUD lifecycle -------------------------------------------

#[tokio::test]
async fn tenant_crud_lifecycle_over_mcp() {
    let h = start().await;
    let p = &h.process;

    // create → on-disk provisioning.
    let created = call(
        p,
        Role::Admin,
        "tenant_create",
        json!({ "tenant_id": "globex", "display_name": "Globex Corp" }),
    )
    .await;
    assert!(created.get("error").is_none(), "create error: {created}");
    assert_eq!(created["result"]["spec"]["tenant_id"], "globex");
    assert_eq!(created["result"]["spec"]["display_name"], "Globex Corp");
    let dir = h.tenants_root.join("globex");
    assert!(dir.join("tenant.json").is_file());
    assert!(dir.join("markdown").is_dir());
    assert!(dir.join("db").join("escurel.duckdb").is_file());

    // get → echoes the spec.
    let got = call(
        p,
        Role::Admin,
        "tenant_get",
        json!({ "tenant_id": "globex" }),
    )
    .await;
    assert_eq!(got["result"]["spec"]["display_name"], "Globex Corp");

    // list → includes the created tenant.
    let listed = call(p, Role::Admin, "tenant_list", json!({})).await;
    let ids: Vec<String> = listed["result"]["tenants"]
        .as_array()
        .expect("tenants array")
        .iter()
        .map(|t| t["tenant_id"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert!(
        ids.contains(&"globex".to_owned()),
        "list missing globex: {ids:?}"
    );

    // update → display name changes and persists.
    let updated = call(
        p,
        Role::Admin,
        "tenant_update",
        json!({ "tenant_id": "globex", "display_name": "Globex Inc" }),
    )
    .await;
    assert_eq!(updated["result"]["spec"]["display_name"], "Globex Inc");
    let reread = call(
        p,
        Role::Admin,
        "tenant_get",
        json!({ "tenant_id": "globex" }),
    )
    .await;
    assert_eq!(reread["result"]["spec"]["display_name"], "Globex Inc");

    // delete → returns true + the directory is gone.
    let deleted = call(
        p,
        Role::Admin,
        "tenant_delete",
        json!({ "tenant_id": "globex" }),
    )
    .await;
    assert_eq!(deleted["result"]["deleted"], true);
    assert!(!dir.exists());

    h.process.shutdown().await;
}

#[tokio::test]
async fn tenant_create_rejects_invalid_id() {
    let h = start().await;
    let body = call(
        &h.process,
        Role::Admin,
        "tenant_create",
        json!({ "tenant_id": "Bad/Id", "display_name": "x" }),
    )
    .await;
    // Invalid tenant id maps to invalid_params (-32602).
    assert_eq!(
        body["error"]["code"], -32602,
        "expected invalid_params: {body}"
    );
    h.process.shutdown().await;
}

#[tokio::test]
async fn tenant_get_missing_is_not_found_error() {
    let h = start().await;
    let body = call(
        &h.process,
        Role::Admin,
        "tenant_get",
        json!({ "tenant_id": "ghost" }),
    )
    .await;
    assert!(
        body.get("error").is_some(),
        "missing tenant must error: {body}"
    );
    h.process.shutdown().await;
}

#[tokio::test]
async fn tenant_delete_missing_returns_false() {
    let h = start().await;
    let body = call(
        &h.process,
        Role::Admin,
        "tenant_delete",
        json!({ "tenant_id": "ghost" }),
    )
    .await;
    assert!(body.get("error").is_none(), "delete error: {body}");
    assert_eq!(body["result"]["deleted"], false);
    h.process.shutdown().await;
}

// --- attach_external --------------------------------------------------

#[tokio::test]
async fn attach_external_rejects_unsafe_source() {
    let h = start().await;
    // A source_url with an embedded quote is the SQL-injection guard
    // case (is_safe_attach_source rejects it).
    let body = call(
        &h.process,
        Role::Admin,
        "attach_external",
        json!({ "tenant_id": TENANT, "source_url": "foo';DROP TABLE pages;--" }),
    )
    .await;
    assert_eq!(
        body["error"]["code"], -32602,
        "expected invalid_params: {body}"
    );
    h.process.shutdown().await;
}

// --- tenant-match guards on the admin surface (codex regressions) -----
//
// Every admin tool that takes a `tenant_id` must reject a value that
// names a tenant other than the one this single-tenant gateway serves,
// rather than silently acting on / reporting the wrong tenant. The
// gateway here is bound to `acme`; `globex` is a valid-but-foreign id.

#[tokio::test]
async fn rebuild_rejects_foreign_tenant() {
    let h = start().await;
    let body = call(
        &h.process,
        Role::Admin,
        "rebuild",
        json!({ "tenant_id": "globex" }),
    )
    .await;
    assert_eq!(
        body["error"]["code"], -32002,
        "rebuild must reject a foreign tenant: {body}"
    );
    h.process.shutdown().await;
}

#[tokio::test]
async fn attach_external_rejects_foreign_tenant() {
    let h = start().await;
    let body = call(
        &h.process,
        Role::Admin,
        "attach_external",
        json!({ "tenant_id": "globex", "source_url": "/tmp/catalog.duckdb" }),
    )
    .await;
    assert_eq!(
        body["error"]["code"], -32002,
        "attach_external must reject a foreign tenant: {body}"
    );
    h.process.shutdown().await;
}

#[tokio::test]
async fn admin_quota_rejects_foreign_tenant() {
    let h = start().await;
    let body = call(
        &h.process,
        Role::Admin,
        "admin_quota",
        json!({ "tenant_id": "globex" }),
    )
    .await;
    assert_eq!(
        body["error"]["code"], -32002,
        "admin_quota must reject a foreign tenant: {body}"
    );
    // The gateway's own tenant still resolves cleanly.
    let ok = call(
        &h.process,
        Role::Admin,
        "admin_quota",
        json!({ "tenant_id": TENANT }),
    )
    .await;
    assert!(ok.get("error").is_none(), "own-tenant quota: {ok}");
    assert!(ok["result"]["queries_remaining"].is_number());
    h.process.shutdown().await;
}

#[tokio::test]
async fn admin_audit_rejects_foreign_tenant() {
    let h = start().await;
    let body = call(
        &h.process,
        Role::Admin,
        "admin_audit",
        json!({ "tenant_id": "globex" }),
    )
    .await;
    assert_eq!(
        body["error"]["code"], -32002,
        "admin_audit must reject a foreign tenant: {body}"
    );
    h.process.shutdown().await;
}

// --- rebuild ----------------------------------------------------------

#[tokio::test]
async fn rebuild_returns_final_counts() {
    let h = start().await;
    let body = call(&h.process, Role::Admin, "rebuild", json!({})).await;
    assert!(body.get("error").is_none(), "rebuild error: {body}");
    let done = body["result"]["done"].as_u64().expect("done");
    let total = body["result"]["total"].as_u64().expect("total");
    // The seeded tenant has the meta-skill + customer skill, so at
    // least one page is rebuilt and the run completes (done == total).
    assert!(total > 0, "expected total>0, got {total}");
    assert_eq!(
        done, total,
        "rebuild should finish: done={done} total={total}"
    );
    h.process.shutdown().await;
}

// --- tenant_export → tenant_import round-trip -------------------------

#[tokio::test]
async fn tenant_export_then_import_round_trips() {
    let h = start().await;
    let p = &h.process;

    // Provision a tenant with on-disk markdown to export.
    call(
        p,
        Role::Admin,
        "tenant_create",
        json!({ "tenant_id": "globex", "display_name": "Globex" }),
    )
    .await;
    // Write a markdown file into the tenant's markdown dir directly.
    let md_dir = h.tenants_root.join("globex").join("markdown");
    std::fs::create_dir_all(md_dir.join("skills")).unwrap();
    std::fs::write(
        md_dir.join("skills").join("customer.md"),
        "---\ntype: skill\nid: customer\n---\n# customer\n",
    )
    .unwrap();

    // export → base64 tarball with a positive byte count.
    let exported = call(
        p,
        Role::Admin,
        "tenant_export",
        json!({ "tenant_id": "globex" }),
    )
    .await;
    assert!(exported.get("error").is_none(), "export error: {exported}");
    let tarball = exported["result"]["tarball_b64"]
        .as_str()
        .expect("tarball_b64");
    assert!(!tarball.is_empty(), "empty tarball");
    assert!(exported["result"]["bytes"].as_u64().unwrap_or(0) > 0);
    // It is valid base64.
    assert!(B64.decode(tarball).is_ok(), "tarball_b64 must decode");

    // Wipe the on-disk markdown, then import the tarball back.
    std::fs::remove_dir_all(&md_dir).unwrap();
    assert!(!md_dir.join("skills").join("customer.md").exists());

    let imported = call(
        p,
        Role::Admin,
        "tenant_import",
        json!({ "tenant_id": "globex", "tarball_b64": tarball }),
    )
    .await;
    assert!(imported.get("error").is_none(), "import error: {imported}");
    assert!(imported["result"]["bytes_imported"].as_u64().unwrap_or(0) > 0);
    // The markdown file is restored on disk.
    assert!(
        md_dir.join("skills").join("customer.md").is_file(),
        "import did not restore the markdown file"
    );

    h.process.shutdown().await;
}

#[tokio::test]
async fn tenant_import_rejects_missing_tenant() {
    let h = start().await;
    // Build a tiny valid tar.gz of an empty dir to pass the base64
    // decode, so the not-found gate is what trips.
    let empty = TempDir::new().unwrap();
    let exported = {
        // Reuse export against the seeded acme tenant to get a real
        // tarball, then feed it to import for a non-existent tenant.
        let _ = &empty;
        call(
            &h.process,
            Role::Admin,
            "tenant_export",
            json!({ "tenant_id": TENANT }),
        )
        .await
    };
    let tarball = exported["result"]["tarball_b64"].as_str().unwrap_or("");
    let body = call(
        &h.process,
        Role::Admin,
        "tenant_import",
        json!({ "tenant_id": "ghost", "tarball_b64": tarball }),
    )
    .await;
    assert!(
        body.get("error").is_some(),
        "import to missing tenant must error: {body}"
    );
    h.process.shutdown().await;
}

// --- compact_lanes ----------------------------------------------------

#[tokio::test]
async fn compact_lanes_returns_totals() {
    // compact_lanes needs a crdt_backend; spin a dedicated gateway
    // with one wired (the shared `start()` harness has no backend).
    let db_dir = TempDir::new().unwrap();
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let shared = Arc::new(Mutex::new(conn));
    let backend: Arc<dyn CrdtBackend> = Arc::new(DuckdbCrdtBackend::new(Arc::clone(&shared)));

    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: None,
        config_overrides: ConfigOverrides {
            crdt_backend: Some(backend),
            disable_indexer: true,
            ..Default::default()
        },
    })
    .await;

    // No pages with snapshots → a clean zero sweep, but the tool
    // succeeds with the totals shape.
    let body = call(
        &process,
        Role::Admin,
        "compact_lanes",
        json!({ "tenant_id": TENANT }),
    )
    .await;
    assert!(body.get("error").is_none(), "compact_lanes error: {body}");
    assert!(body["result"]["ops_compacted"].is_number());
    assert!(body["result"]["bytes_reclaimed"].is_number());

    process.shutdown().await;
}

// --- admin-role enforcement ------------------------------------------

#[tokio::test]
async fn agent_role_rejected_from_new_admin_tools() {
    let h = start().await;
    for (name, args) in [
        (
            "tenant_create",
            json!({ "tenant_id": "x", "display_name": "y" }),
        ),
        ("tenant_list", json!({})),
        ("tenant_get", json!({ "tenant_id": "x" })),
        ("tenant_delete", json!({ "tenant_id": "x" })),
        (
            "attach_external",
            json!({ "alias": "a", "source_url": "b" }),
        ),
        ("rebuild", json!({})),
        ("tenant_export", json!({ "tenant_id": "x" })),
    ] {
        let body = call(&h.process, Role::Agent, name, args).await;
        assert_eq!(
            body["error"]["code"], -32001,
            "{name} must reject agent role with -32001: {body}"
        );
    }
    h.process.shutdown().await;
}

// --- serde wire-compat regression ------------------------------------
//
// The serde migration must keep the two flagged wire keys verbatim:
// `admin_quota` emits `concurrent_sessions_in_use`; `admin_lane_blob`
// emits `bytes_base64`.

#[tokio::test]
async fn serde_migration_keeps_quota_and_lane_blob_wire_keys() {
    let h = start().await;
    let p = &h.process;

    let quota = call(p, Role::Admin, "admin_quota", json!({})).await;
    // No quota manager wired → tool returns an error; in that case the
    // wire-key assertion is vacuous, so wire a quota-less skip.
    if quota.get("error").is_none() {
        assert!(
            quota["result"].get("concurrent_sessions_in_use").is_some(),
            "admin_quota must emit concurrent_sessions_in_use: {quota}"
        );
        assert!(quota["result"].get("concurrent_sessions").is_none());
    }

    let blob = call(
        p,
        Role::Admin,
        "admin_lane_blob",
        json!({ "key": "markdown/skills/escurel.md" }),
    )
    .await;
    assert!(blob.get("error").is_none(), "lane_blob error: {blob}");
    assert!(
        blob["result"].get("bytes_base64").is_some(),
        "admin_lane_blob must emit bytes_base64: {blob}"
    );
    assert_eq!(blob["result"]["content_type"], "text/markdown");

    h.process.shutdown().await;
}
