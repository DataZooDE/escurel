//! Dispatch-level tool aliases + scope-driven quota exemption
//! (2026-08-14 API review, naming B1 + the advocate's concession 3).
//!
//! - The noun-first `tenant_*` family and `embedding_reload` gain
//!   verb-first ALIASES resolved before the dispatch match
//!   (`create_tenant` → `tenant_create`, …). Canonical names stay the
//!   only advertised ones — the alias is a courtesy for callers using
//!   the surface's dominant verb-first convention, not a rename.
//! - `dimension_for`'s admin exemption keys on the `scope` label
//!   instead of the `admin_` prefix + a hand-kept `matches!` list —
//!   the drift trap where an unprefixed admin tool silently debited
//!   the tenant's *agent* rate budget. All 40+ admin-scope tools are
//!   exempt now, not just the prefixed/remembered ones.

use std::sync::Arc;

use escurel_admin::{FsTenantStore, TenantStore};
use escurel_quota::{QuotaConfig, QuotaManager};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};
use tempfile::TempDir;

const TENANT: &str = "stuttgart-ai";

async fn call(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
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
async fn verb_first_aliases_route_to_the_canonical_tool() {
    let tenants_dir = TempDir::new().unwrap();
    let tenant_store: Arc<dyn TenantStore> =
        Arc::new(FsTenantStore::new(tenants_dir.path().to_path_buf()));
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            tenant_store: Some(tenant_store),
            ..Default::default()
        },
        fixtures: Some(FixtureBuilder::new().tenant(TENANT).done()),
    })
    .await;
    let admin = p.mint_token(TENANT, Role::Admin);

    // Create through the verb-first spelling…
    let created = call(
        &p,
        &admin,
        "create_tenant",
        json!({ "tenant_id": TENANT, "display_name": "Stuttgart AI" }),
    )
    .await;
    assert!(
        created.get("error").is_none(),
        "create_tenant alias: {created}"
    );

    // …and read it back through another one.
    let listed = call(&p, &admin, "list_tenants", json!({})).await;
    assert!(
        listed.get("error").is_none(),
        "list_tenants alias: {listed}"
    );
    let names = listed["result"]["structuredContent"].to_string();
    assert!(
        names.contains(TENANT),
        "the created tenant is listed: {listed}"
    );

    // The aliases are dispatch-level ONLY: tools/list advertises the
    // canonical names, never the alias twins.
    let body: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {admin}"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json");
    let advertised: Vec<&str> = body["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert!(advertised.contains(&"tenant_create"), "canonical stays");
    for alias in ["create_tenant", "list_tenants", "reload_embedding"] {
        assert!(
            !advertised.contains(&alias),
            "alias `{alias}` must not be advertised"
        );
    }
}

/// An admin-scope tool must not debit the tenant's *agent* rate budget —
/// keyed on the `scope` label now, so unprefixed admin tools (the ones
/// the old hand-kept exemption list forgot) are exempt too.
#[tokio::test]
async fn unprefixed_admin_tools_do_not_debit_the_agent_budget() {
    let q = QuotaConfig {
        queries_per_minute: 1,
        writes_per_minute: 60,
        embeds_per_minute: 60,
        concurrent_sessions: 32,
        max_blob_bytes: 25 * 1024 * 1024,
    };
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            quota: Some(Arc::new(QuotaManager::new(q))),
            ..Default::default()
        },
        fixtures: Some(FixtureBuilder::new().tenant(TENANT).done()),
    })
    .await;
    let admin = p.mint_token(TENANT, Role::Admin);

    // `list_credentials` is admin-gated but carries no `admin_` prefix —
    // exactly the tool the old exemption list forgot. Two calls with a
    // 1-query/minute budget: both must pass, or the operator's own
    // tooling is eating the tenant's agent budget.
    for attempt in 1..=2u8 {
        let out = call(&p, &admin, "list_credentials", json!({})).await;
        assert!(
            out.get("error").is_none(),
            "admin call {attempt} must not be rate-limited: {out}"
        );
    }
}

/// Codex-review P2: shipped consumers (explorer-kit, the seeded
/// meta-skill) still post the retired `run_stored_query`. The alias
/// layer routes the legacy name to `query_instance` — whose `query_id`
/// alias binds the legacy argument and whose response is a superset —
/// so old callers keep working instead of hitting method-not-found.
/// (The legacy tool was admin-gated; the routed target enforces the
/// per-instance ACL, so this is never a privilege increase.)
#[tokio::test]
async fn legacy_run_stored_query_routes_to_query_instance() {
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides::default(),
        fixtures: Some(FixtureBuilder::new().tenant(TENANT).done()),
    })
    .await;
    let admin = p.mint_token(TENANT, Role::Admin);
    let out = call(&p, &admin, "run_stored_query", json!({})).await;
    // The routed target refuses the missing ref as invalid params —
    // NOT `-32601 method not found`, which is what a dropped tool
    // answers and what a legacy caller cannot recover from.
    assert_ne!(
        out["error"]["code"],
        json!(-32601),
        "legacy name must route, not vanish: {out}"
    );
    assert_eq!(out["error"]["code"], json!(-32602), "{out}");
}
