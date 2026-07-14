//! End-to-end tests for overlay-shadows-base (AT-LAYER-2 /
//! REQ-LAYER-03): a tenant specialises an imported base skill by
//! authoring an overlay page with the same skill id — without forking
//! the base and without losing upstream upgradability.
//!
//! Semantics (page-level precedence + drift visibility):
//! * `resolve` prefers the overlay page when both an overlay and a
//!   base page declare the same slug;
//! * `list_skills` reports ONE entry per skill id — the overlay — with
//!   an additive `shadows: base@<pack>@<version>` pin;
//! * `expand` of the shadowing overlay exposes the shadowed base
//!   frontmatter under `shadow.base` so drift is visible, never
//!   silently masked;
//! * INV-SHADOW — the base page stays pristine (expanding it directly
//!   returns the pack's content; the shadow never mutates it);
//! * `import_pack` lands a base skill under an EXISTING tenant skill
//!   of the same id as a shadow (no more `pack_skill_collision` for
//!   the overlay direction; pack-vs-pack stays refused).
//!
//! Real gateway, real DuckDB, real OIDC, real `/mcp`. Packs are built
//! with the real bundler + HMAC signer. No mocks.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const PACK_SECRET: &str = "shared-pack-signing-secret";

const BASE_SKILL_IN_PACK: &str = "---\n\
type: skill\n\
id: pallet-consolidation\n\
description: Firm-authored canonical procedure (v1).\n\
severity_threshold: 10\n\
---\n\
# pallet-consolidation\n\nFirm-authored body.\n";

const OVERLAY_SKILL: &str = "---\n\
type: skill\n\
id: pallet-consolidation\n\
description: Acme-specialised procedure.\n\
---\n\
# pallet-consolidation\n\nTenant-specialised body.\n";

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
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": name, "arguments": args },
        }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json")
}

/// Build + sign a one-skill pack and import it into `p`.
async fn import_base_pack(p: &EscurelProcess) -> Value {
    let pages = vec![(
        "skills/pallet-consolidation.md".to_owned(),
        BASE_SKILL_IN_PACK.to_owned(),
    )];
    let tarball = escurel_server::pack::build_tarball(&pages).unwrap();
    let mut manifest = escurel_types::PackManifest {
        format_version: escurel_server::pack::PACK_FORMAT_VERSION,
        id: "logistics-midmarket".into(),
        version: 1,
        vertical: "logistics-midmarket".into(),
        publisher: "hub.test".into(),
        page_count: 1,
        content_hash: escurel_server::pack::content_hash(&tarball),
        signature: String::new(),
    };
    manifest.signature = escurel_server::pack::sign_manifest(&manifest, PACK_SECRET);
    call(
        p,
        Role::Admin,
        "import_pack",
        json!({
            "tenant_id": TENANT,
            "manifest": manifest,
            "tarball_b64": B64.encode(&tarball),
        }),
    )
    .await
}

const BASE_PAGE_ID: &str = "markdown/base/logistics-midmarket/skills/pallet-consolidation.md";
const OVERLAY_PAGE_ID: &str = "markdown/skills/pallet-consolidation.md";

#[tokio::test]
async fn overlay_shadows_base_for_resolve_and_list_skills() {
    // Base first (import), overlay second (tenant authors the shadow).
    let p = start(FixtureBuilder::new().tenant(TENANT).done()).await;
    let imp = import_base_pack(&p).await;
    assert!(imp.get("error").is_none(), "{imp}");

    let w = call(
        &p,
        Role::Admin,
        "update_page",
        json!({ "page_id": OVERLAY_PAGE_ID, "content": OVERLAY_SKILL }),
    )
    .await;
    assert_eq!(w["result"]["structuredContent"]["ok"], true, "{w}");

    // resolve: the overlay wins for display.
    let r = call(
        &p,
        Role::Agent,
        "resolve",
        json!({ "wikilink": "[[skill::pallet-consolidation]]" }),
    )
    .await;
    assert_eq!(
        r["result"]["structuredContent"]["page"]["page_id"], OVERLAY_PAGE_ID,
        "overlay must win resolution: {r}"
    );

    // list_skills: ONE entry for the id — the overlay — with the pin.
    let skills = call(&p, Role::Agent, "list_skills", json!({})).await;
    let skills = skills["result"]["structuredContent"]["skills"]
        .as_array()
        .unwrap()
        .clone();
    let entries: Vec<&Value> = skills
        .iter()
        .filter(|s| s["id"] == "pallet-consolidation")
        .collect();
    assert_eq!(entries.len(), 1, "one entry per skill id: {skills:?}");
    assert_eq!(entries[0]["layer"], "overlay");
    assert_eq!(
        entries[0]["shadows"], "base@logistics-midmarket@v1",
        "{entries:?}"
    );

    p.shutdown().await;
}

#[tokio::test]
async fn expand_of_the_shadowing_overlay_exposes_base_fields() {
    let p = start(FixtureBuilder::new().tenant(TENANT).done()).await;
    let imp = import_base_pack(&p).await;
    assert!(imp.get("error").is_none(), "{imp}");
    let w = call(
        &p,
        Role::Admin,
        "update_page",
        json!({ "page_id": OVERLAY_PAGE_ID, "content": OVERLAY_SKILL }),
    )
    .await;
    assert_eq!(w["result"]["structuredContent"]["ok"], true, "{w}");

    // Drift visibility: the overlay's expand carries the shadowed base.
    let ex = call(
        &p,
        Role::Agent,
        "expand",
        json!({ "page_id": OVERLAY_PAGE_ID }),
    )
    .await;
    let sc = &ex["result"]["structuredContent"];
    assert_eq!(
        sc["frontmatter"]["description"],
        "Acme-specialised procedure."
    );
    let shadow = &sc["shadow"];
    assert_eq!(shadow["base_page_id"], BASE_PAGE_ID, "{ex}");
    assert_eq!(
        shadow["base"]["description"], "Firm-authored canonical procedure (v1).",
        "base value visible, not silently masked: {ex}"
    );
    assert_eq!(
        shadow["base"]["severity_threshold"], 10,
        "fields the overlay does NOT override are visible too: {ex}"
    );

    // INV-SHADOW: the base page itself stays pristine.
    let base = call(
        &p,
        Role::Agent,
        "expand",
        json!({ "page_id": BASE_PAGE_ID }),
    )
    .await;
    let fm = &base["result"]["structuredContent"]["frontmatter"];
    assert_eq!(fm["description"], "Firm-authored canonical procedure (v1).");
    assert_eq!(fm["layer"], "base@logistics-midmarket@v1");

    // A non-shadowing page carries no shadow object (additive field).
    let plain = call(
        &p,
        Role::Admin,
        "update_page",
        json!({ "page_id": "markdown/skills/local-notes.md",
                "content": "---\ntype: skill\nid: local-notes\ndescription: x\n---\n# local-notes\n" }),
    )
    .await;
    assert_eq!(plain["result"]["structuredContent"]["ok"], true, "{plain}");
    let ex = call(
        &p,
        Role::Agent,
        "expand",
        json!({ "page_id": "markdown/skills/local-notes.md" }),
    )
    .await;
    assert!(
        ex["result"]["structuredContent"].get("shadow").is_none()
            || ex["result"]["structuredContent"]["shadow"].is_null(),
        "{ex}"
    );

    p.shutdown().await;
}

#[tokio::test]
async fn a_shadow_does_not_hide_a_pack_vs_pack_collision() {
    // codex P1: with an overlay AND pack A both declaring the id, a
    // LIMIT-1 conflict lookup could return the overlay and let pack B
    // slip a SECOND base page in — two base pages, no precedence.
    let p = start(
        FixtureBuilder::new()
            .tenant(TENANT)
            .skill("pallet-consolidation", OVERLAY_SKILL)
            .done(),
    )
    .await;
    let imp = import_base_pack(&p).await;
    assert!(imp.get("error").is_none(), "{imp}");

    // Pack B (different id, same vertical) ships the same skill id.
    let pages = vec![(
        "skills/pallet-consolidation.md".to_owned(),
        BASE_SKILL_IN_PACK.to_owned(),
    )];
    let tarball = escurel_server::pack::build_tarball(&pages).unwrap();
    let mut manifest = escurel_types::PackManifest {
        format_version: escurel_server::pack::PACK_FORMAT_VERSION,
        id: "logistics-extras".into(),
        version: 1,
        vertical: "logistics-midmarket".into(),
        publisher: "hub.test".into(),
        page_count: 1,
        content_hash: escurel_server::pack::content_hash(&tarball),
        signature: String::new(),
    };
    manifest.signature = escurel_server::pack::sign_manifest(&manifest, PACK_SECRET);
    let second = call(
        &p,
        Role::Admin,
        "import_pack",
        json!({
            "tenant_id": TENANT,
            "manifest": manifest,
            "tarball_b64": B64.encode(&tarball),
        }),
    )
    .await;
    let msg = second["error"]["message"].as_str().unwrap_or_default();
    assert!(
        msg.contains("pack_skill_collision"),
        "the shadow must not hide the base-vs-base collision: {second}"
    );
    p.shutdown().await;
}

#[tokio::test]
async fn a_promotable_shadowing_overlay_still_promotes() {
    // codex P2: the promotion resolver must prefer the tenant overlay
    // over the shadowed base page for the same id.
    let p = start(FixtureBuilder::new().tenant(TENANT).done()).await;
    let imp = import_base_pack(&p).await;
    assert!(imp.get("error").is_none(), "{imp}");
    let promotable_overlay = "---\ntype: skill\nid: pallet-consolidation\n\
        description: Acme-specialised, curated for harvest.\npromotable: true\n---\n\
        # pallet-consolidation\n\nTenant-specialised body.\n";
    let w = call(
        &p,
        Role::Admin,
        "update_page",
        json!({ "page_id": OVERLAY_PAGE_ID, "content": promotable_overlay }),
    )
    .await;
    assert_eq!(w["result"]["structuredContent"]["ok"], true, "{w}");

    let r = call(
        &p,
        Role::Admin,
        "submit_promotion",
        json!({
            "tenant_id": TENANT,
            "candidate_id": "logistics-harvest",
            "vertical": "logistics-midmarket",
            "skills": ["pallet-consolidation"],
        }),
    )
    .await;
    assert!(
        r.get("error").is_none(),
        "the OVERLAY is promotable; the shadowed base must not block it: {r}"
    );
    p.shutdown().await;
}

#[tokio::test]
async fn agents_cannot_author_a_shadow_over_a_base_skill() {
    // agy MUST-FIX: shadowing changes which skill DEFINITION governs the
    // id's instances (backend binding, ACL, required_frontmatter) — an
    // unprivileged agent authoring `markdown/skills/<base-id>.md` would
    // hijack pack governance. Shadow creation is curator (admin) work.
    let p = start(FixtureBuilder::new().tenant(TENANT).done()).await;
    let imp = import_base_pack(&p).await;
    assert!(imp.get("error").is_none(), "{imp}");

    let w = call(
        &p,
        Role::Agent,
        "update_page",
        json!({ "page_id": OVERLAY_PAGE_ID, "content": OVERLAY_SKILL }),
    )
    .await;
    let r = &w["result"]["structuredContent"];
    assert_eq!(r["ok"], false, "agent-authored shadow must refuse: {w}");
    assert_eq!(r["issues"][0]["code"], "shadow_requires_curator");

    // The curator (admin) may author it — covered by the other tests,
    // re-asserted here for the contrast.
    let w = call(
        &p,
        Role::Admin,
        "update_page",
        json!({ "page_id": OVERLAY_PAGE_ID, "content": OVERLAY_SKILL }),
    )
    .await;
    assert_eq!(w["result"]["structuredContent"]["ok"], true, "{w}");
    p.shutdown().await;
}

#[tokio::test]
async fn import_lands_as_shadow_under_an_existing_tenant_skill() {
    // Overlay first (the tenant authored the skill before subscribing),
    // pack second: the import must land as the shadowed base instead of
    // refusing with pack_skill_collision (that refusal now applies only
    // to pack-vs-pack, which stays covered by the import suite).
    let p = start(
        FixtureBuilder::new()
            .tenant(TENANT)
            .skill("pallet-consolidation", OVERLAY_SKILL)
            .done(),
    )
    .await;
    let imp = import_base_pack(&p).await;
    assert!(
        imp.get("error").is_none(),
        "overlay-direction collision must land as a shadow: {imp}"
    );

    // Overlay still wins; the pin is visible.
    let r = call(
        &p,
        Role::Agent,
        "resolve",
        json!({ "wikilink": "[[skill::pallet-consolidation]]" }),
    )
    .await;
    assert_eq!(
        r["result"]["structuredContent"]["page"]["page_id"], OVERLAY_PAGE_ID,
        "{r}"
    );
    let skills = call(&p, Role::Agent, "list_skills", json!({})).await;
    let skills = skills["result"]["structuredContent"]["skills"]
        .as_array()
        .unwrap()
        .clone();
    let entries: Vec<&Value> = skills
        .iter()
        .filter(|s| s["id"] == "pallet-consolidation")
        .collect();
    assert_eq!(entries.len(), 1, "{skills:?}");
    assert_eq!(entries[0]["shadows"], "base@logistics-midmarket@v1");

    p.shutdown().await;
}
