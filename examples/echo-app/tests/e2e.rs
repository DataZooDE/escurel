//! End-to-end acceptance test for the dx.md §"Chaining recipe".
//!
//! This is the executable proof that the
//! `escurel-client` + `escurel-test-support` façade lets a
//! consuming application stand up the full
//! `escurel → app-backend → HTTP client` chain in one file with no
//! mocks at the boundaries.
//!
//! Triton is not a sibling repo here; the echo-app backend
//! substitutes for the application-specific HTTP edge that, in the
//! full recipe, would be reached *through* triton. The shape of
//! this test still reads like the snippet in
//! [`docs/spec/dx.md`](../../../docs/spec/dx.md) §"Chaining recipe".

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};

#[tokio::test]
async fn dashboard_round_trips_through_echo_app() {
    // 1. escurel up, with a tenant seeded.
    let escurel = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant("acme")
                .skill("customer", include_str!("fixtures/customer.skill.md"))
                .instance(
                    "customer",
                    "acme-corp",
                    include_str!("fixtures/acme-corp.md"),
                )
                .done(),
        ),
        ..Default::default()
    })
    .await;
    let token = escurel.mint_token("acme", Role::Agent);

    // 2. echo-app backend up, pointed at escurel's gRPC endpoint.
    let backend = echo_app::spawn(echo_app::Opts {
        escurel_endpoint: escurel.grpc_endpoint().to_owned(),
        escurel_token: token,
    })
    .await
    .expect("echo-app spawn");

    // 3. drive an HTTP request through the backend — the shape a
    //    real frontend (or triton) would take.
    let resp = reqwest::Client::new()
        .get(format!("{}/pages/acme-corp", backend.base_url()))
        .send()
        .await
        .expect("GET /pages/acme-corp");
    assert_eq!(
        resp.status(),
        200,
        "echo-app must return 200 for a seeded instance"
    );
    let body = resp.text().await.expect("body");

    // The instance fixture's body must round-trip through
    // resolve→expand→HTTP.
    assert!(
        body.contains("Acme Corp"),
        "echo-app body must contain the instance title; got: {body}"
    );

    backend.shutdown().await;
    escurel.shutdown().await;
}

#[tokio::test]
async fn unknown_slug_returns_404() {
    // Same wiring, missing fixture. The contract is: resolve says
    // the wikilink does not exist, the backend translates that into
    // a 404 rather than a 5xx.
    let escurel = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        ..Default::default()
    })
    .await;
    let token = escurel.mint_token("acme", Role::Agent);

    let backend = echo_app::spawn(echo_app::Opts {
        escurel_endpoint: escurel.grpc_endpoint().to_owned(),
        escurel_token: token,
    })
    .await
    .expect("echo-app spawn");

    let resp = reqwest::Client::new()
        .get(format!("{}/pages/does-not-exist", backend.base_url()))
        .send()
        .await
        .expect("GET /pages/does-not-exist");
    assert_eq!(resp.status(), 404);

    backend.shutdown().await;
    escurel.shutdown().await;
}
