//! What an OPERATOR can see, through the real server: boot a real `EscurelConfig` over a data
//! directory whose lane holds LEGACY pages (the old `type:` key) and read `/readyz` and `/metrics`
//! over real HTTP. No probe is injected: this is the production `DependencyProbe`.
//!
//! The decision these pin (docs/deploy/kind-migration.md): a quarantined tenant is READY (200, so the
//! one-shot `migrate-kind` can run against the new image) but says so loudly: a JSON body, an
//! `x-escurel-quarantined` header and the `escurel_tenant_quarantined` gauge.

use std::collections::HashMap;

use escurel_server::EscurelConfig;
use tempfile::TempDir;

fn source(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    move |k: &str| map.get(k).cloned()
}

fn write(dir: &std::path::Path, rel: &str, text: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

struct Booted {
    base: String,
    metrics: String,
    _server: escurel_server::config::BootedServer,
    _dir: TempDir,
}

async fn boot(legacy: bool, provider: &str, key: Option<&str>) -> Booted {
    let dir = TempDir::new().unwrap();
    if legacy {
        write(
            dir.path(),
            "tenants/default/markdown/skills/customer.md",
            "---\ntype: skill\nid: customer\ndescription: x\n---\n# customer\n",
        );
    } else {
        write(
            dir.path(),
            "tenants/default/markdown/skills/customer.md",
            "---\nkind: skill\nid: customer\ndescription: x\n---\n# customer\n",
        );
    }
    let mut pairs = vec![
        ("ESCUREL_SERVER_DATA_DIR", dir.path().to_str().unwrap()),
        ("ESCUREL_SERVER_LISTEN_HTTP", "127.0.0.1:0"),
        ("ESCUREL_OBSERVABILITY_METRICS_LISTEN", "127.0.0.1:0"),
        ("ESCUREL_EMBEDDING_PROVIDER", provider),
    ];
    if let Some(k) = key {
        pairs.push(("ESCUREL_GEMINI_API_KEY", k));
    }
    let cfg = EscurelConfig::from_source(&source(&pairs)).unwrap();
    let server = cfg.build().await.expect("boots");
    let base = format!("http://{}", server.handle.local_addr);
    let metrics = format!(
        "http://{}",
        server.handle.metrics_addr.expect("metrics listener")
    );
    Booted {
        base,
        metrics,
        _server: server,
        _dir: dir,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_quarantined_tenant_is_ready_but_says_so_everywhere() {
    let b = boot(true, "zero", None).await;
    let client = reqwest::Client::new();

    // Liveness is dependency-free; readiness stays 200 so the migration job can run against it...
    assert_eq!(
        client
            .get(format!("{}/healthz", b.base))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let ready = client
        .get(format!("{}/readyz", b.base))
        .send()
        .await
        .unwrap();
    assert_eq!(
        ready.status(),
        200,
        "quarantine must not 503 /readyz (the migration needs the server up)"
    );
    // ...but an orchestrator or a human can SEE it.
    assert_eq!(
        ready
            .headers()
            .get("x-escurel-quarantined")
            .map(|v| v.to_str().unwrap()),
        Some("1")
    );
    let body: serde_json::Value = ready.json().await.unwrap();
    assert_eq!(body["ready"], true);
    assert_eq!(body["components"]["quarantined"], true);
    assert!(
        body["notices"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n == "quarantined"),
        "{body}"
    );

    let metrics = client
        .get(format!("{}/metrics", b.metrics))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        metrics.contains(r#"escurel_tenant_quarantined{tenant="default"} 1"#),
        "{metrics}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_clean_tenant_has_no_quarantine_notice_and_the_gauge_reads_zero() {
    let b = boot(false, "zero", None).await;
    let client = reqwest::Client::new();
    let ready = client
        .get(format!("{}/readyz", b.base))
        .send()
        .await
        .unwrap();
    assert_eq!(ready.status(), 200);
    assert!(ready.headers().get("x-escurel-quarantined").is_none());
    let body: serde_json::Value = ready.json().await.unwrap();
    assert_eq!(body["components"]["quarantined"], false, "{body}");
    assert!(
        !body["notices"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n == "quarantined"),
        "{body}"
    );
    let metrics = client
        .get(format!("{}/metrics", b.metrics))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    // A clean tenant reads 0, not absent: an alert on `== 1` must clear after a migration.
    assert!(
        metrics.contains(r#"escurel_tenant_quarantined{tenant="default"} 0"#),
        "{metrics}"
    );
}

/// The keyless-gemini default boots and "works" with zero vectors: that must be VISIBLE, not only a
/// boot log line an operator never reads.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keyless_gemini_reports_semantic_search_disabled() {
    let b = boot(false, "gemini", None).await;
    let client = reqwest::Client::new();
    let ready = client
        .get(format!("{}/readyz", b.base))
        .send()
        .await
        .unwrap();
    assert_eq!(
        ready.status(),
        200,
        "lexical search still works: still ready"
    );
    let body: serde_json::Value = ready.json().await.unwrap();
    assert_eq!(body["components"]["semantic_search"], false, "{body}");
    assert!(
        body["notices"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n == "semantic_search_disabled"),
        "{body}"
    );
    let metrics = client
        .get(format!("{}/metrics", b.metrics))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        metrics.contains("escurel_semantic_search_enabled 0"),
        "{metrics}"
    );
}
