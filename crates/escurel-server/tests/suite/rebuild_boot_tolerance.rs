//! A tenant whose lane holds editor-saved pages (UTF-8 BOM, CRLF) boots with a rebuilt index
//! (`ESCUREL_REBUILD_INDEX_ON_BOOT=always`, a fresh volume, node loss) and serves them. A page that
//! genuinely cannot be parsed (broken YAML) is SKIPPED AND REPORTED at boot (`/readyz` notice
//! `pages_skipped`, the page named in the components) instead of aborting the boot: one bad file
//! must not take a node offline. The explicit admin `rebuild` keeps refusing (it names every
//! offender and leaves the index untouched).
//!
//! Round-2 review: a CRLF or BOM page made the real server refuse to boot over a rebuilt index.

use std::collections::HashMap;

use escurel_server::EscurelConfig;
use serde_json::{Value, json};
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

async fn tool(base: &str, name: &str, args: Value) -> Value {
    let v: Value = reqwest::Client::new()
        .post(format!("{base}/mcp"))
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                       "params":{"name":name,"arguments":args}}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    v["result"]["structuredContent"].clone()
}

#[tokio::test]
async fn bom_and_crlf_pages_boot_and_a_broken_page_is_skipped_and_reported() {
    let dir = TempDir::new().unwrap();
    let lane = "tenants/default/markdown";
    write(
        dir.path(),
        &format!("{lane}/skills/s0.md"),
        "---\nkind: skill\nid: s0\ndescription: d\n---\n# s0\n",
    );
    write(
        dir.path(),
        &format!("{lane}/instances/s0/bom.md"),
        "\u{feff}---\nkind: instance\nskill: s0\nid: bom\n---\n# Bom page\n",
    );
    write(
        dir.path(),
        &format!("{lane}/instances/s0/crlf.md"),
        "---\r\nkind: instance\r\nskill: s0\r\nid: crlf\r\n---\r\n# Crlf page\r\n",
    );
    write(
        dir.path(),
        &format!("{lane}/instances/s0/broken.md"),
        "---\nkind: instance\nskill: [unclosed\nid: broken\n---\n# Broken\n",
    );
    let pairs = [
        ("ESCUREL_SERVER_DATA_DIR", dir.path().to_str().unwrap()),
        ("ESCUREL_SERVER_LISTEN_HTTP", "127.0.0.1:0"),
        ("ESCUREL_OBSERVABILITY_METRICS_LISTEN", "127.0.0.1:0"),
        ("ESCUREL_EMBEDDING_PROVIDER", "zero"),
        ("ESCUREL_REBUILD_INDEX_ON_BOOT", "always"),
    ];
    let cfg = EscurelConfig::from_source(&source(&pairs)).unwrap();
    let server = cfg
        .build()
        .await
        .expect("a rebuild over BOM/CRLF/broken pages must boot");
    let base = format!("http://{}", server.handle.local_addr);

    let list = tool(&base, "list_instances", json!({"skill": "s0"})).await;
    let ids = list.to_string();
    assert!(ids.contains("bom"), "BOM page served: {list}");
    assert!(ids.contains("crlf"), "CRLF page served: {list}");
    assert!(
        !ids.contains("broken"),
        "the broken page is not indexed: {list}"
    );

    let ready: Value = reqwest::get(format!("{base}/readyz"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        ready["notices"].to_string().contains("pages_skipped"),
        "skipped pages are surfaced: {ready}"
    );
    assert!(
        ready["components"]["skipped_pages"]
            .to_string()
            .contains("broken.md"),
        "the skipped page is named: {ready}"
    );
}

#[tokio::test]
async fn migrate_apply_rewrites_legacy_bom_and_crlf_pages() {
    let dir = TempDir::new().unwrap();
    let lane = "tenants/default/markdown";
    write(
        dir.path(),
        &format!("{lane}/skills/s0.md"),
        "\u{feff}---\r\ntype: skill\r\nid: s0\r\ndescription: d\r\n---\r\n# s0\r\n",
    );
    write(
        dir.path(),
        &format!("{lane}/instances/s0/a.md"),
        "---\r\ntype: instance\r\nskill: s0\r\nid: a\r\n---\r\n# A\r\n",
    );
    let pairs = [
        ("ESCUREL_SERVER_DATA_DIR", dir.path().to_str().unwrap()),
        ("ESCUREL_SERVER_LISTEN_HTTP", "127.0.0.1:0"),
        ("ESCUREL_OBSERVABILITY_METRICS_LISTEN", "127.0.0.1:0"),
        ("ESCUREL_EMBEDDING_PROVIDER", "zero"),
    ];
    let server = EscurelConfig::from_source(&source(&pairs))
        .unwrap()
        .build()
        .await
        .expect("boots quarantined");
    let base = format!("http://{}", server.handle.local_addr);
    let report = tool(&base, "migrate_kind", json!({"apply": true})).await;
    assert_eq!(report["tenant_quarantined"], false, "{report}");
    let page =
        std::fs::read_to_string(dir.path().join(format!("{lane}/instances/s0/a.md"))).unwrap();
    assert!(
        page.starts_with("---\r\nkind: instance\r\n"),
        "EOLs kept: {page:?}"
    );
    let skill = std::fs::read_to_string(dir.path().join(format!("{lane}/skills/s0.md"))).unwrap();
    assert!(
        skill.starts_with("\u{feff}---\r\nkind: skill\r\n"),
        "BOM kept: {skill:?}"
    );
}

async fn notices(listen: &str, dir: &std::path::Path) -> String {
    let pairs = [
        ("ESCUREL_SERVER_DATA_DIR", dir.to_str().unwrap()),
        ("ESCUREL_SERVER_LISTEN_HTTP", listen),
        ("ESCUREL_OBSERVABILITY_METRICS_LISTEN", "127.0.0.1:0"),
        ("ESCUREL_EMBEDDING_PROVIDER", "zero"),
    ];
    let server = EscurelConfig::from_source(&source(&pairs))
        .unwrap()
        .build()
        .await
        .expect("boots");
    let ready: Value = reqwest::get(format!("http://{}/readyz", server.handle.local_addr))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    ready["notices"].to_string()
}

// Round-2 review: the default is unauthenticated and silent. A non-loopback listener without an
// OIDC issuer says so on /readyz (and loudly in the boot log); loopback does not nag.
#[tokio::test]
async fn an_unauthenticated_non_loopback_listener_is_flagged_on_readyz() {
    let dir = TempDir::new().unwrap();
    let exposed = notices("0.0.0.0:0", dir.path()).await;
    assert!(exposed.contains("unauthenticated_exposed"), "{exposed}");
    let dir2 = TempDir::new().unwrap();
    let local = notices("127.0.0.1:0", dir2.path()).await;
    assert!(!local.contains("unauthenticated_exposed"), "{local}");
}

// The source-timeout knobs are config, validated at boot like every other number.
#[tokio::test]
async fn source_timeout_knobs_are_validated_at_boot() {
    for (var, bad) in [
        ("ESCUREL_ROWS_QUERY_TIMEOUT_SECS", "soon"),
        ("ESCUREL_SQL_CONNECT_TIMEOUT_SECS", "0"),
    ] {
        let dir = TempDir::new().unwrap();
        let pairs = [
            ("ESCUREL_SERVER_DATA_DIR", dir.path().to_str().unwrap()),
            ("ESCUREL_SERVER_LISTEN_HTTP", "127.0.0.1:0"),
            ("ESCUREL_EMBEDDING_PROVIDER", "zero"),
            (var, bad),
        ];
        let err = EscurelConfig::from_source(&source(&pairs)).err();
        assert!(
            err.is_some_and(|e| e.to_string().contains(var)),
            "{var}={bad} must be refused naming the variable"
        );
    }
    let dir = TempDir::new().unwrap();
    let pairs = [
        ("ESCUREL_SERVER_DATA_DIR", dir.path().to_str().unwrap()),
        ("ESCUREL_SERVER_LISTEN_HTTP", "127.0.0.1:0"),
        ("ESCUREL_EMBEDDING_PROVIDER", "zero"),
        ("ESCUREL_ROWS_QUERY_TIMEOUT_SECS", "45"),
        ("ESCUREL_SQL_CONNECT_TIMEOUT_SECS", "9"),
    ];
    let cfg = EscurelConfig::from_source(&source(&pairs)).unwrap();
    assert_eq!(cfg.rows_query_timeout.as_secs(), 45);
    assert_eq!(cfg.sql_connect_timeout.as_secs(), 9);
}

#[tokio::test]
async fn boot_sweeps_the_orphan_temp_files_a_killed_write_left() {
    let dir = TempDir::new().unwrap();
    let lane = "tenants/default/markdown";
    write(
        dir.path(),
        &format!("{lane}/skills/s0.md"),
        "---\nkind: skill\nid: s0\ndescription: d\n---\n# s0\n",
    );
    write(
        dir.path(),
        &format!("{lane}/instances/s0/half.md.tmp"),
        "half a write",
    );
    let pairs = [
        ("ESCUREL_SERVER_DATA_DIR", dir.path().to_str().unwrap()),
        ("ESCUREL_SERVER_LISTEN_HTTP", "127.0.0.1:0"),
        ("ESCUREL_OBSERVABILITY_METRICS_LISTEN", "127.0.0.1:0"),
        ("ESCUREL_EMBEDDING_PROVIDER", "zero"),
    ];
    let _server = EscurelConfig::from_source(&source(&pairs))
        .unwrap()
        .build()
        .await
        .unwrap();
    assert!(
        !dir.path()
            .join(format!("{lane}/instances/s0/half.md.tmp"))
            .exists(),
        "the orphan temp file was swept at boot"
    );
}
