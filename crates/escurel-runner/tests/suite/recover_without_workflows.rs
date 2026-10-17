//! Startup crash recovery re-drives non-terminal workflow runs. A tenant that never declared the
//! `workflow-run` skill (heron's, for one) has none, and asking the gateway for that skill's instances is
//! answered `unknown_skill`: that is "nothing to recover", not a warning at every boot.
//!
//! Real gateway + DuckDB + client. No mocks.

use escurel_client::{Client, SecretString};
use escurel_runner_core::recover_workflows;
use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};

const TENANT: &str = "acme";

#[tokio::test]
async fn a_tenant_without_the_workflow_run_skill_has_nothing_to_recover() {
    // The fixture tenant declares no skills at all, so `workflow-run` is unknown to it.
    let gateway = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(FixtureBuilder::new().tenant(TENANT).done()),
        ..Default::default()
    })
    .await;
    let token = gateway.mint_token(TENANT, Role::Admin);
    let client = Client::connect(gateway.base_url(), SecretString::from(token))
        .await
        .expect("connect client");

    let resumed = recover_workflows(&client, 100)
        .await
        .expect("an unknown workflow-run skill means nothing to recover, not an error");
    assert_eq!(resumed, 0);
    gateway.shutdown().await;
}
