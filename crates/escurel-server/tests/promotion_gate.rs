//! End-to-end tests for `submit_promotion` — the L2→L3 harvest coupler
//! and THE security-critical seam of federation (REQ-PROMO-01..04):
//! the one boundary where a mistake leaks customer-confidential data
//! into a substrate other customers consume.
//!
//! Real gateway, real DuckDB, real OIDC, real `/mcp`; the candidate
//! bundle is decoded with real flate2/tar. No mocks.
//!
//! Covers:
//! * AT-PROMO-1 — the **zero-leakage regression battery** (modelled on
//!   the INV-ACL-FUSION fusion-ACL test): under every input shape,
//!   nothing non-eligible ever crosses — instance pages (even ones
//!   maliciously tagged `promotable: true`), skill pages without the
//!   curator marker, base-layer pages, credential-shaped content — and
//!   the emitted bundle contains EXACTLY the eligible pages.
//! * AT-PROMO-2 — an agent cannot set `promotable: true`; only an
//!   admin ("curator" in the v1 two-role model) writes it.
//! * AT-PROMO-3 — every submission emits an immutable audit event
//!   ("what left this spoke, when, by whom").
//! * the admin gate — an agent-role token cannot submit.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const PACK_SECRET: &str = "shared-pack-signing-secret";

/// A curated, promotable, firm-authored skill — the one thing that MAY
/// leave the spoke.
const PROMOTABLE_SKILL: &str = "---\n\
type: skill\n\
id: pallet-consolidation\n\
description: Consolidate partial pallets (firm-authored, reusable).\n\
promotable: true\n\
---\n\
# pallet-consolidation\n\nGeneric, customer-free procedure.\n";

/// Tenant-authored skill WITHOUT the curator marker — default-deny.
const UNMARKED_SKILL: &str = "---\n\
type: skill\n\
id: local-notes\n\
description: Tenant-local notes skill.\n\
---\n\
# local-notes\n";

/// Customer data. Never promotes — not even when someone tags the
/// instance itself `promotable: true` (eligibility is skills-only).
const CUSTOMER_INSTANCE: &str = "---\n\
type: instance\n\
skill: pallet-consolidation\n\
id: acme-shipment-4711\n\
promotable: true\n\
customer: ACME GmbH\n\
---\n\
# ACME shipment 4711\n\nConfidential: contract volume 1.2M EUR.\n";

/// A promotable skill that trips the deterministic scrubber.
const LEAKY_PROMOTABLE_SKILL: &str = "---\n\
type: skill\n\
id: leaky-skill\n\
description: promotable but carries a credential.\n\
promotable: true\n\
---\n\
# leaky-skill\n\nConnect via postgres://svc:hunter2@db.internal/prod\n";

fn fixtures() -> FixtureBuilder {
    FixtureBuilder::new()
        .tenant(TENANT)
        .skill("pallet-consolidation", PROMOTABLE_SKILL)
        .skill("local-notes", UNMARKED_SKILL)
        .instance(
            "pallet-consolidation",
            "acme-shipment-4711",
            CUSTOMER_INSTANCE,
        )
        .done()
}

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(fixtures()),
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

fn submit_args(skills: &[&str]) -> Value {
    json!({
        "tenant_id": TENANT,
        "candidate_id": "logistics-harvest",
        "vertical": "logistics-midmarket",
        "skills": skills,
    })
}

/// Decode a gzip tarball into its entry paths.
fn tar_paths(b64: &str) -> Vec<String> {
    let bytes = B64.decode(b64.as_bytes()).expect("base64");
    let gz = flate2::read::GzDecoder::new(&bytes[..]);
    let mut archive = tar::Archive::new(gz);
    archive
        .entries()
        .expect("entries")
        .map(|e| e.expect("entry").path().unwrap().display().to_string())
        .collect()
}

#[tokio::test]
async fn a_promotable_skill_submits_and_the_bundle_holds_exactly_it() {
    let p = start().await;
    let body = call(
        &p,
        Role::Admin,
        "submit_promotion",
        submit_args(&["pallet-consolidation"]),
    )
    .await;
    assert!(body.get("error").is_none(), "submit failed: {body}");
    let r = &body["result"]["structuredContent"];

    // The candidate bundle holds EXACTLY the eligible skill page —
    // no instances, ever (zero-leakage half 1).
    let paths = tar_paths(r["tarball_b64"].as_str().expect("tarball"));
    assert_eq!(paths, ["skills/pallet-consolidation.md"], "{body}");
    assert_eq!(r["manifest"]["id"], "logistics-harvest");
    assert_eq!(r["manifest"]["vertical"], "logistics-midmarket");
    assert!(r["event_id"].as_str().is_some_and(|s| !s.is_empty()));

    p.shutdown().await;
}

#[tokio::test]
async fn zero_leakage_battery_nothing_non_eligible_ever_crosses() {
    // AT-PROMO-1, modelled on the INV-ACL-FUSION regression test:
    // every shape that must NOT cross, refused fail-closed.
    let p = start().await;

    // 1. A skill WITHOUT the curator marker: default-deny.
    let body = call(
        &p,
        Role::Admin,
        "submit_promotion",
        submit_args(&["local-notes"]),
    )
    .await;
    let msg = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        msg.contains("promotion_not_eligible"),
        "unmarked skill must be refused: {body}"
    );

    // 2. An INSTANCE, even one tagged promotable: eligibility is
    //    skills-only; raw instance data never promotes.
    let body = call(
        &p,
        Role::Admin,
        "submit_promotion",
        submit_args(&["acme-shipment-4711"]),
    )
    .await;
    let msg = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        msg.contains("promotion_not_eligible"),
        "an instance must never promote: {body}"
    );

    // 3. A promotable skill whose body trips the deterministic
    //    scrubber: the whole submission aborts.
    let w = call(
        &p,
        Role::Admin,
        "update_page",
        json!({ "page_id": "markdown/skills/leaky-skill.md", "content": LEAKY_PROMOTABLE_SKILL }),
    )
    .await;
    assert_eq!(w["result"]["structuredContent"]["ok"], true, "{w}");
    let body = call(
        &p,
        Role::Admin,
        "submit_promotion",
        submit_args(&["leaky-skill", "pallet-consolidation"]),
    )
    .await;
    let msg = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        msg.contains("pack_secret_detected"),
        "credential-shaped content must abort the whole submission: {body}"
    );

    // 4. Mixed request: one eligible + one not ⇒ the WHOLE submission
    //    refuses (no silent partial harvest).
    let body = call(
        &p,
        Role::Admin,
        "submit_promotion",
        submit_args(&["pallet-consolidation", "local-notes"]),
    )
    .await;
    let msg = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        msg.contains("promotion_not_eligible"),
        "partial harvests must refuse atomically: {body}"
    );

    p.shutdown().await;
}

#[tokio::test]
async fn base_layer_pages_never_promote() {
    // Re-promoting the hub's own pack content back at it would launder
    // provenance; base pages are not the spoke's to promote.
    let p = start().await;
    // Land a base page through a real import (hub = this same process
    // exporting its promotable skill, imported into a second spoke
    // would collide — so hand-build the signed pack for a base skill
    // with a DIFFERENT id).
    let pages = vec![(
        "skills/pack-skill.md".to_owned(),
        "---\ntype: skill\nid: pack-skill\ndescription: from the pack.\npromotable: true\n---\n# pack-skill\n"
            .to_owned(),
    )];
    let tarball = escurel_server::pack::build_tarball(&pages).unwrap();
    let mut manifest = escurel_types::PackManifest {
        format_version: escurel_server::pack::PACK_FORMAT_VERSION,
        id: "upstream".into(),
        version: 1,
        vertical: "logistics-midmarket".into(),
        publisher: "hub.test".into(),
        page_count: 1,
        content_hash: escurel_server::pack::content_hash(&tarball),
        signature: String::new(),
    };
    manifest.signature = escurel_server::pack::sign_manifest(&manifest, PACK_SECRET);
    let imp = call(
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
    assert!(imp.get("error").is_none(), "{imp}");

    let body = call(
        &p,
        Role::Admin,
        "submit_promotion",
        submit_args(&["pack-skill"]),
    )
    .await;
    let msg = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        msg.contains("promotion_not_eligible"),
        "base-layer pages must never promote: {body}"
    );
    p.shutdown().await;
}

#[tokio::test]
async fn agents_cannot_set_the_promotable_marker() {
    // AT-PROMO-2: `promotable: true` is curator-set (admin in the v1
    // two-role model) — never by an agent, never by default.
    let p = start().await;
    let draft = "---\n\
        type: skill\n\
        id: local-notes\n\
        description: agent tries to self-promote.\n\
        promotable: true\n\
        ---\n\
        # local-notes\n";
    let w = call(
        &p,
        Role::Agent,
        "update_page",
        json!({ "page_id": "markdown/skills/local-notes.md", "content": draft }),
    )
    .await;
    let r = &w["result"]["structuredContent"];
    assert_eq!(r["ok"], false, "agent-set promotable must refuse: {w}");
    assert_eq!(r["issues"][0]["code"], "promotable_requires_curator");

    // The admin (curator) CAN set it.
    let w = call(
        &p,
        Role::Admin,
        "update_page",
        json!({ "page_id": "markdown/skills/local-notes.md", "content": draft }),
    )
    .await;
    assert_eq!(w["result"]["structuredContent"]["ok"], true, "{w}");

    p.shutdown().await;
}

#[tokio::test]
async fn every_submission_emits_an_immutable_audit_event() {
    // AT-PROMO-3: what left this spoke, when, submitted by whom —
    // replayable, contract-grade.
    let p = start().await;
    let body = call(
        &p,
        Role::Admin,
        "submit_promotion",
        submit_args(&["pallet-consolidation"]),
    )
    .await;
    assert!(body.get("error").is_none(), "{body}");
    let event_id = body["result"]["structuredContent"]["event_id"]
        .as_str()
        .expect("event_id")
        .to_owned();

    let inbox = call(&p, Role::Agent, "list_inbox", json!({})).await;
    let events = inbox["result"]["structuredContent"]["events"]
        .as_array()
        .expect("events")
        .clone();
    let ev = events
        .iter()
        .find(|e| e["event_id"] == event_id.as_str())
        .expect("audit event in the store");
    assert_eq!(ev["source"], "promotion");
    assert!(
        ev["body"]
            .as_str()
            .unwrap_or_default()
            .contains("skills/pallet-consolidation.md"),
        "the event records WHAT left: {ev}"
    );
    assert!(
        ev["provenance"]["submitted_by"].as_str().is_some(),
        "the event records WHO: {ev}"
    );

    p.shutdown().await;
}

#[tokio::test]
async fn the_auto_merge_path_cannot_launder_the_promotable_marker() {
    // agy MUST-FIX: the curator guard checks the DRAFT; a stale
    // base_version triggers a CRDT auto-merge whose OUTPUT can carry
    // `promotable: true` reconstructed from the head even when the
    // agent's draft never contained it. The guard must re-run on the
    // merged content — an agent write may never persist the marker.
    use duckdb::Connection;
    use escurel_crdt::{CrdtBackend, DuckdbCrdtBackend};
    use escurel_embed::{Embedder, ZeroEmbedder};
    use escurel_index::{Indexer, Migrator};
    use escurel_storage::{FsStore, LaneStore};
    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio::sync::Mutex;

    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let crdt_conn = conn.try_clone().unwrap();
    let indexer = Arc::new(Indexer::new(store, embedder, conn, TENANT).unwrap());
    let crdt_backend: Arc<dyn CrdtBackend> =
        Arc::new(DuckdbCrdtBackend::new(Arc::new(Mutex::new(crdt_conn))));
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: None,
        config_overrides: ConfigOverrides {
            indexer: Some(indexer),
            crdt_backend: Some(crdt_backend),
            pack_secret: Some(PACK_SECRET.to_owned()),
            ..Default::default()
        },
    })
    .await;

    let page_id = "markdown/skills/curated.md";
    // v1: an UNMARKED page — this is the base the agent will branch from.
    let unmarked = "---\ntype: skill\nid: curated\ndescription: curated.\n---\n# curated\n\nbody\n";
    let w = call(
        &p,
        Role::Admin,
        "update_page",
        json!({ "page_id": page_id, "content": unmarked }),
    )
    .await;
    assert_eq!(w["result"]["structuredContent"]["ok"], true, "{w}");
    let v1 = w["result"]["structuredContent"]["new_version"]
        .as_str()
        .unwrap()
        .to_owned();
    // v2: the head GAINS the marker (stands in for the CRDT-injection
    // shape — however the marker got into the head, the merged output
    // of an agent write will carry it).
    let marked = "---\ntype: skill\nid: curated\ndescription: curated.\npromotable: true\n---\n# curated\n\nbody\n";
    let w = call(
        &p,
        Role::Admin,
        "update_page",
        json!({ "page_id": page_id, "content": marked }),
    )
    .await;
    assert_eq!(w["result"]["structuredContent"]["ok"], true, "{w}");

    // The AGENT submits a clean draft (no marker anywhere) against the
    // stale v1 base. The plain draft-guard passes; the auto-merge
    // reconstructs (base → head) ∪ (base → draft) — and the head's side
    // carries `promotable: true` into the merged output. That output
    // must never persist from an agent call.
    let clean_draft =
        "---\ntype: skill\nid: curated\ndescription: curated.\n---\n# curated\n\nbody agent-edit\n";
    let w = call(
        &p,
        Role::Agent,
        "update_page",
        json!({ "page_id": page_id, "content": clean_draft, "base_version": v1 }),
    )
    .await;
    let r = &w["result"]["structuredContent"];
    if r["ok"] == true {
        // Merge went through: the persisted page must not carry the
        // marker (i.e. the guard stripped/refused the laundered copy).
        let ex = call(&p, Role::Agent, "expand", json!({ "page_id": page_id })).await;
        let fm = &ex["result"]["structuredContent"]["frontmatter"];
        assert!(
            fm.get("promotable").is_none() || fm["promotable"] != true,
            "an agent write may never persist the curator marker: {ex}"
        );
    } else {
        // Or the write refused outright — fail-closed is acceptable;
        // it must be the typed curator (or conflict) rejection, not a
        // silent server error.
        let code = r["issues"][0]["code"].as_str().unwrap_or_default();
        assert!(
            code == "promotable_requires_curator" || code == "conflict",
            "unexpected rejection: {w}"
        );
    }

    p.shutdown().await;
}

#[tokio::test]
async fn submit_promotion_requires_admin_role() {
    let p = start().await;
    let body = call(
        &p,
        Role::Agent,
        "submit_promotion",
        submit_args(&["pallet-consolidation"]),
    )
    .await;
    assert!(
        body.get("error").is_some(),
        "agent-role submission must be rejected: {body}"
    );
    p.shutdown().await;
}
