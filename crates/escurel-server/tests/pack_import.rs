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
