//! End-to-end tests for the auth + quota middleware on /mcp.
//!
//! Real gateway, real Indexer (DuckDB + FsStore + ZeroEmbedder),
//! real OidcVerifier against the in-process JWKS the support
//! crate stands up, real QuotaManager.

use std::sync::Arc;

use escurel_quota::{QuotaConfig, QuotaManager};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "acme";

const CUSTOMER_SKILL: &str = "---\ntype: skill\nid: customer\ndescription: x\n---\n# customer\n";

async fn start_authed(quota: Option<Arc<QuotaManager>>) -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("customer", CUSTOMER_SKILL)
                .done(),
        ),
        config_overrides: ConfigOverrides {
            quota,
            ..Default::default()
        },
    })
    .await
}

async fn post_mcp(p: &EscurelProcess, bearer: Option<&str>, body: Value) -> reqwest::Response {
    let mut req = reqwest::Client::new().post(p.mcp_url()).json(&body);
    if let Some(t) = bearer {
        req = req.header("authorization", format!("Bearer {t}"));
    }
    req.send().await.unwrap()
}

fn list_skills_call() -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": "list_skills", "arguments": {} }
    })
}

#[tokio::test]
async fn missing_bearer_returns_401() {
    let p = start_authed(None).await;
    let resp = post_mcp(&p, None, list_skills_call()).await;
    assert_eq!(resp.status(), 401);
    p.shutdown().await;
}

#[tokio::test]
async fn bearer_without_prefix_returns_401() {
    let p = start_authed(None).await;
    let valid = p.mint_token(TENANT, Role::Agent);
    let resp = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", valid) // missing "Bearer " prefix
        .json(&list_skills_call())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    p.shutdown().await;
}

#[tokio::test]
async fn invalid_token_returns_401() {
    let p = start_authed(None).await;
    let resp = post_mcp(&p, Some("not.a.real.jwt"), list_skills_call()).await;
    assert_eq!(resp.status(), 401);
    p.shutdown().await;
}

#[tokio::test]
async fn valid_token_lets_request_through() {
    let p = start_authed(None).await;
    let t = p.mint_token(TENANT, Role::Agent);
    let resp = post_mcp(&p, Some(&t), list_skills_call()).await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert!(body["result"]["skills"].is_array());
    p.shutdown().await;
}

#[tokio::test]
async fn quota_exhaustion_returns_429_with_retry_after_header() {
    let q = QuotaConfig {
        queries_per_minute: 1, // 1 per minute
        writes_per_minute: 60,
        embeds_per_minute: 60,
        concurrent_sessions: 32,
    };
    let p = start_authed(Some(Arc::new(QuotaManager::new(q)))).await;
    let t = p.mint_token(TENANT, Role::Agent);

    // First call succeeds.
    let resp = post_mcp(&p, Some(&t), list_skills_call()).await;
    assert_eq!(resp.status(), 200);

    // Second call exhausts.
    let resp = post_mcp(&p, Some(&t), list_skills_call()).await;
    assert_eq!(resp.status(), 429);
    let retry = resp
        .headers()
        .get("Retry-After-Ms")
        .map(|v| v.to_str().unwrap().to_owned());
    assert!(retry.is_some(), "Retry-After-Ms header must be present");
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], -32000);

    p.shutdown().await;
}

#[tokio::test]
async fn write_tool_debits_writes_dimension_independently() {
    // Quotas: 60 queries/min, 1 write/min.
    let q = QuotaConfig {
        queries_per_minute: 60,
        writes_per_minute: 1,
        embeds_per_minute: 60,
        concurrent_sessions: 32,
    };
    let p = start_authed(Some(Arc::new(QuotaManager::new(q)))).await;
    let t = p.mint_token(TENANT, Role::Agent);

    let write_body = "---\ntype: instance\nskill: customer\nid: one\n---\n# One\n";
    let write_call = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": "update_page", "arguments": {
            "page_id": "markdown/instances/customer/one.md",
            "content": write_body,
        }}
    });

    // First write succeeds; second exhausts.
    assert_eq!(
        post_mcp(&p, Some(&t), write_call.clone()).await.status(),
        200
    );
    let resp = post_mcp(
        &p,
        Some(&t),
        json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": { "name": "update_page", "arguments": {
                "page_id": "markdown/instances/customer/two.md",
                "content": write_body.replace("one", "two"),
            }}
        }),
    )
    .await;
    assert_eq!(resp.status(), 429);

    // But a read still goes through (independent bucket).
    let resp = post_mcp(&p, Some(&t), list_skills_call()).await;
    assert_eq!(resp.status(), 200);

    p.shutdown().await;
}

#[tokio::test]
async fn tools_list_does_not_debit_quota() {
    let q = QuotaConfig {
        queries_per_minute: 1, // only 1 query budget
        writes_per_minute: 60,
        embeds_per_minute: 60,
        concurrent_sessions: 32,
    };
    let p = start_authed(Some(Arc::new(QuotaManager::new(q)))).await;
    let t = p.mint_token(TENANT, Role::Agent);

    // tools/list should not debit; we can call it 5 times.
    for _ in 0..5 {
        let resp = post_mcp(
            &p,
            Some(&t),
            json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/list"
            }),
        )
        .await;
        assert_eq!(resp.status(), 200, "tools/list should not be rate-limited");
    }
    // After 5 tools/list, the queries bucket is still fresh and a
    // single tools/call can succeed.
    let resp = post_mcp(&p, Some(&t), list_skills_call()).await;
    assert_eq!(resp.status(), 200);

    p.shutdown().await;
}

#[tokio::test]
async fn tenants_have_independent_quota_state() {
    let q = QuotaConfig {
        queries_per_minute: 1,
        writes_per_minute: 60,
        embeds_per_minute: 60,
        concurrent_sessions: 32,
    };
    let p = start_authed(Some(Arc::new(QuotaManager::new(q)))).await;

    let t_acme = p.mint_token("acme", Role::Agent);
    let t_globex = p.mint_token("globex", Role::Agent);

    assert_eq!(
        post_mcp(&p, Some(&t_acme), list_skills_call())
            .await
            .status(),
        200
    );
    assert_eq!(
        post_mcp(&p, Some(&t_acme), list_skills_call())
            .await
            .status(),
        429
    );
    // Globex's bucket is still fresh.
    assert_eq!(
        post_mcp(&p, Some(&t_globex), list_skills_call())
            .await
            .status(),
        200
    );

    p.shutdown().await;
}
