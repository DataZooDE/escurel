//! End-to-end tests for `unsubscribe_pack`: the clean removal of a
//! subscription — the pin row AND every base page the pack landed —
//! so `rebuild` cannot resurrect orphaned base content and a fresh
//! `import_pack` starts from zero.
//!
//! Real gateway, real DuckDB, real OIDC, real `/mcp`. No mocks.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const PACK_SECRET: &str = "shared-pack-signing-secret";

const BASE_SKILL: &str =
    "---\ntype: skill\nid: pallet\ndescription: from the pack.\n---\n# pallet\n";
const OVERLAY: &str = "---\ntype: skill\nid: pallet\ndescription: acme-special.\n---\n# pallet\n";

async fn start(fixtures: FixtureBuilder) -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(fixtures),
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

async fn import(p: &EscurelProcess) {
    let pages = vec![("skills/pallet.md".to_owned(), BASE_SKILL.to_owned())];
    let tarball = escurel_server::pack::build_tarball(&pages).unwrap();
    let mut m = escurel_types::PackManifest {
        format_version: escurel_server::pack::PACK_FORMAT_VERSION,
        id: "logistics".into(),
        version: 1,
        vertical: "logistics".into(),
        publisher: "hub.test".into(),
        page_count: 1,
        content_hash: escurel_server::pack::content_hash(&tarball),
        signature: String::new(),
    };
    m.signature = escurel_server::pack::sign_manifest(&m, PACK_SECRET);
    let r = call(
        p,
        Role::Admin,
        "import_pack",
        json!({ "tenant_id": TENANT, "manifest": m, "tarball_b64": B64.encode(&tarball) }),
    )
    .await;
    assert!(r.get("error").is_none(), "{r}");
}

#[tokio::test]
async fn unsubscribe_removes_pages_and_pin_and_allows_reimport() {
    let p = start(FixtureBuilder::new().tenant(TENANT).done()).await;
    import(&p).await;

    let r = call(
        &p,
        Role::Admin,
        "unsubscribe_pack",
        json!({ "tenant_id": TENANT, "pack_id": "logistics" }),
    )
    .await;
    assert!(r.get("error").is_none(), "{r}");
    let sc = &r["result"]["structuredContent"];
    assert_eq!(sc["pack"], "logistics");
    assert_eq!(sc["pages_removed"], 1, "{r}");

    // The base page is gone and the pin is gone.
    let ex = call(
        &p,
        Role::Agent,
        "expand",
        json!({ "page_id": "markdown/base/logistics/skills/pallet.md" }),
    )
    .await;
    assert!(ex["result"]["structuredContent"]["page"].is_null(), "{ex}");
    let packs = call(&p, Role::Admin, "list_packs", json!({})).await;
    assert!(
        packs["result"]["structuredContent"]["packs"]
            .as_array()
            .unwrap()
            .is_empty(),
        "{packs}"
    );

    // A fresh import starts from zero (no version pin in the way).
    import(&p).await;

    p.shutdown().await;
}

#[tokio::test]
async fn a_shadowing_overlay_survives_and_stops_reporting_the_pin() {
    let p = start(
        FixtureBuilder::new()
            .tenant(TENANT)
            .skill("pallet", OVERLAY)
            .done(),
    )
    .await;
    import(&p).await;

    let r = call(
        &p,
        Role::Admin,
        "unsubscribe_pack",
        json!({ "tenant_id": TENANT, "pack_id": "logistics" }),
    )
    .await;
    assert!(r.get("error").is_none(), "{r}");

    // The tenant's own page is untouched and no longer shadows.
    let skills = call(&p, Role::Agent, "list_skills", json!({})).await;
    let skills = skills["result"]["structuredContent"]["skills"]
        .as_array()
        .unwrap()
        .clone();
    let entry = skills
        .iter()
        .find(|s| s["id"] == "pallet")
        .expect("overlay survives");
    assert_eq!(entry["layer"], "overlay");
    assert!(
        entry.get("shadows").is_none() || entry["shadows"].is_null(),
        "{entry:?}"
    );

    p.shutdown().await;
}

#[tokio::test]
async fn underscore_pack_ids_cannot_delete_a_siblings_pages() {
    // codex review: `_` is a valid pack-id char AND a SQL LIKE wildcard —
    // unsubscribing `foo_bar` must not remove `foo-bar`'s pages.
    let p = start(FixtureBuilder::new().tenant(TENANT).done()).await;
    for (pack, skill_id) in [("foo_bar", "alpha"), ("foo-bar", "beta")] {
        let pages = vec![(
            format!("skills/{skill_id}.md"),
            format!("---\ntype: skill\nid: {skill_id}\ndescription: x\n---\n# {skill_id}\n"),
        )];
        let tarball = escurel_server::pack::build_tarball(&pages).unwrap();
        let mut m = escurel_types::PackManifest {
            format_version: escurel_server::pack::PACK_FORMAT_VERSION,
            id: pack.into(),
            version: 1,
            vertical: "logistics".into(),
            publisher: "hub.test".into(),
            page_count: 1,
            content_hash: escurel_server::pack::content_hash(&tarball),
            signature: String::new(),
        };
        m.signature = escurel_server::pack::sign_manifest(&m, PACK_SECRET);
        let r = call(
            &p,
            Role::Admin,
            "import_pack",
            json!({ "tenant_id": TENANT, "manifest": m, "tarball_b64": B64.encode(&tarball) }),
        )
        .await;
        assert!(r.get("error").is_none(), "{r}");
    }
    let r = call(
        &p,
        Role::Admin,
        "unsubscribe_pack",
        json!({ "tenant_id": TENANT, "pack_id": "foo_bar" }),
    )
    .await;
    assert!(r.get("error").is_none(), "{r}");
    assert_eq!(r["result"]["structuredContent"]["pages_removed"], 1, "{r}");
    // The sibling's page survives.
    let ex = call(
        &p,
        Role::Agent,
        "expand",
        json!({ "page_id": "markdown/base/foo-bar/skills/beta.md" }),
    )
    .await;
    assert!(
        !ex["result"]["structuredContent"]["page"].is_null(),
        "sibling pack's page must survive: {ex}"
    );
    p.shutdown().await;
}

#[tokio::test]
async fn unsubscribe_refuses_unknown_packs_and_agents() {
    let p = start(FixtureBuilder::new().tenant(TENANT).done()).await;
    let r = call(
        &p,
        Role::Admin,
        "unsubscribe_pack",
        json!({ "tenant_id": TENANT, "pack_id": "ghost" }),
    )
    .await;
    assert!(
        r["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("pack_not_subscribed"),
        "{r}"
    );
    import(&p).await;
    let r = call(
        &p,
        Role::Agent,
        "unsubscribe_pack",
        json!({ "tenant_id": TENANT, "pack_id": "logistics" }),
    )
    .await;
    assert!(r.get("error").is_some(), "agent must be refused: {r}");
    p.shutdown().await;
}
