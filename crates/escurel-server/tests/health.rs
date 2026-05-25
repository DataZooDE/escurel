//! End-to-end tests for the gateway's health surface. Spins up
//! the real server on a random port through `escurel-test-support`
//! and dials back with a real reqwest client. No mocks.

use std::sync::Arc;

use async_trait::async_trait;
use escurel_server::{ReadinessProbe, ReadinessReport};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, Opts};

struct StaticReport(ReadinessReport);

#[async_trait]
impl ReadinessProbe for StaticReport {
    async fn probe(&self) -> ReadinessReport {
        self.0.clone()
    }
}

fn ready_all_up() -> Arc<dyn ReadinessProbe> {
    Arc::new(StaticReport(ReadinessReport {
        lane_store: true,
        indexer: true,
        embedder: true,
    }))
}

fn ready_one_down() -> Arc<dyn ReadinessProbe> {
    Arc::new(StaticReport(ReadinessReport {
        lane_store: true,
        indexer: false,
        embedder: true,
    }))
}

async fn start(readiness: Arc<dyn ReadinessProbe>, version: &str) -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::Disabled,
        fixtures: None,
        config_overrides: ConfigOverrides {
            gateway_version: Some(version.to_owned()),
            readiness: Some(readiness),
            disable_grpc: true,
            disable_indexer: true,
            ..Default::default()
        },
    })
    .await
}

fn url(p: &EscurelProcess, path: &str) -> String {
    format!("{}{path}", p.base_url())
}

#[tokio::test]
async fn healthz_is_always_ok() {
    let p = start(ready_one_down(), "1.2.3-test").await; // even when other probes are down
    let resp = reqwest::get(url(&p, "/healthz")).await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "OK");
    p.shutdown().await;
}

#[tokio::test]
async fn readyz_returns_200_when_all_up() {
    let p = start(ready_all_up(), "1.2.3-test").await;
    let resp = reqwest::get(url(&p, "/readyz")).await.unwrap();
    assert_eq!(resp.status(), 200);
    p.shutdown().await;
}

#[tokio::test]
async fn readyz_returns_503_with_component_report_when_any_down() {
    let p = start(ready_one_down(), "1.2.3-test").await;
    let resp = reqwest::get(url(&p, "/readyz")).await.unwrap();
    assert_eq!(resp.status(), 503);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["ready"], false);
    assert_eq!(body["components"]["lane_store"], true);
    assert_eq!(body["components"]["indexer"], false);
    assert_eq!(body["components"]["embedder"], true);
    p.shutdown().await;
}

#[tokio::test]
async fn version_returns_configured_version() {
    let p = start(ready_all_up(), "1.2.3-test").await;
    let body = reqwest::get(url(&p, "/version"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(body, "1.2.3-test");
    p.shutdown().await;
}

#[tokio::test]
async fn metrics_returns_prometheus_text() {
    let p = start(ready_all_up(), "1.2.3-test").await;
    let resp = reqwest::get(url(&p, "/metrics")).await.unwrap();
    assert_eq!(resp.status(), 200);
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    assert!(
        ct.as_deref().unwrap_or("").starts_with("text/plain"),
        "content-type must be text/plain, got: {ct:?}",
    );
    let body = resp.text().await.unwrap();
    assert!(body.contains("# HELP"));
    assert!(body.contains("escurel_up 1"));
    p.shutdown().await;
}

#[tokio::test]
async fn unknown_path_returns_404() {
    let p = start(ready_all_up(), "1.2.3-test").await;
    let resp = reqwest::get(url(&p, "/does-not-exist")).await.unwrap();
    assert_eq!(resp.status(), 404);
    p.shutdown().await;
}
