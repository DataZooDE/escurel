//! End-to-end tests for `rebase_pack` (REQ-REBASE-01/02): the reviewed,
//! explicit upgrade of a subscribed pack — the only operation that
//! moves a version pin. Conflicts (the tenant's shadow overrides a
//! field the new pack version also changed) surface as typed
//! `rebase_conflict` Issues for a human; nothing auto-resolves.
//!
//! Real gateway, real DuckDB, real OIDC, real `/mcp`; packs built with
//! the real bundler + HMAC signer. No mocks.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const PACK_SECRET: &str = "shared-pack-signing-secret";

fn skill(id: &str, description: &str, extra: &str) -> String {
    format!("---\ntype: skill\nid: {id}\ndescription: {description}\n{extra}---\n# {id}\n\nbody\n")
}

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(FixtureBuilder::new().tenant(TENANT).done()),
        config_overrides: ConfigOverrides {
            pack_secret: Some(PACK_SECRET.to_owned()),
            ..Default::default()
        },
    })
    .await
}

async fn call(p: &EscurelProcess, role: Role, name: &str, args: Value) -> Value {
    let token = p.mint_token(TENANT, role);
    reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": args },
        }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json")
}

fn signed(pages: &[(String, String)], version: u32) -> (Value, String) {
    let tarball = escurel_server::pack::build_tarball(pages).unwrap();
    let mut m = escurel_types::PackManifest {
        format_version: escurel_server::pack::PACK_FORMAT_VERSION,
        id: "logistics".into(),
        version,
        vertical: "logistics".into(),
        publisher: "hub.test".into(),
        page_count: pages.len() as u32,
        content_hash: escurel_server::pack::content_hash(&tarball),
        signature: String::new(),
    };
    m.signature = escurel_server::pack::sign_manifest(&m, PACK_SECRET);
    (serde_json::to_value(&m).unwrap(), B64.encode(&tarball))
}

fn v1_pages() -> Vec<(String, String)> {
    vec![
        (
            "skills/alpha.md".to_owned(),
            skill("alpha", "alpha v1.", ""),
        ),
        ("skills/beta.md".to_owned(), skill("beta", "beta v1.", "")),
    ]
}

/// v2: alpha changed, beta removed, gamma added.
fn v2_pages() -> Vec<(String, String)> {
    vec![
        (
            "skills/alpha.md".to_owned(),
            skill("alpha", "alpha v2.", ""),
        ),
        (
            "skills/gamma.md".to_owned(),
            skill("gamma", "gamma v2.", ""),
        ),
    ]
}

async fn import_v1(p: &EscurelProcess) {
    let (m, t) = signed(&v1_pages(), 1);
    let r = call(
        p,
        Role::Admin,
        "import_pack",
        json!({ "tenant_id": TENANT, "manifest": m, "tarball_b64": t }),
    )
    .await;
    assert!(r.get("error").is_none(), "{r}");
}

async fn rebase(p: &EscurelProcess, manifest: &Value, tarball: &str, ack: bool) -> Value {
    call(
        p,
        Role::Admin,
        "rebase_pack",
        json!({
            "tenant_id": TENANT,
            "manifest": manifest,
            "tarball_b64": tarball,
            "acknowledge_conflicts": ack,
        }),
    )
    .await
}

async fn rebase_dry_run(p: &EscurelProcess, manifest: &Value, tarball: &str) -> Value {
    call(
        p,
        Role::Admin,
        "rebase_pack",
        json!({
            "tenant_id": TENANT,
            "manifest": manifest,
            "tarball_b64": tarball,
            "dry_run": true,
        }),
    )
    .await
}

#[tokio::test]
async fn clean_upgrade_applies_and_moves_the_pin() {
    // AT-REBASE-1: no overlay overlap ⇒ the upgrade applies silently.
    let p = start().await;
    import_v1(&p).await;
    let (m2, t2) = signed(&v2_pages(), 2);
    let r = rebase(&p, &m2, &t2, false).await;
    assert!(r.get("error").is_none(), "{r}");
    let sc = &r["result"]["structuredContent"];
    assert_eq!(sc["ok"], true, "{r}");
    assert_eq!(sc["from_version"], 1);
    assert_eq!(sc["to_version"], 2);
    assert_eq!(sc["pages_removed"], 1, "beta is orphaned: {r}");

    // alpha carries v2 content + the new pin.
    let ex = call(
        &p,
        Role::Agent,
        "expand",
        json!({ "page_id": "markdown/base/logistics/skills/alpha.md" }),
    )
    .await;
    let fm = &ex["result"]["structuredContent"]["frontmatter"];
    assert_eq!(fm["description"], "alpha v2.");
    assert_eq!(fm["layer"], "base@logistics@v2");

    // beta is gone; gamma exists.
    let gone = call(
        &p,
        Role::Agent,
        "expand",
        json!({ "page_id": "markdown/base/logistics/skills/beta.md" }),
    )
    .await;
    assert!(
        gone["result"]["structuredContent"]["page"].is_null(),
        "orphaned base page must be removed: {gone}"
    );
    let skills = call(&p, Role::Agent, "list_skills", json!({})).await;
    let ids: Vec<String> = skills["result"]["structuredContent"]["skills"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap().to_owned())
        .collect();
    assert!(ids.contains(&"gamma".to_owned()), "{ids:?}");
    assert!(!ids.contains(&"beta".to_owned()), "{ids:?}");

    // The pin moved.
    let packs = call(&p, Role::Admin, "list_packs", json!({})).await;
    assert_eq!(
        packs["result"]["structuredContent"]["packs"][0]["version"], 2,
        "{packs}"
    );

    p.shutdown().await;
}

#[tokio::test]
async fn conflicting_override_surfaces_issues_and_leaves_the_base_unchanged() {
    // AT-REBASE-2: the tenant's shadow overrides `description`; v2 also
    // changes `description` ⇒ conflict Issue, nothing moves. With the
    // explicit acknowledgement, the upgrade applies (the overlay keeps
    // winning for display; the shadow object shows the new base value).
    let p = start().await;
    import_v1(&p).await;
    // The tenant shadows alpha, overriding description.
    let w = call(
        &p,
        Role::Admin,
        "update_page",
        json!({ "page_id": "markdown/skills/alpha.md",
                "content": skill("alpha", "acme-special.", "") }),
    )
    .await;
    assert_eq!(w["result"]["structuredContent"]["ok"], true, "{w}");

    let (m2, t2) = signed(&v2_pages(), 2);
    let r = rebase(&p, &m2, &t2, false).await;
    let sc = &r["result"]["structuredContent"];
    assert_eq!(sc["ok"], false, "conflict must block: {r}");
    let issues = sc["issues"].as_array().expect("issues");
    assert!(
        issues.iter().any(|i| i["code"] == "rebase_conflict"
            && i["location"].as_str().unwrap_or_default().contains("alpha")
            && i["location"]
                .as_str()
                .unwrap_or_default()
                .contains("description")),
        "{r}"
    );
    // Base unchanged, pin unchanged.
    let ex = call(
        &p,
        Role::Agent,
        "expand",
        json!({ "page_id": "markdown/base/logistics/skills/alpha.md" }),
    )
    .await;
    assert_eq!(
        ex["result"]["structuredContent"]["frontmatter"]["description"],
        "alpha v1."
    );
    let packs = call(&p, Role::Admin, "list_packs", json!({})).await;
    assert_eq!(
        packs["result"]["structuredContent"]["packs"][0]["version"], 1,
        "{packs}"
    );

    // Acknowledged: the upgrade applies; the overlay still wins; the
    // shadow shows the NEW base value (drift stays visible).
    let r = rebase(&p, &m2, &t2, true).await;
    assert_eq!(r["result"]["structuredContent"]["ok"], true, "{r}");
    let ex = call(
        &p,
        Role::Agent,
        "expand",
        json!({ "page_id": "markdown/skills/alpha.md" }),
    )
    .await;
    let sc = &ex["result"]["structuredContent"];
    assert_eq!(sc["frontmatter"]["description"], "acme-special.");
    assert_eq!(sc["shadow"]["base"]["description"], "alpha v2.", "{ex}");

    p.shutdown().await;
}

#[tokio::test]
async fn dry_run_reports_the_plan_without_applying_anything() {
    // A clean upgrade under dry_run: full validation + conflict scan run,
    // the would-import / would-remove counts come back, and NOTHING moves
    // — no page writes, no orphan removal, the pin stays.
    let p = start().await;
    import_v1(&p).await;
    let (m2, t2) = signed(&v2_pages(), 2);
    let r = rebase_dry_run(&p, &m2, &t2).await;
    assert!(r.get("error").is_none(), "{r}");
    let sc = &r["result"]["structuredContent"];
    assert_eq!(sc["ok"], true, "{r}");
    assert_eq!(sc["dry_run"], true, "{r}");
    assert_eq!(sc["issues"].as_array().map(Vec::len), Some(0), "{r}");
    assert_eq!(sc["would_import"], 2, "alpha + gamma: {r}");
    assert_eq!(sc["would_remove"], 1, "beta is orphaned: {r}");
    assert_eq!(sc["from_version"], 1);
    assert_eq!(sc["to_version"], 2);

    // Pages untouched: alpha still carries v1 content + pin, beta alive.
    let ex = call(
        &p,
        Role::Agent,
        "expand",
        json!({ "page_id": "markdown/base/logistics/skills/alpha.md" }),
    )
    .await;
    let fm = &ex["result"]["structuredContent"]["frontmatter"];
    assert_eq!(fm["description"], "alpha v1.", "{ex}");
    assert_eq!(fm["layer"], "base@logistics@v1", "{ex}");
    let beta = call(
        &p,
        Role::Agent,
        "expand",
        json!({ "page_id": "markdown/base/logistics/skills/beta.md" }),
    )
    .await;
    assert!(
        !beta["result"]["structuredContent"]["page"].is_null(),
        "dry-run must not remove orphans: {beta}"
    );

    // The pin did not move.
    let packs = call(&p, Role::Admin, "list_packs", json!({})).await;
    assert_eq!(
        packs["result"]["structuredContent"]["packs"][0]["version"], 1,
        "{packs}"
    );

    p.shutdown().await;
}

#[tokio::test]
async fn dry_run_with_conflicts_lists_the_same_issues_and_applies_nothing() {
    // The shadow-vs-upstream conflict scan runs under dry_run exactly as
    // in a real rebase; `ok:false` + the rebase_conflict Issues come
    // back, and the base/pin stay untouched.
    let p = start().await;
    import_v1(&p).await;
    let w = call(
        &p,
        Role::Admin,
        "update_page",
        json!({ "page_id": "markdown/skills/alpha.md",
                "content": skill("alpha", "acme-special.", "") }),
    )
    .await;
    assert_eq!(w["result"]["structuredContent"]["ok"], true, "{w}");

    let (m2, t2) = signed(&v2_pages(), 2);
    let r = rebase_dry_run(&p, &m2, &t2).await;
    let sc = &r["result"]["structuredContent"];
    assert_eq!(sc["ok"], false, "conflicts ⇒ not clean: {r}");
    assert_eq!(sc["dry_run"], true, "{r}");
    let issues = sc["issues"].as_array().expect("issues");
    assert!(
        issues.iter().any(|i| i["code"] == "rebase_conflict"
            && i["location"].as_str().unwrap_or_default().contains("alpha")
            && i["location"]
                .as_str()
                .unwrap_or_default()
                .contains("description")),
        "{r}"
    );

    // Nothing applied.
    let ex = call(
        &p,
        Role::Agent,
        "expand",
        json!({ "page_id": "markdown/base/logistics/skills/alpha.md" }),
    )
    .await;
    assert_eq!(
        ex["result"]["structuredContent"]["frontmatter"]["description"],
        "alpha v1."
    );
    let packs = call(&p, Role::Admin, "list_packs", json!({})).await;
    assert_eq!(
        packs["result"]["structuredContent"]["packs"][0]["version"], 1,
        "{packs}"
    );

    p.shutdown().await;
}

#[tokio::test]
async fn rebase_refuses_unsubscribed_packs_and_non_upgrades() {
    let p = start().await;
    import_v1(&p).await;

    // Not subscribed.
    let (m, t) = {
        let pages = vec![("skills/x.md".to_owned(), skill("x", "x.", ""))];
        let tarball = escurel_server::pack::build_tarball(&pages).unwrap();
        let mut m = escurel_types::PackManifest {
            format_version: escurel_server::pack::PACK_FORMAT_VERSION,
            id: "other-pack".into(),
            version: 2,
            vertical: "logistics".into(),
            publisher: "hub.test".into(),
            page_count: 1,
            content_hash: escurel_server::pack::content_hash(&tarball),
            signature: String::new(),
        };
        m.signature = escurel_server::pack::sign_manifest(&m, PACK_SECRET);
        (serde_json::to_value(&m).unwrap(), B64.encode(&tarball))
    };
    let r = rebase(&p, &m, &t, false).await;
    assert!(
        r["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("pack_not_subscribed"),
        "{r}"
    );

    // Same version ⇒ not an upgrade (idempotent refresh is import's job).
    let (m1, t1) = signed(&v1_pages(), 1);
    let r = rebase(&p, &m1, &t1, false).await;
    assert!(
        r["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("pack_rebase_not_an_upgrade"),
        "{r}"
    );

    p.shutdown().await;
}

#[tokio::test]
async fn rebase_requires_admin_role() {
    let p = start().await;
    import_v1(&p).await;
    let (m2, t2) = signed(&v2_pages(), 2);
    let body = call(
        &p,
        Role::Agent,
        "rebase_pack",
        json!({ "tenant_id": TENANT, "manifest": m2, "tarball_b64": t2 }),
    )
    .await;
    assert!(body.get("error").is_some(), "{body}");
    p.shutdown().await;
}
