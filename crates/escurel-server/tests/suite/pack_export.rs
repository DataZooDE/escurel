//! End-to-end tests for `export_pack` (REQ-PACK-01/02/04): building a
//! versioned, HMAC-signed skill pack — the Company Model's unit of
//! distribution — from a tenant's corpus.
//!
//! Real gateway, real DuckDB, real OIDC (TestIssuer JWKS), real
//! reqwest over `POST /mcp`; the returned tarball is decoded with the
//! real flate2/tar readers and the signature is recomputed with the
//! real hmac/sha2 crates. No mocks.
//!
//! Covers:
//! * AT-PACK-1 — export a skill subtree ⇒ conformant pack: the tar.gz
//!   holds exactly the selected skill pages (+ their instances when
//!   `include_instances`), the manifest carries id/version/vertical/
//!   publisher/content_hash, and the signature verifies under the
//!   shared pack secret.
//! * determinism — exporting twice yields byte-identical tarballs and
//!   equal manifests (a pack is content-addressed; rebuilds must not
//!   drift).
//! * fail-closed signing — a server with no pack secret configured
//!   refuses to export (packs are signed, always).
//! * fail-closed secret hygiene (REQ-PACK-04 / INV-SECRETFREE) — a
//!   selected page whose body carries a credential-shaped string (a
//!   DSN with embedded `user:pass@`) aborts the export with a typed
//!   issue; nothing is returned.
//! * the admin gate — an agent-role token is rejected.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use hmac::{Hmac, Mac};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const TENANT: &str = "acme";
const PACK_SECRET: &str = "test-pack-signing-secret";

const PALLET_SKILL: &str = "---\n\
type: skill\n\
id: pallet-consolidation\n\
description: Consolidate partial pallets.\n\
---\n\
# pallet-consolidation\n\nFirm-authored canonical procedure.\n";

const PALLET_EDGE: &str = "---\n\
type: instance\n\
skill: pallet-consolidation\n\
id: edge-mixed-carrier\n\
---\n\
# Edge case: mixed carrier\n\nTemplate.\n";

const OTHER_SKILL: &str = "---\n\
type: skill\n\
id: unrelated\n\
description: Not part of the pack.\n\
---\n\
# unrelated\n";

fn fixtures() -> FixtureBuilder {
    FixtureBuilder::new()
        .tenant(TENANT)
        .skill("pallet-consolidation", PALLET_SKILL)
        .instance("pallet-consolidation", "edge-mixed-carrier", PALLET_EDGE)
        .skill("unrelated", OTHER_SKILL)
        .done()
}

async fn start(pack_secret: Option<&str>) -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(fixtures()),
        config_overrides: ConfigOverrides {
            pack_secret: pack_secret.map(str::to_owned),
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

fn export_args() -> Value {
    json!({
        "tenant_id": TENANT,
        "id": "logistics-midmarket",
        "version": 7,
        "vertical": "logistics-midmarket",
        "publisher": "hub.stuttgart-ai",
        "skills": ["pallet-consolidation"],
        "include_instances": true,
    })
}

/// Decode a gzip tarball into `(entry path, contents)` pairs.
fn untar(bytes: &[u8]) -> Vec<(String, String)> {
    let gz = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(gz);
    let mut out = Vec::new();
    for entry in archive.entries().expect("tar entries") {
        let mut entry = entry.expect("tar entry");
        let path = entry.path().expect("path").display().to_string();
        if entry.header().entry_type().is_dir() {
            continue;
        }
        let mut body = String::new();
        std::io::Read::read_to_string(&mut entry, &mut body).expect("read entry");
        out.push((path, body));
    }
    out
}

#[tokio::test]
async fn export_pack_builds_a_signed_conformant_pack() {
    // AT-PACK-1.
    let p = start(Some(PACK_SECRET)).await;
    let body = call(&p, Role::Admin, "export_pack", export_args()).await;
    assert!(body.get("error").is_none(), "export_pack failed: {body}");
    let r = &body["result"]["structuredContent"];

    // The tarball holds exactly the selected subtree, canonical paths.
    let bytes = B64
        .decode(r["tarball_b64"].as_str().expect("tarball_b64"))
        .expect("valid base64");
    let mut entries = untar(&bytes);
    entries.sort();
    let paths: Vec<&str> = entries.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(
        paths,
        [
            "instances/pallet-consolidation/edge-mixed-carrier.md",
            "skills/pallet-consolidation.md",
        ],
        "pack must hold the selected skill + its instances and nothing else"
    );
    assert!(entries[1].1.contains("Firm-authored canonical procedure"));

    // Manifest: identity + integrity.
    let m = &r["manifest"];
    assert_eq!(m["format_version"], 1);
    assert_eq!(m["id"], "logistics-midmarket");
    assert_eq!(m["version"], 7);
    assert_eq!(m["vertical"], "logistics-midmarket");
    assert_eq!(m["publisher"], "hub.stuttgart-ai");
    assert_eq!(m["page_count"], 2);
    let hash = m["content_hash"].as_str().expect("content_hash");
    let recomputed = format!("sha256:{:x}", Sha256::digest(&bytes));
    assert_eq!(
        hash, recomputed,
        "content_hash must cover the tarball bytes"
    );

    // Signature: HMAC-SHA256 over the canonical manifest JSON —
    // the `PackManifest` struct serialisation with `signature` set to
    // the empty string (the documented signing contract; struct field
    // order fixes the byte layout). Recomputed here through the same
    // wire→struct decode an importer performs, with the real hmac/sha2
    // crates (REQ-PACK-02).
    let sig = m["signature"].as_str().expect("signature");
    let unsigned = escurel_types::PackManifest {
        signature: String::new(),
        ..serde_json::from_value(m.clone()).expect("manifest decodes")
    };
    let payload = serde_json::to_vec(&unsigned).unwrap();
    let mut mac = Hmac::<Sha256>::new_from_slice(PACK_SECRET.as_bytes()).unwrap();
    mac.update(&payload);
    let expect = format!("sha256={:x}", mac.finalize().into_bytes());
    assert_eq!(sig, expect, "signature must verify under the pack secret");

    p.shutdown().await;
}

#[tokio::test]
async fn export_pack_is_deterministic() {
    let p = start(Some(PACK_SECRET)).await;
    let a = call(&p, Role::Admin, "export_pack", export_args()).await;
    let b = call(&p, Role::Admin, "export_pack", export_args()).await;
    let (ra, rb) = (
        &a["result"]["structuredContent"],
        &b["result"]["structuredContent"],
    );
    assert_eq!(
        ra["tarball_b64"], rb["tarball_b64"],
        "two exports of unchanged content must be byte-identical"
    );
    assert_eq!(ra["manifest"], rb["manifest"]);
    p.shutdown().await;
}

#[tokio::test]
async fn export_pack_without_configured_secret_fails_closed() {
    // Packs are signed, always: a hub with no ESCUREL_PACK_SECRET must
    // refuse to publish rather than emit an unverifiable bundle.
    let p = start(None).await;
    let body = call(&p, Role::Admin, "export_pack", export_args()).await;
    let msg = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        msg.contains("pack_secret_not_configured"),
        "unsigned export must be refused: {body}"
    );
    p.shutdown().await;
}

#[tokio::test]
async fn export_pack_rejects_credential_shaped_content() {
    // REQ-PACK-04 / INV-SECRETFREE, fail-closed: a DSN with embedded
    // credentials in a selected page aborts the whole export.
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("pallet-consolidation", PALLET_SKILL)
                .instance(
                    "pallet-consolidation",
                    "leaky",
                    "---\ntype: instance\nskill: pallet-consolidation\nid: leaky\n---\n\
                     # leaky\n\nConnect via postgres://svc:hunter2@db.internal/prod\n",
                )
                .done(),
        ),
        config_overrides: ConfigOverrides {
            pack_secret: Some(PACK_SECRET.to_owned()),
            ..Default::default()
        },
    })
    .await;
    let body = call(&p, Role::Admin, "export_pack", export_args()).await;
    let msg = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        msg.contains("pack_secret_detected"),
        "credential-shaped content must abort the export: {body}"
    );
    p.shutdown().await;
}

#[tokio::test]
async fn export_pack_requires_admin_role() {
    let p = start(Some(PACK_SECRET)).await;
    let body = call(&p, Role::Agent, "export_pack", export_args()).await;
    assert!(
        body.get("error").is_some(),
        "agent-role export_pack must be rejected: {body}"
    );
    p.shutdown().await;
}
