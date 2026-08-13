//! F2.5: a rebase interrupted after its pages land is completed by re-running
//! the same rebase.
//!
//! `tool_rebase_pack` applies in a deliberate order — land every page, remove
//! the orphans, and move the version pin **last** — and a code comment claims
//! that this makes a crash mid-apply recoverable by simply re-running. That
//! claim rested entirely on the comment. Nothing asserted it, which is the
//! same shape as the two defects the concurrency review already found.
//!
//! ## How the crash is produced, stated plainly
//!
//! `EscurelProcess` runs the gateway in-process (`tokio::spawn`), so there is
//! no pid to kill mid-apply and no way to time a signal into the window
//! between the last page write and the pin move. This test therefore
//! **reconstructs the post-crash state** rather than inducing it: it writes
//! part of v2 through a real `Indexer` — the same store the gateway is
//! serving, real DuckDB, real markdown lane — and leaves the subscription
//! pinned at v1. That is byte-for-byte the state a crash in that window
//! leaves behind.
//!
//! What this does prove: from that state, the ordinary `rebase_pack` call
//! succeeds, is not rejected as a duplicate, converges every page to v2,
//! removes the dropped skill, and moves the pin. What it does not prove is
//! anything about signal handling or a torn DuckDB write — those need a real
//! process and are out of this test's reach.
//!
//! Real gateway, real DuckDB, real OIDC, real `/mcp`; packs built with the
//! real bundler + HMAC signer. No mocks.

use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, LaneStore};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, Opts, Role};
use serde_json::{Value, json};
use tempfile::TempDir;

const TENANT: &str = "acme";
const PACK_SECRET: &str = "shared-pack-signing-secret";
const PACK: &str = "logistics";

fn skill(id: &str, description: &str) -> String {
    format!("---\ntype: skill\nid: {id}\ndescription: {description}\n---\n# {id}\n\nbody\n")
}

fn v1_pages() -> Vec<(String, String)> {
    vec![
        ("skills/alpha.md".to_owned(), skill("alpha", "alpha v1.")),
        ("skills/beta.md".to_owned(), skill("beta", "beta v1.")),
    ]
}

/// v2 changes `alpha`, drops `beta`, adds `gamma` — so a completed rebase has
/// to do all three things, not just overwrite.
fn v2_pages() -> Vec<(String, String)> {
    vec![
        ("skills/alpha.md".to_owned(), skill("alpha", "alpha v2.")),
        ("skills/gamma.md".to_owned(), skill("gamma", "gamma v2.")),
    ]
}

fn signed(pages: &[(String, String)], version: u32) -> (Value, String) {
    let tarball = escurel_server::pack::build_tarball(pages).unwrap();
    let mut m = escurel_types::PackManifest {
        format_version: escurel_server::pack::PACK_FORMAT_VERSION,
        id: PACK.into(),
        version,
        vertical: PACK.into(),
        publisher: "hub.test".into(),
        page_count: pages.len() as u32,
        content_hash: escurel_server::pack::content_hash(&tarball),
        signature: String::new(),
    };
    m.signature = escurel_server::pack::sign_manifest(&m, PACK_SECRET);
    (serde_json::to_value(&m).unwrap(), B64.encode(&tarball))
}

struct Setup {
    process: EscurelProcess,
    indexer: Arc<Indexer>,
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

    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            indexer: Some(Arc::clone(&indexer)),
            pack_secret: Some(PACK_SECRET.to_owned()),
            ..Default::default()
        },
        ..Default::default()
    })
    .await;

    Setup {
        process,
        indexer,
        _dirs: vec![store_dir, db_dir],
    }
}

async fn call(s: &Setup, name: &str, args: Value) -> Value {
    let token = s.process.mint_token(TENANT, Role::Admin);
    reqwest::Client::new()
        .post(s.process.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": args } }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json")
}

fn sc(env: &Value) -> Value {
    env["result"]["structuredContent"].clone()
}

fn base_id(rel: &str) -> String {
    format!(
        "{}{}/{}",
        escurel_index::pack::RESERVED_BASE_PREFIX,
        PACK,
        rel
    )
}

#[tokio::test]
async fn crash_mid_apply_is_resumable_by_rerunning() {
    let s = setup().await;

    // Subscribe at v1.
    let (m1, t1) = signed(&v1_pages(), 1);
    let r = call(
        &s,
        "import_pack",
        json!({ "tenant_id": TENANT, "manifest": m1, "tarball_b64": t1 }),
    )
    .await;
    assert!(r.get("error").is_none(), "import v1 must succeed: {r}");

    // --- reconstruct the crash window -----------------------------------
    // A rebase to v2 got as far as writing `alpha` and died before writing
    // `gamma`, before removing `beta`, and before moving the pin. Written
    // through the real Indexer the gateway is serving, with the layer stamp
    // a real apply would have used.
    let layer = format!("base@{PACK}@v2");
    let alpha_v2 = escurel_server::pack::stamp_layer(&skill("alpha", "alpha v2."), &layer).unwrap();
    s.indexer
        .update_page(&base_id("skills/alpha.md"), &alpha_v2)
        .await
        .expect("partial apply write");

    // The pin must still read v1 — that is what makes this the crash window
    // rather than a completed rebase.
    let subs = s
        .indexer
        .list_pack_subscriptions()
        .await
        .expect("subscriptions");
    let pinned = subs.iter().find(|x| x.pack_id == PACK).expect("subscribed");
    assert_eq!(pinned.version, 1, "precondition: the pin has not moved");

    // Preconditions that keep the post-recovery assertions from being
    // vacuous: beta is still present (so its later absence means the re-run
    // removed it) and gamma is still absent (so its later presence means the
    // re-run wrote it).
    assert!(
        s.indexer
            .page_content(&base_id("skills/beta.md"))
            .await
            .expect("read beta")
            .is_some(),
        "precondition: the interrupted run had not yet removed beta"
    );
    assert!(
        s.indexer
            .page_content(&base_id("skills/gamma.md"))
            .await
            .expect("read gamma")
            .is_none(),
        "precondition: the interrupted run had not yet written gamma"
    );

    // --- the recovery procedure: re-run the same rebase ------------------
    let (m2, t2) = signed(&v2_pages(), 2);
    let rebased = call(
        &s,
        "rebase_pack",
        json!({ "tenant_id": TENANT, "manifest": m2, "tarball_b64": t2 }),
    )
    .await;
    assert!(
        rebased.get("error").is_none(),
        "re-run must not error: {rebased}"
    );
    let out = sc(&rebased);
    assert_eq!(
        out["ok"], true,
        "a re-run from the crash window must complete the rebase, not be \
         rejected as already-applied: {rebased}"
    );

    // Converged: alpha at v2, gamma present, beta gone.
    let alpha = s
        .indexer
        .page_content(&base_id("skills/alpha.md"))
        .await
        .expect("read alpha")
        .expect("alpha present");
    assert!(
        alpha.contains("alpha v2."),
        "alpha must be at v2 after recovery: {alpha}"
    );
    assert!(
        alpha.contains(&layer),
        "alpha must carry the v2 layer stamp: {alpha}"
    );

    let gamma = s
        .indexer
        .page_content(&base_id("skills/gamma.md"))
        .await
        .expect("read gamma");
    assert!(
        gamma.is_some(),
        "gamma is new in v2 and the interrupted run never wrote it; \
         the re-run must land it"
    );

    let beta = s
        .indexer
        .page_content(&base_id("skills/beta.md"))
        .await
        .expect("read beta");
    assert!(
        beta.is_none(),
        "beta was dropped in v2 and the interrupted run never removed it; \
         the re-run must: {beta:?}"
    );

    // And the pin finally moves.
    let subs = s
        .indexer
        .list_pack_subscriptions()
        .await
        .expect("subscriptions");
    let pinned = subs.iter().find(|x| x.pack_id == PACK).expect("subscribed");
    assert_eq!(
        pinned.version, 2,
        "the pin moves only once every page has landed"
    );
}
