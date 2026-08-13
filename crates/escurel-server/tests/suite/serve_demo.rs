//! End-to-end tests for the optional static-demo serving surface.
//!
//! When `ServerConfig::demo_dir` is set, the gateway serves the built
//! demo bundle (Flutter web) at `/` via a `tower-http` `ServeDir`
//! fallback, while the explicit API routes (`/healthz`, `/mcp`, …)
//! keep precedence and unknown paths fall back to `index.html` (SPA
//! routing). Real server on a random port, real reqwest client, real
//! files on disk. No mocks.

use std::fs;

use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, Opts};
use tempfile::TempDir;

const INDEX_MARKER: &str = "<!--escurel-demo-index-->";

/// Build a temp dir that looks like a Flutter `build/web` bundle.
fn demo_bundle() -> TempDir {
    let dir = TempDir::new().expect("tempdir for demo bundle");
    fs::write(
        dir.path().join("index.html"),
        format!("<!doctype html><title>Escurel Demo</title>{INDEX_MARKER}"),
    )
    .expect("write index.html");
    fs::write(dir.path().join("main.dart.js"), "console.log('demo');").expect("write main.dart.js");
    dir
}

async fn start(demo: &TempDir) -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::Disabled,
        fixtures: None,
        config_overrides: ConfigOverrides {
            demo_dir: Some(demo.path().to_path_buf()),
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
async fn root_serves_demo_index_html() {
    let demo = demo_bundle();
    let p = start(&demo).await;
    let resp = reqwest::get(url(&p, "/")).await.unwrap();
    assert_eq!(resp.status(), 200);
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    assert!(ct.starts_with("text/html"), "content-type: {ct}");
    assert!(resp.text().await.unwrap().contains(INDEX_MARKER));
    p.shutdown().await;
}

#[tokio::test]
async fn static_assets_are_served() {
    let demo = demo_bundle();
    let p = start(&demo).await;
    let resp = reqwest::get(url(&p, "/main.dart.js")).await.unwrap();
    assert_eq!(resp.status(), 200);
    assert!(resp.text().await.unwrap().contains("demo"));
    p.shutdown().await;
}

#[tokio::test]
async fn unknown_path_falls_back_to_index_spa() {
    // SPA routing: a client-side route the server doesn't know must
    // serve index.html so the in-app router can take over.
    let demo = demo_bundle();
    let p = start(&demo).await;
    let resp = reqwest::get(url(&p, "/chat/room-1")).await.unwrap();
    assert_eq!(resp.status(), 200);
    assert!(resp.text().await.unwrap().contains(INDEX_MARKER));
    p.shutdown().await;
}

#[tokio::test]
async fn api_routes_keep_precedence_over_demo() {
    // The static fallback must not shadow the explicit API routes.
    let demo = demo_bundle();
    let p = start(&demo).await;

    let health = reqwest::get(url(&p, "/healthz")).await.unwrap();
    assert_eq!(health.status(), 200);
    assert_eq!(health.text().await.unwrap(), "OK");

    let version = reqwest::get(url(&p, "/version")).await.unwrap();
    assert_eq!(version.status(), 200);

    // Metrics live on their own dedicated listener — unaffected by the
    // demo SPA fallback mounted on the main app.
    let metrics = reqwest::get(p.metrics_url().expect("metrics listener"))
        .await
        .unwrap();
    assert_eq!(metrics.status(), 200);
    assert!(metrics.text().await.unwrap().contains("escurel_up"));
    p.shutdown().await;
}

#[tokio::test]
async fn demo_mode_sets_permissive_cors() {
    // The flutter-drive integration harness calls /mcp from its own
    // web-server origin, so demo mode must relax CORS.
    let demo = demo_bundle();
    let p = start(&demo).await;
    let resp = reqwest::Client::new()
        .get(url(&p, "/healthz"))
        .header("Origin", "http://localhost:12345")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert!(
        resp.headers().contains_key("access-control-allow-origin"),
        "demo mode must emit an Access-Control-Allow-Origin header",
    );
    p.shutdown().await;
}

#[tokio::test]
async fn no_demo_dir_means_unknown_path_is_404() {
    // Default (no demo_dir) keeps the bare-API behaviour: unknown
    // paths are 404, not an SPA fallback.
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::Disabled,
        fixtures: None,
        config_overrides: ConfigOverrides {
            disable_indexer: true,
            ..Default::default()
        },
    })
    .await;
    let resp = reqwest::get(url(&p, "/does-not-exist")).await.unwrap();
    assert_eq!(resp.status(), 404);
    p.shutdown().await;
}
