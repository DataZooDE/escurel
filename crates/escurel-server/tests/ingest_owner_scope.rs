//! Owner-scoped + group-shared document ingest (REQ-DOC-06 extension).
//!
//! `/ingest` may name an explicit target `skill` (not just MIME routing), and
//! the materialised instance is **owned by the uploader** (`owner_field` ←
//! caller subject). That makes two real visibility shapes the herkules demo
//! needs:
//!   - a *personal* document skill (`read: [owner]`) → only the uploader reads;
//!   - a *group-shared* document skill (`read: [owner, <group>]`) → the whole
//!     group reads, others do not.
//!
//! Real gateway + DuckDB + OIDC, born-digital text (no kreuzberg). No mocks.

use std::sync::Arc;

use bytes::Bytes;
use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, LaneStore};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, Opts};
use serde_json::{Value, json};
use tempfile::TempDir;

const TENANT: &str = "herkules";

// A personal document skill: anyone may create their own; only the uploader reads.
const PERSONAL_SKILL: &str = "\
---
type: skill
id: ablage
description: Persönliche Ablage.
owner_field: author
acl:
  read: [owner]
  create: [owner]
backend:
  kind: document
  accepts: [text/plain]
---
# ablage
";

// A group-shared document skill: uploads are visible to the whole fraktion.
const TEAM_SKILL: &str = "\
---
type: skill
id: fraktion_gruene_dok
description: Interne Dokumente der Fraktion GRÜNE.
owner_field: author
acl:
  read: [owner, fraktion:gruene]
  create: [fraktion:gruene]
backend:
  kind: document
  accepts: [text/plain]
---
# fraktion_gruene_dok
";

struct Setup {
    process: EscurelProcess,
    store: Arc<dyn LaneStore>,
    _dirs: Vec<TempDir>,
}

async fn setup() -> Setup {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Arc::new(Indexer::new(Arc::clone(&store), embedder, conn, TENANT).unwrap());
    indexer
        .update_page("markdown/skills/ablage.md", PERSONAL_SKILL)
        .await
        .unwrap();
    indexer
        .update_page("markdown/skills/fraktion_gruene_dok.md", TEAM_SKILL)
        .await
        .unwrap();
    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            indexer: Some(Arc::clone(&indexer)),
            ..Default::default()
        },
        ..Default::default()
    })
    .await;
    Setup {
        process,
        store,
        _dirs: vec![store_dir, db_dir],
    }
}

async fn deposit(s: &Setup, body: &str) -> String {
    s.store
        .put_inbox_blob(TENANT, Bytes::from(body.as_bytes().to_vec()), None)
        .await
        .unwrap()
        .as_str()
        .to_owned()
}

/// `POST /ingest` with an optional explicit target `skill`.
async fn post_ingest(
    p: &EscurelProcess,
    token: &str,
    blob_id: &str,
    ct: &str,
    skill: Option<&str>,
) -> (reqwest::StatusCode, Value) {
    let mut body = json!({ "blob_id": blob_id, "content_type": ct });
    if let Some(sk) = skill {
        body["skill"] = json!(sk);
    }
    let resp = reqwest::Client::new()
        .post(format!("{}/ingest", p.base_url()))
        .header("authorization", format!("Bearer {token}"))
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = resp.status();
    (status, resp.json().await.unwrap())
}

async fn expand(p: &EscurelProcess, token: &str, page_id: &str) -> Value {
    let resp = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "expand", "arguments": { "page_id": page_id } },
        }))
        .send()
        .await
        .unwrap();
    let body: Value = resp.json().await.unwrap();
    body["result"]["structuredContent"].clone()
}

#[tokio::test]
async fn personal_upload_is_owner_private() {
    let s = setup().await;
    let alice = s
        .process
        .mint_token_with_groups(TENANT, "alice", &[], false);
    let bob = s.process.mint_token_with_groups(TENANT, "bob", &[], false);

    let blob = deposit(&s, "Meine private Notiz: KENNWORT-ALPHA.").await;
    let (status, resp) = post_ingest(&s.process, &alice, &blob, "text/plain", Some("ablage")).await;
    assert_eq!(status, 202, "ingest accepted: {resp}");
    assert_eq!(resp["status"], "materialised", "resp: {resp}");
    assert_eq!(
        resp["handler_skill"], "ablage",
        "explicit skill honoured: {resp}"
    );
    let page_id = resp["page_id"].as_str().expect("page_id").to_owned();

    // The uploader (owner) reads it…
    let own = expand(&s.process, &alice, &page_id).await;
    assert!(own["page"].is_object(), "alice reads her own upload: {own}");

    // …a different subject does not (owner-private).
    let other = expand(&s.process, &bob, &page_id).await;
    assert!(
        other["page"].is_null(),
        "bob must NOT read alice's personal upload: {other}"
    );

    s.process.shutdown().await;
}

#[tokio::test]
async fn fraktion_upload_is_shared_within_group_only() {
    let s = setup().await;
    let alice = s
        .process
        .mint_token_with_groups(TENANT, "alice", &["fraktion:gruene"], false);
    let carla = s
        .process
        .mint_token_with_groups(TENANT, "carla", &["fraktion:gruene"], false);
    let dora = s
        .process
        .mint_token_with_groups(TENANT, "dora", &["fraktion:cdu"], false);

    let blob = deposit(&s, "Fraktionsinterne Linie: KENNWORT-EISVOGEL.").await;
    // Two text/plain document skills exist; only an explicit target reaches the
    // fraktion skill (MIME routing would pick the alphabetically-first, `ablage`).
    let (status, resp) = post_ingest(
        &s.process,
        &alice,
        &blob,
        "text/plain",
        Some("fraktion_gruene_dok"),
    )
    .await;
    assert_eq!(status, 202, "ingest accepted: {resp}");
    assert_eq!(resp["handler_skill"], "fraktion_gruene_dok", "resp: {resp}");
    let page_id = resp["page_id"].as_str().expect("page_id").to_owned();

    // Another GRÜNE member reads the shared upload…
    let same = expand(&s.process, &carla, &page_id).await;
    assert!(
        same["page"].is_object(),
        "a fellow fraktion member reads the shared upload: {same}"
    );

    // …a member of another fraktion does not.
    let cross = expand(&s.process, &dora, &page_id).await;
    assert!(
        cross["page"].is_null(),
        "fraktion:cdu must NOT read GRÜNE's internal doc: {cross}"
    );

    s.process.shutdown().await;
}

#[tokio::test]
async fn unknown_or_incompatible_target_skill_is_rejected() {
    let s = setup().await;
    let alice = s
        .process
        .mint_token_with_groups(TENANT, "alice", &[], false);
    let blob = deposit(&s, "irrelevant").await;
    let (status, resp) = post_ingest(
        &s.process,
        &alice,
        &blob,
        "text/plain",
        Some("does_not_exist"),
    )
    .await;
    assert_eq!(
        status, 422,
        "a target skill that is not a document skill accepting the MIME is rejected: {resp}"
    );
    s.process.shutdown().await;
}

#[tokio::test]
async fn ingest_into_a_skill_the_caller_cannot_create_in_is_forbidden() {
    // A caller outside `fraktion:gruene` must NOT be able to inject a (group-
    // readable) document into `fraktion_gruene_dok` by naming it explicitly —
    // the explicit-target path enforces the skill's `create` ACL.
    let s = setup().await;
    let outsider = s
        .process
        .mint_token_with_groups(TENANT, "mallory", &["fraktion:cdu"], false);
    let blob = deposit(&s, "Versuchte Fremd-Einschleusung.").await;
    let (status, resp) = post_ingest(
        &s.process,
        &outsider,
        &blob,
        "text/plain",
        Some("fraktion_gruene_dok"),
    )
    .await;
    assert_eq!(
        status, 403,
        "a non-GRÜNE caller must be forbidden from creating in fraktion_gruene_dok: {resp}"
    );
    s.process.shutdown().await;
}
