//! A graceful stop must END. `axum`'s graceful shutdown waits for every in-flight request for ever,
//! so one request stuck on a hung upstream held the whole pet past the orchestrator's patience and
//! it was SIGKILLed mid-write. The drain has a deadline: after it, what is still running is aborted.
//!
//! Real gateway, a real upstream that never answers, a real shutdown.

use std::time::{Duration, Instant};

use axum::Router;
use axum::routing::get;
use escurel_test_support::{EgressPolicy, Role};
use serde_json::json;

use super::remote_support::{admin, call_as, serve, spawn_gateway_with};

const SKILL: &str = "---\n\
     kind: skill\n\
     id: thing\n\
     description: Things behind a REST service that hangs.\n\
     backend:\n\
    \x20 kind: openapi\n\
    \x20 endpoint: hung\n\
    \x20 instances: rows\n\
    \x20 key: $.id\n\
    \x20 list: { path: /things, items: $.data }\n\
    \x20 read: { path: \"/things/{id}\" }\n\
    \x20 project: { name: $.name }\n\
     ---\n\
     # thing\n";

#[tokio::test]
async fn a_stop_does_not_wait_for_ever_on_a_request_that_is_stuck_on_a_hung_upstream() {
    let app = Router::new().route(
        "/things",
        get(|| async {
            tokio::time::sleep(Duration::from_secs(120)).await;
            "never"
        }),
    );
    let (base, _h) = serve(app).await;
    let (p, _dirs) = spawn_gateway_with(
        &[("thing", SKILL)],
        EgressPolicy {
            allow_loopback: true,
            timeout: Duration::from_secs(300),
            ..EgressPolicy::default()
        },
        Some(Duration::from_millis(500)),
    )
    .await;
    admin(
        &p,
        "register_endpoint",
        json!({ "name": "hung", "kind": "openapi", "base_url": base }),
    )
    .await;

    // A request that will not finish on its own.
    let url = p.mcp_url();
    let token = p.mint_token(super::remote_support::TENANT, Role::Admin);
    let stuck = tokio::spawn(async move {
        let _ = reqwest::Client::new()
            .post(url)
            .header("authorization", format!("Bearer {token}"))
            .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                           "params": { "name": "list_instances",
                                       "arguments": { "skill_id": "thing", "limit": 5 } } }))
            .send()
            .await;
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    let _ = call_as(&p, Role::Admin, "list_skills", json!({})).await;

    let started = Instant::now();
    p.shutdown().await;
    let took = started.elapsed();

    assert!(
        took < Duration::from_secs(10),
        "the stop was held by the stuck request for {took:?}: it must give up after the drain deadline"
    );
    stuck.abort();
}
