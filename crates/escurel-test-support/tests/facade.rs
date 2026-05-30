//! Integration tests for the `escurel-test-support` façade.
//!
//! These tests use **only** the public surface of
//! `escurel-test-support`. They must not import `wiremock`,
//! `jsonwebtoken`, or `rsa` directly — that is the contract from
//! `docs/spec/dx.md` §"Auth in tests" (commitment 1).
//!
//! Real `serve()`, real `Client`, real OIDC verifier (the support
//! crate stands up an in-process JWKS internally), real DuckDB +
//! FsStore via the same `update_page` write path production uses.

use escurel_client::{ListSkillsRequest, ResolveRequest, SearchRequest};
use escurel_test_support::{
    AuthMode, EscurelProcess, FixtureBuilder, ListSkillsRequest as McpListSkillsRequest, Opts,
    ResolveRequest as McpResolveRequest, Role, SearchRequest as McpSearchRequest,
};

const CUSTOMER_SKILL: &str = "---\n\
type: skill\n\
id: customer\n\
description: A buying organisation.\n\
---\n\
# customer\n";

const ACME_INSTANCE: &str = "---\n\
type: instance\n\
skill: customer\n\
id: acme\n\
---\n\
# Acme Corp\n\nFamous coyote-trap vendor.\n";

fn standard_fixture() -> FixtureBuilder {
    FixtureBuilder::new()
        .tenant("acme")
        .skill("customer", CUSTOMER_SKILL)
        .instance("customer", "acme", ACME_INSTANCE)
        .done()
}

#[tokio::test]
async fn spawn_with_test_issuer_returns_running_process_with_token() {
    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        ..Default::default()
    })
    .await;

    assert!(
        process.base_url().starts_with("http://127.0.0.1:"),
        "base_url must point at the loopback bind, got {}",
        process.base_url()
    );
    assert_eq!(process.mcp_url(), format!("{}/mcp", process.base_url()));

    // The minted token must be accepted by the running server.
    let token = process.mint_token("acme", Role::Agent);
    let resp = reqwest::Client::new()
        .post(process.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "minted token must be accepted");

    process.shutdown().await;
}

#[tokio::test]
async fn mint_token_works_for_agent_and_admin_roles() {
    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        ..Default::default()
    })
    .await;

    // Both tokens parse, are bearer-prefixed implicitly, and reach
    // the server. We don't yet have an admin-gated read tool on
    // /mcp, so the role gate here is *structural*: the token mints
    // are non-empty and the server accepts both.
    let agent = process.mint_token("acme", Role::Agent);
    let admin = process.mint_token("acme", Role::Admin);
    assert!(!agent.is_empty());
    assert!(!admin.is_empty());
    assert_ne!(agent, admin, "tokens differ by role claim");

    for token in [agent, admin] {
        let resp = reqwest::Client::new()
            .post(process.mcp_url())
            .header("authorization", format!("Bearer {token}"))
            .json(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/list",
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    process.shutdown().await;
}

#[tokio::test]
async fn fixture_builder_seeds_skills_and_instances_via_update_page() {
    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(standard_fixture()),
        ..Default::default()
    })
    .await;

    let client = process.client();
    let skills = client
        .list_skills(ListSkillsRequest::default())
        .await
        .unwrap();
    let ids: Vec<&str> = skills.skills.iter().map(|s| s.id.as_str()).collect();
    assert!(ids.contains(&"customer"), "got skills: {ids:?}");

    let resolved = client
        .resolve(ResolveRequest {
            wikilink: "[[customer::acme]]".to_owned(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(resolved.exists, "fixture instance must resolve");
    let page = resolved.page.expect("page present");
    assert_eq!(page.skill, "customer");
    assert_eq!(page.slug, "acme");

    process.shutdown().await;
}

#[tokio::test]
async fn client_and_mcp_client_both_round_trip_search() {
    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(standard_fixture()),
        ..Default::default()
    })
    .await;

    // 1. gRPC client surface.
    let client = process.client();
    let _ = client
        .search(SearchRequest {
            q: "Acme".to_owned(),
            k: 5,
            ..Default::default()
        })
        .await
        .unwrap();

    // 2. MCP-over-HTTP client surface.
    let mcp = process.mcp_client();
    let list = mcp
        .list_skills(McpListSkillsRequest::default())
        .await
        .unwrap();
    assert!(
        list.skills.iter().any(|s| s.id == "customer"),
        "mcp list_skills must include the seeded skill"
    );

    let resolved = mcp
        .resolve(McpResolveRequest {
            wikilink: "[[customer::acme]]".to_owned(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(resolved.exists);

    let search = mcp
        .search(McpSearchRequest {
            q: "Acme".to_owned(),
            k: 5,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(search.granularity, "block");

    process.shutdown().await;
}

#[tokio::test]
async fn multiple_processes_run_in_parallel_with_independent_data() {
    let a = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant("acme")
                .skill("customer", CUSTOMER_SKILL)
                .instance("customer", "acme", ACME_INSTANCE)
                .done(),
        ),
        ..Default::default()
    })
    .await;
    let b = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        // No fixtures — b is empty.
        ..Default::default()
    })
    .await;

    assert_ne!(
        a.base_url(),
        b.base_url(),
        "each spawn must bind a fresh port"
    );

    let a_skills = a
        .client()
        .list_skills(ListSkillsRequest::default())
        .await
        .unwrap();
    let b_skills = b
        .client()
        .list_skills(ListSkillsRequest::default())
        .await
        .unwrap();
    assert!(
        a_skills.skills.iter().any(|s| s.id == "customer"),
        "a was seeded with customer"
    );
    // b has no fixtures, so it carries only the mandatory `escurel`
    // meta-skill every tenant ships (locked decision 3) — and crucially
    // none of a's data, proving the two processes are independent.
    assert!(
        !b_skills.skills.iter().any(|s| s.id == "customer"),
        "b must not see a's customer fixture: {b_skills:?}"
    );
    assert!(
        b_skills.skills.iter().all(|s| s.id == "escurel"),
        "b carries only the meta-skill: {b_skills:?}"
    );

    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test]
async fn dropping_process_tears_down_cleanly() {
    let base_url;
    {
        let process = EscurelProcess::spawn(Opts {
            auth: AuthMode::Disabled,
            ..Default::default()
        })
        .await;
        base_url = process.base_url().to_owned();
        // healthz responds while alive.
        let resp = reqwest::Client::new()
            .get(format!("{base_url}/healthz"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }
    // After drop, the bound port should be free. Try to connect;
    // expect failure (server gone) within a short window.
    //
    // We retry briefly because graceful shutdown is async — Drop
    // signals the oneshot, the runtime polls the join, the port
    // is released. A handful of 50ms ticks is more than enough on
    // a healthy machine.
    let mut released = false;
    for _ in 0..20 {
        let probe = reqwest::Client::new()
            .get(format!("{base_url}/healthz"))
            .timeout(std::time::Duration::from_millis(100))
            .send()
            .await;
        if probe.is_err() {
            released = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(released, "process did not release its port after Drop");
}

#[tokio::test]
async fn auth_mode_disabled_skips_verifier() {
    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::Disabled,
        ..Default::default()
    })
    .await;

    // No Authorization header — the request must still reach the
    // dispatcher (and tools/list responds 200 even without an
    // indexer wired).
    let resp = reqwest::Client::new()
        .post(process.mcp_url())
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "Disabled mode must not gate /mcp");

    process.shutdown().await;
}
