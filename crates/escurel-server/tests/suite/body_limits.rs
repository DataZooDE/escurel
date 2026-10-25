//! The request-body limit of `/mcp` and `/ingest/upload` is configurable
//! (`ESCUREL_MCP_MAX_BODY_BYTES`, `ESCUREL_INGEST_MAX_BODY_BYTES`); the default is 16 MiB.
//! Real gateway over HTTP, real config parser.

use std::collections::HashMap;

use base64::Engine as _;
use escurel_server::EscurelConfig;
use escurel_server::config::ConfigError;
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::json;

const TENANT: &str = "acme";
const MEMO: &str = "---\nkind: skill\nid: memo\ndescription: Memos.\nbackend:\n  kind: document\n  accepts: [text/plain]\n---\n# memo\n";

#[allow(clippy::result_large_err)]
fn cfg(pairs: &[(&str, &str)]) -> Result<EscurelConfig, ConfigError> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    EscurelConfig::from_source(&move |k: &str| map.get(k).cloned())
}

#[test]
fn the_default_is_sixteen_mib_and_the_env_overrides_it() {
    let c = cfg(&[]).unwrap();
    assert_eq!(c.mcp_max_body_bytes, 16 * 1024 * 1024);
    assert_eq!(c.ingest_max_body_bytes, 16 * 1024 * 1024);
    let c = cfg(&[
        ("ESCUREL_MCP_MAX_BODY_BYTES", "4096"),
        ("ESCUREL_INGEST_MAX_BODY_BYTES", "33554432"),
    ])
    .unwrap();
    assert_eq!(c.mcp_max_body_bytes, 4096);
    assert_eq!(c.ingest_max_body_bytes, 32 * 1024 * 1024);
    for bad in ["0", "-1", "lots"] {
        let err = cfg(&[("ESCUREL_INGEST_MAX_BODY_BYTES", bad)]).expect_err(bad);
        assert!(
            matches!(
                err,
                ConfigError::InvalidValue {
                    var: "ESCUREL_INGEST_MAX_BODY_BYTES",
                    ..
                }
            ),
            "{err}"
        );
    }
}

async fn start(overrides: ConfigOverrides) -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("memo", MEMO)
                .done(),
        ),
        config_overrides: overrides,
    })
    .await
}

async fn upload(p: &EscurelProcess, bytes: usize) -> reqwest::StatusCode {
    let token = p.mint_token(TENANT, Role::Admin);
    reqwest::Client::new()
        .post(format!("{}/ingest/upload", p.base_url()))
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "content_type": "text/plain",
            "bytes_b64": base64::engine::general_purpose::STANDARD.encode(vec![b'a'; bytes]),
        }))
        .send()
        .await
        .unwrap()
        .status()
}

async fn mcp_status(p: &EscurelProcess, padding: usize) -> reqwest::StatusCode {
    let token = p.mint_token(TENANT, Role::Admin);
    reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "list_skills", "arguments": {}, "pad": "x".repeat(padding) },
        }))
        .send()
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn ingest_upload_limit_follows_the_config() {
    // 13 MB of bytes is ~17.3 MB of base64: over the 16 MiB default, under a raised limit.
    let default = start(ConfigOverrides::default()).await;
    assert_eq!(upload(&default, 13_000_000).await, 413);
    assert!(upload(&default, 1000).await.is_success());
    default.shutdown().await;

    let raised = start(ConfigOverrides {
        ingest_max_body_bytes: Some(64 * 1024 * 1024),
        ..Default::default()
    })
    .await;
    assert!(upload(&raised, 13_000_000).await.is_success());
    raised.shutdown().await;

    let tight = start(ConfigOverrides {
        ingest_max_body_bytes: Some(4096),
        ..Default::default()
    })
    .await;
    assert_eq!(upload(&tight, 6000).await, 413);
    tight.shutdown().await;
}

#[tokio::test]
async fn mcp_limit_follows_the_config() {
    let default = start(ConfigOverrides::default()).await;
    assert_eq!(mcp_status(&default, 17_000_000).await, 413);
    assert!(mcp_status(&default, 10).await.is_success());
    default.shutdown().await;

    let raised = start(ConfigOverrides {
        mcp_max_body_bytes: Some(64 * 1024 * 1024),
        ..Default::default()
    })
    .await;
    assert!(mcp_status(&raised, 17_000_000).await.is_success());
    raised.shutdown().await;
}
