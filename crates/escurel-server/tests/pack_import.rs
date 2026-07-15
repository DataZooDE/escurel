//! End-to-end tests for `import_pack` + the subscription pin
//! (REQ-SUB-01/02/03, REQ-LAYER-01/02): the L3→L2 coupler that turns a
//! published skill pack into a pinned, read-only **base layer** at the
//! spoke.
//!
//! The centrepiece is a real two-node federation: a **hub**
//! `EscurelProcess` exports a signed pack over `/mcp`, and a separate
//! **spoke** `EscurelProcess` imports the bytes over `/mcp` — two real
//! gateways, two real DuckDBs, real OIDC on both, no mocks anywhere.
//! The transport is the base64 tarball itself, so the offline
//! (air-gapped) path IS the tested path (INV-AIRGAP): a live pull
//! would only change who moves the bytes.
//!
//! Covers:
//! * AT-SUB-1 — import ⇒ pages land as `layer: base@<pack>@v<version>`,
//!   read-only (`layer_read_only` on update_page), indexed (expand
//!   returns the hub's content), reported by `list_skills`, and the
//!   subscription is pinned.
//! * AT-PACK-2 (end-to-end) — a tampered tarball is rejected
//!   `pack_signature_invalid`; nothing lands.
//! * AT-REBASE-3 — the pinned version never moves silently: importing
//!   v8 over a subscribed v7 is refused (`pack_version_pinned`);
//!   re-importing the same v7 is idempotent.
//! * REQ-SUB-03 — subscribing a pack from an unrelated vertical is
//!   refused (`vertical_mismatch`) unless explicitly overridden.
//! * reserved prefix — `update_page` can never write under
//!   `markdown/base/` (even page ids no import has landed yet), so a
//!   racing import can neither be squatted nor bypassed (the TOCTOU
//!   finding from the layer-model review).
//! * the admin gate — an agent-role token cannot import.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const HUB_TENANT: &str = "hub";
const SPOKE_TENANT: &str = "acme";
const PACK_SECRET: &str = "shared-pack-signing-secret";

const PALLET_SKILL: &str = "---\n\
type: skill\n\
id: pallet-consolidation\n\
description: Consolidate partial pallets (firm-authored).\n\
---\n\
# pallet-consolidation\n\nFirm-authored canonical procedure.\n";

const PALLET_EDGE: &str = "---\n\
type: instance\n\
skill: pallet-consolidation\n\
id: edge-mixed-carrier\n\
---\n\
# Edge case: mixed carrier\n\nTemplate shipped with the pack.\n";

const DENTAL_SKILL: &str = "---\n\
type: skill\n\
id: recall-scheduling\n\
description: Dental recall scheduling.\n\
---\n\
# recall-scheduling\n";

async fn start(tenant: &'static str, fixtures: FixtureBuilder) -> EscurelProcess {
    let _ = tenant;
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

async fn start_hub() -> EscurelProcess {
    start(
        HUB_TENANT,
        FixtureBuilder::new()
            .tenant(HUB_TENANT)
            .skill("pallet-consolidation", PALLET_SKILL)
            .instance("pallet-consolidation", "edge-mixed-carrier", PALLET_EDGE)
            .skill("recall-scheduling", DENTAL_SKILL)
            .done(),
    )
    .await
}

async fn start_spoke() -> EscurelProcess {
    start(
        SPOKE_TENANT,
        FixtureBuilder::new().tenant(SPOKE_TENANT).done(),
    )
    .await
}

async fn call(p: &EscurelProcess, tenant: &str, role: Role, name: &str, args: Value) -> Value {
    let token = p.mint_token(tenant, role);
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

/// Export `skills` from the hub as `pack <id>@v<version>`; returns
/// `(manifest, tarball_b64)`.
async fn export(
    hub: &EscurelProcess,
    id: &str,
    version: u32,
    vertical: &str,
    skills: &[&str],
) -> (Value, String) {
    let body = call(
        hub,
        HUB_TENANT,
        Role::Admin,
        "export_pack",
        json!({
            "tenant_id": HUB_TENANT,
            "id": id,
            "version": version,
            "vertical": vertical,
            "publisher": "hub.test",
            "skills": skills,
            "include_instances": true,
        }),
    )
    .await;
    assert!(body.get("error").is_none(), "hub export failed: {body}");
    let r = &body["result"]["structuredContent"];
    (
        r["manifest"].clone(),
        r["tarball_b64"].as_str().unwrap().to_owned(),
    )
}

async fn import(spoke: &EscurelProcess, manifest: &Value, tarball_b64: &str) -> Value {
    call(
        spoke,
        SPOKE_TENANT,
        Role::Admin,
        "import_pack",
        json!({
            "tenant_id": SPOKE_TENANT,
            "manifest": manifest,
            "tarball_b64": tarball_b64,
        }),
    )
    .await
}

#[tokio::test]
async fn hub_export_to_spoke_import_lands_pinned_read_only_base_layer() {
    // AT-SUB-1 over two real gateways.
    let hub = start_hub().await;
    let spoke = start_spoke().await;
    let (manifest, tarball) = export(
        &hub,
        "logistics-midmarket",
        7,
        "logistics-midmarket",
        &["pallet-consolidation"],
    )
    .await;

    let body = import(&spoke, &manifest, &tarball).await;
    assert!(body.get("error").is_none(), "import failed: {body}");
    let r = &body["result"]["structuredContent"];
    assert_eq!(r["pack"], "logistics-midmarket");
    assert_eq!(r["version"], 7);
    assert_eq!(r["pages_imported"], 2);

    // The skill arrived, layer-pinned, and is visible to agents.
    let skills = call(&spoke, SPOKE_TENANT, Role::Agent, "list_skills", json!({})).await;
    let skills = skills["result"]["structuredContent"]["skills"]
        .as_array()
        .unwrap()
        .clone();
    let base = skills
        .iter()
        .find(|s| s["id"] == "pallet-consolidation")
        .expect("imported skill listed");
    assert_eq!(base["layer"], "base@logistics-midmarket@v7");

    // Content fidelity: the spoke serves the hub's body.
    let page_id = "markdown/base/logistics-midmarket/skills/pallet-consolidation.md";
    let ex = call(
        &spoke,
        SPOKE_TENANT,
        Role::Agent,
        "expand",
        json!({ "page_id": page_id }),
    )
    .await;
    let sc = &ex["result"]["structuredContent"];
    assert!(
        sc["body"]
            .as_str()
            .unwrap_or_default()
            .contains("Firm-authored canonical procedure"),
        "{ex}"
    );
    assert_eq!(sc["frontmatter"]["layer"], "base@logistics-midmarket@v7");

    // Read-only at the spoke: the agent write surface refuses.
    let w = call(
        &spoke,
        SPOKE_TENANT,
        Role::Agent,
        "update_page",
        json!({ "page_id": page_id, "content": PALLET_SKILL }),
    )
    .await;
    let r = &w["result"]["structuredContent"];
    assert_eq!(r["ok"], false, "base page must be read-only: {w}");
    assert_eq!(r["issues"][0]["code"], "layer_read_only");

    hub.shutdown().await;
    spoke.shutdown().await;
}

#[tokio::test]
async fn tampered_pack_is_rejected_and_nothing_lands() {
    // AT-PACK-2, end-to-end over `/mcp`.
    let hub = start_hub().await;
    let spoke = start_spoke().await;
    let (manifest, tarball) = export(
        &hub,
        "logistics-midmarket",
        7,
        "logistics-midmarket",
        &["pallet-consolidation"],
    )
    .await;

    // Flip one byte in the middle of the decoded tarball.
    let mut bytes = B64.decode(tarball.as_bytes()).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0x01;
    let body = import(&spoke, &manifest, &B64.encode(&bytes)).await;
    let msg = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        msg.contains("pack_signature_invalid"),
        "tampered pack must fail closed: {body}"
    );

    // Nothing landed.
    let skills = call(&spoke, SPOKE_TENANT, Role::Agent, "list_skills", json!({})).await;
    assert!(
        !skills["result"]["structuredContent"]["skills"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["id"] == "pallet-consolidation"),
        "{skills}"
    );

    hub.shutdown().await;
    spoke.shutdown().await;
}

#[tokio::test]
async fn pinned_version_never_moves_without_explicit_rebase() {
    // AT-REBASE-3: v8 over a subscribed v7 is refused; same-version
    // re-import is idempotent.
    let hub = start_hub().await;
    let spoke = start_spoke().await;
    let (m7, t7) = export(
        &hub,
        "logistics-midmarket",
        7,
        "logistics-midmarket",
        &["pallet-consolidation"],
    )
    .await;
    let (m8, t8) = export(
        &hub,
        "logistics-midmarket",
        8,
        "logistics-midmarket",
        &["pallet-consolidation"],
    )
    .await;

    let first = import(&spoke, &m7, &t7).await;
    assert!(first.get("error").is_none(), "{first}");

    let upgrade = import(&spoke, &m8, &t8).await;
    let msg = upgrade["error"]["message"].as_str().unwrap_or_default();
    assert!(
        msg.contains("pack_version_pinned"),
        "silent upgrade must be refused: {upgrade}"
    );

    let again = import(&spoke, &m7, &t7).await;
    assert!(
        again.get("error").is_none(),
        "same-version re-import is idempotent: {again}"
    );

    hub.shutdown().await;
    spoke.shutdown().await;
}

#[tokio::test]
async fn unrelated_vertical_is_refused_unless_overridden() {
    // REQ-SUB-03: convergence holds within a vertical; silent mixing
    // resets the ramp.
    let hub = start_hub().await;
    let spoke = start_spoke().await;
    let (ml, tl) = export(
        &hub,
        "logistics-midmarket",
        7,
        "logistics-midmarket",
        &["pallet-consolidation"],
    )
    .await;
    let (md, td) = export(&hub, "dental-basics", 1, "dental", &["recall-scheduling"]).await;

    let first = import(&spoke, &ml, &tl).await;
    assert!(first.get("error").is_none(), "{first}");

    let mixed = import(&spoke, &md, &td).await;
    let msg = mixed["error"]["message"].as_str().unwrap_or_default();
    assert!(
        msg.contains("vertical_mismatch"),
        "cross-vertical subscription must be refused: {mixed}"
    );

    // Explicit operator override is the loud escape hatch.
    let forced = call(
        &spoke,
        SPOKE_TENANT,
        Role::Admin,
        "import_pack",
        json!({
            "tenant_id": SPOKE_TENANT,
            "manifest": md,
            "tarball_b64": td,
            "allow_vertical_mismatch": true,
        }),
    )
    .await;
    assert!(forced.get("error").is_none(), "{forced}");

    hub.shutdown().await;
    spoke.shutdown().await;
}

#[tokio::test]
async fn update_page_can_never_write_under_the_reserved_base_prefix() {
    // The reserved-prefix guard closes the review's TOCTOU/squatting
    // findings statically: `markdown/base/…` belongs to pack import
    // alone, even for page ids no import has landed yet.
    let spoke = start_spoke().await;
    let squat = "---\n\
        type: skill\n\
        id: squatted\n\
        description: agent-authored, pretending to be pack content\n\
        ---\n\
        # squatted\n";
    let w = call(
        &spoke,
        SPOKE_TENANT,
        Role::Agent,
        "update_page",
        json!({
            "page_id": "markdown/base/future-pack/skills/squatted.md",
            "content": squat,
        }),
    )
    .await;
    let r = &w["result"]["structuredContent"];
    assert_eq!(r["ok"], false, "reserved prefix must refuse writes: {w}");
    assert_eq!(r["issues"][0]["code"], "layer_read_only");

    // Admin tokens get no special pass on the agent write surface.
    let w = call(
        &spoke,
        SPOKE_TENANT,
        Role::Admin,
        "update_page",
        json!({
            "page_id": "markdown/base/future-pack/skills/squatted.md",
            "content": squat,
        }),
    )
    .await;
    let r = &w["result"]["structuredContent"];
    assert_eq!(r["ok"], false, "{w}");

    spoke.shutdown().await;
}

#[tokio::test]
async fn same_version_with_different_content_is_refused() {
    // codex review P2: a signed manifest for the same pack@version but a
    // DIFFERENT content_hash must not silently replace the pin —
    // idempotent re-import means same bytes, anything else is a rebase.
    let hub = start_hub().await;
    let spoke = start_spoke().await;
    let (m7, t7) = export(
        &hub,
        "logistics-midmarket",
        7,
        "logistics-midmarket",
        &["pallet-consolidation"],
    )
    .await;
    let first = import(&spoke, &m7, &t7).await;
    assert!(first.get("error").is_none(), "{first}");

    // The hub edits the skill and (wrongly) re-publishes the SAME v7.
    let edited = "---\n\
        type: skill\n\
        id: pallet-consolidation\n\
        description: Edited after publish.\n\
        ---\n\
        # pallet-consolidation\n\nChanged content, same version.\n";
    let w = call(
        &hub,
        HUB_TENANT,
        Role::Admin,
        "update_page",
        json!({ "page_id": "markdown/skills/pallet-consolidation.md", "content": edited }),
    )
    .await;
    assert!(w.get("error").is_none(), "{w}");
    let (m7b, t7b) = export(
        &hub,
        "logistics-midmarket",
        7,
        "logistics-midmarket",
        &["pallet-consolidation"],
    )
    .await;
    assert_ne!(m7b["content_hash"], m7["content_hash"], "content changed");

    let again = import(&spoke, &m7b, &t7b).await;
    let msg = again["error"]["message"].as_str().unwrap_or_default();
    assert!(
        msg.contains("pack_content_mismatch"),
        "changed content under a pinned version must be refused: {again}"
    );

    hub.shutdown().await;
    spoke.shutdown().await;
}

#[tokio::test]
async fn a_pack_with_one_malformed_page_lands_nothing() {
    // codex review P2 / agy MUST-FIX 5: validation completes over the
    // WHOLE pack before the first page is written, so a bad page means
    // zero landed pages, not a half-imported base layer.
    let spoke = start_spoke().await;

    // Hand-build a signed pack whose second entry has no frontmatter.
    let pages = vec![
        (
            "skills/good.md".to_owned(),
            "---\ntype: skill\nid: good\ndescription: ok\n---\n# good\n".to_owned(),
        ),
        (
            "skills/naked.md".to_owned(),
            "no frontmatter here\n".to_owned(),
        ),
    ];
    let tarball = escurel_server::pack::build_tarball(&pages).unwrap();
    let mut manifest = escurel_types::PackManifest {
        format_version: escurel_server::pack::PACK_FORMAT_VERSION,
        id: "broken-pack".into(),
        version: 1,
        vertical: "logistics-midmarket".into(),
        publisher: "hub.test".into(),
        page_count: 2,
        content_hash: escurel_server::pack::content_hash(&tarball),
        signature: String::new(),
    };
    manifest.signature = escurel_server::pack::sign_manifest(&manifest, PACK_SECRET);

    let body = import(
        &spoke,
        &serde_json::to_value(&manifest).unwrap(),
        &B64.encode(&tarball),
    )
    .await;
    let msg = body["error"]["message"].as_str().unwrap_or_default();
    assert!(msg.contains("pack_malformed"), "{body}");

    // NOTHING landed — not even the good page — and no pin exists.
    let skills = call(&spoke, SPOKE_TENANT, Role::Agent, "list_skills", json!({})).await;
    assert!(
        !skills["result"]["structuredContent"]["skills"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["id"] == "good"),
        "half-imported pack: {skills}"
    );
    let packs = call(&spoke, SPOKE_TENANT, Role::Admin, "list_packs", json!({})).await;
    assert!(
        packs["result"]["structuredContent"]["packs"]
            .as_array()
            .unwrap()
            .is_empty(),
        "{packs}"
    );

    spoke.shutdown().await;
}

#[tokio::test]
async fn a_second_pack_shipping_the_same_skill_id_is_refused() {
    // agy MUST-FIX 6: two pages declaring the same skill id would make
    // slug resolution non-deterministic (silent shadowing). Until the
    // explicit shadow-merge feature lands, the collision refuses.
    let hub = start_hub().await;
    let spoke = start_spoke().await;
    let (m1, t1) = export(
        &hub,
        "logistics-midmarket",
        7,
        "logistics-midmarket",
        &["pallet-consolidation"],
    )
    .await;
    // A second pack id, same vertical, shipping the SAME skill.
    let (m2, t2) = export(
        &hub,
        "logistics-extras",
        1,
        "logistics-midmarket",
        &["pallet-consolidation"],
    )
    .await;

    let first = import(&spoke, &m1, &t1).await;
    assert!(first.get("error").is_none(), "{first}");
    let second = import(&spoke, &m2, &t2).await;
    let msg = second["error"]["message"].as_str().unwrap_or_default();
    assert!(
        msg.contains("pack_skill_collision"),
        "duplicate skill id across packs must refuse: {second}"
    );

    hub.shutdown().await;
    spoke.shutdown().await;
}

#[tokio::test]
async fn a_manifest_id_with_unsafe_characters_is_refused() {
    // agy MUST-FIX 3: the layer stamp interpolates the manifest id into
    // frontmatter — an id carrying newlines would smuggle YAML keys.
    // Rejected at export AND import (defence in depth); here: import.
    let spoke = start_spoke().await;
    let pages = vec![(
        "skills/x.md".to_owned(),
        "---\ntype: skill\nid: x\ndescription: ok\n---\n# x\n".to_owned(),
    )];
    let tarball = escurel_server::pack::build_tarball(&pages).unwrap();
    let mut manifest = escurel_types::PackManifest {
        format_version: escurel_server::pack::PACK_FORMAT_VERSION,
        id: "evil\ninjected: true".into(),
        version: 1,
        vertical: "logistics-midmarket".into(),
        publisher: "hub.test".into(),
        page_count: 1,
        content_hash: escurel_server::pack::content_hash(&tarball),
        signature: String::new(),
    };
    manifest.signature = escurel_server::pack::sign_manifest(&manifest, PACK_SECRET);
    let body = import(
        &spoke,
        &serde_json::to_value(&manifest).unwrap(),
        &B64.encode(&tarball),
    )
    .await;
    let msg = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        msg.contains("pack_id_invalid"),
        "unsafe pack id must be refused: {body}"
    );
    spoke.shutdown().await;
}

#[tokio::test]
async fn a_version_zero_candidate_is_not_importable() {
    // codex/agy review: a promotion CANDIDATE (version 0, signed with
    // the same shared secret) must not be importable as a base pack —
    // that would bypass the hub curator's maker/checker gate and squat
    // the pack id against the later approved v1.
    let spoke = start_spoke().await;
    let pages = vec![(
        "skills/x.md".to_owned(),
        "---\ntype: skill\nid: x\ndescription: ok\n---\n# x\n".to_owned(),
    )];
    let tarball = escurel_server::pack::build_tarball(&pages).unwrap();
    let mut manifest = escurel_types::PackManifest {
        format_version: escurel_server::pack::PACK_FORMAT_VERSION,
        id: "candidate-pack".into(),
        version: 0,
        vertical: "logistics-midmarket".into(),
        publisher: "spoke.other".into(),
        page_count: 1,
        content_hash: escurel_server::pack::content_hash(&tarball),
        signature: String::new(),
    };
    manifest.signature = escurel_server::pack::sign_manifest(&manifest, PACK_SECRET);
    let body = import(
        &spoke,
        &serde_json::to_value(&manifest).unwrap(),
        &B64.encode(&tarball),
    )
    .await;
    let msg = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        msg.contains("pack_candidate_not_importable"),
        "version-0 candidates must be refused: {body}"
    );
    spoke.shutdown().await;
}

#[tokio::test]
async fn a_pack_shipping_the_same_skill_id_twice_is_refused() {
    // codex review: the collision check must also see DUPLICATES inside
    // the incoming pack itself — the DB knows nothing about pages that
    // haven't landed yet.
    let spoke = start_spoke().await;
    let pages = vec![
        (
            "skills/a.md".to_owned(),
            "---\ntype: skill\nid: twin\ndescription: first\n---\n# a\n".to_owned(),
        ),
        (
            "skills/b.md".to_owned(),
            "---\ntype: skill\nid: twin\ndescription: second\n---\n# b\n".to_owned(),
        ),
    ];
    let tarball = escurel_server::pack::build_tarball(&pages).unwrap();
    let mut manifest = escurel_types::PackManifest {
        format_version: escurel_server::pack::PACK_FORMAT_VERSION,
        id: "twin-pack".into(),
        version: 1,
        vertical: "logistics-midmarket".into(),
        publisher: "hub.test".into(),
        page_count: 2,
        content_hash: escurel_server::pack::content_hash(&tarball),
        signature: String::new(),
    };
    manifest.signature = escurel_server::pack::sign_manifest(&manifest, PACK_SECRET);
    let body = import(
        &spoke,
        &serde_json::to_value(&manifest).unwrap(),
        &B64.encode(&tarball),
    )
    .await;
    let msg = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        msg.contains("pack_skill_collision"),
        "intra-pack duplicate skill ids must refuse: {body}"
    );
    spoke.shutdown().await;
}

#[tokio::test]
async fn import_pack_requires_admin_role() {
    let hub = start_hub().await;
    let spoke = start_spoke().await;
    let (m, t) = export(
        &hub,
        "logistics-midmarket",
        7,
        "logistics-midmarket",
        &["pallet-consolidation"],
    )
    .await;
    let body = call(
        &spoke,
        SPOKE_TENANT,
        Role::Agent,
        "import_pack",
        json!({ "tenant_id": SPOKE_TENANT, "manifest": m, "tarball_b64": t }),
    )
    .await;
    assert!(
        body.get("error").is_some(),
        "agent-role import must be rejected: {body}"
    );
    hub.shutdown().await;
    spoke.shutdown().await;
}

/// Regression for the export path-construction bug: pages authored
/// through the PUBLIC `update_page` API are stored with LOGICAL page
/// ids (`skill::x`, `<skill>::<id>`), not lane paths. The exporter used
/// to emit those verbatim as tar entries (`requirement::logi-req`),
/// which import then rejected as unsafe paths. The fix reconstructs
/// `skills/<slug>.md` / `instances/<skill>/<slug>.md` from
/// (page_type, skill, slug). This round-trips an update_page-authored
/// skill + instance hub→spoke.
#[tokio::test]
async fn export_of_api_authored_pages_roundtrips_to_spoke() {
    let hub = start(HUB_TENANT, FixtureBuilder::new().tenant(HUB_TENANT).done()).await;
    let spoke = start_spoke().await;

    // Author a skill AND an instance through the public write path.
    let skill = "---\ntype: skill\nid: playbook\ndescription: A reusable playbook.\n\
                 required_frontmatter: [title]\n---\n# playbook\n";
    let inst =
        "---\ntype: instance\nskill: playbook\nid: p1\ntitle: First playbook\n---\n# First\n";
    let w = call(
        &hub,
        HUB_TENANT,
        Role::Admin,
        "update_page",
        json!({ "page_id": "skill::playbook", "content": skill }),
    )
    .await;
    assert_eq!(w["result"]["structuredContent"]["ok"], true, "{w}");
    let w = call(
        &hub,
        HUB_TENANT,
        Role::Admin,
        "update_page",
        json!({ "page_id": "playbook::p1", "content": inst }),
    )
    .await;
    assert_eq!(w["result"]["structuredContent"]["ok"], true, "{w}");

    // Export with instances — the previously-broken path.
    let ex = call(
        &hub,
        HUB_TENANT,
        Role::Admin,
        "export_pack",
        json!({
            "tenant_id": HUB_TENANT, "id": "playbooks", "version": 1,
            "vertical": "ops", "publisher": "hub",
            "skills": ["playbook"], "include_instances": true,
        }),
    )
    .await;
    let sc = &ex["result"]["structuredContent"];
    let manifest = sc["manifest"].clone();
    let tarball_b64 = sc["tarball_b64"].as_str().unwrap().to_owned();

    // Import onto the spoke — must NOT fail with pack_malformed.
    let im = call(
        &spoke,
        SPOKE_TENANT,
        Role::Admin,
        "import_pack",
        json!({
            "tenant_id": SPOKE_TENANT, "manifest": manifest, "tarball_b64": tarball_b64,
            "allow_vertical_mismatch": true,
        }),
    )
    .await;
    let r = &im["result"]["structuredContent"];
    assert_eq!(r["pages_imported"], 2, "skill + instance imported: {im}");

    // The imported instance is indexed and enumerable on the spoke
    // (base pages live under the reserved base/<pack>/ namespace; they
    // surface via list_instances, the path a consumer library uses).
    let li = call(
        &spoke,
        SPOKE_TENANT,
        Role::Agent,
        "list_instances",
        json!({ "skill_id": "playbook" }),
    )
    .await;
    let instances = li["result"]["structuredContent"]["instances"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        instances
            .iter()
            .any(|i| i["frontmatter"]["title"] == "First playbook"),
        "imported instance enumerable on spoke: {li}"
    );

    hub.shutdown().await;
    spoke.shutdown().await;
}
