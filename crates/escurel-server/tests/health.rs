//! End-to-end tests for the gateway's health surface. Spins up
//! the real server on a random port, dials back with a real
//! reqwest client. No mocks.

use std::sync::Arc;

use async_trait::async_trait;
use escurel_server::{ReadinessProbe, ReadinessReport, ServerConfig, serve};

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

async fn start(readiness: Arc<dyn ReadinessProbe>) -> escurel_server::ServerHandle {
    let cfg = ServerConfig {
        listen: "127.0.0.1:0".to_owned(),
        version: "1.2.3-test".to_owned(),
        readiness,
    };
    serve(cfg).await.expect("server starts")
}

fn url(h: &escurel_server::ServerHandle, path: &str) -> String {
    format!("http://{}{path}", h.local_addr)
}

#[tokio::test]
async fn healthz_is_always_ok() {
    let h = start(ready_one_down()).await; // even when other probes are down
    let resp = reqwest::get(url(&h, "/healthz")).await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "OK");
    h.shutdown().await;
}

#[tokio::test]
async fn readyz_returns_200_when_all_up() {
    let h = start(ready_all_up()).await;
    let resp = reqwest::get(url(&h, "/readyz")).await.unwrap();
    assert_eq!(resp.status(), 200);
    h.shutdown().await;
}

#[tokio::test]
async fn readyz_returns_503_with_component_report_when_any_down() {
    let h = start(ready_one_down()).await;
    let resp = reqwest::get(url(&h, "/readyz")).await.unwrap();
    assert_eq!(resp.status(), 503);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["ready"], false);
    assert_eq!(body["components"]["lane_store"], true);
    assert_eq!(body["components"]["indexer"], false);
    assert_eq!(body["components"]["embedder"], true);
    h.shutdown().await;
}

#[tokio::test]
async fn version_returns_configured_version() {
    let h = start(ready_all_up()).await;
    let body = reqwest::get(url(&h, "/version"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(body, "1.2.3-test");
    h.shutdown().await;
}

#[tokio::test]
async fn metrics_returns_prometheus_text() {
    let h = start(ready_all_up()).await;
    let resp = reqwest::get(url(&h, "/metrics")).await.unwrap();
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
    h.shutdown().await;
}

#[tokio::test]
async fn unknown_path_returns_404() {
    let h = start(ready_all_up()).await;
    let resp = reqwest::get(url(&h, "/does-not-exist")).await.unwrap();
    assert_eq!(resp.status(), 404);
    h.shutdown().await;
}
