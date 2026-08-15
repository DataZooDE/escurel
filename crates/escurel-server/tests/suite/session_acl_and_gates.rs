//! CRDT session tools + `/ws` must sit behind the same gates as the rest
//! of the write/read surface (security review, 2026-08).
//!
//! Four confirmed holes, each pinned here before the fix:
//!
//! 1. HTTP `open_session` took no caller at all, so a non-owner could open
//!    a live session on an owner-private page and edit it op by op —
//!    bypassing the write ACL `update_page` enforces.
//! 2. `close_session` committed via `update_page_as` directly, so a
//!    session opened while writable stayed committable after the page's
//!    ownership changed. The commit must RE-CHECK the same write policy
//!    `update_page` uses.
//! 3. `list_snapshots` consulted no ACL: a non-owner could enumerate the
//!    snapshot history of an owner-private page. Denial must read as
//!    absence (the `list_op_authors` shape), not as an error.
//! 4. `/ws` authenticated but never checked `tenant_suspended`; `/mcp`
//!    rejects non-admin callers of a suspended tenant, so the socket was
//!    a way around the suspension.
//!
//! Real gateway, real DuckDB indexer, real CRDT backend, real OIDC
//! (TestIssuer JWKS), real tokio-tungstenite. No mocks.

use std::sync::Arc;

use duckdb::Connection;
use escurel_admin::{FsTenantStore, TenantStore};
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, LaneStore};
use escurel_test_support::{
    AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role, WriteAclMode,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

const TENANT: &str = "stuttgart-ai";
const ALICE: &str = "whatsapp:111";
const BOB: &str = "whatsapp:222";

// Same owner-private shape as `write_acl.rs`: only the resolved owner (or
// admin) may write an instance of `community_member`.
const MEMBER_SKILL: &str = "---\ntype: skill\nid: community_member\n\
    description: A member.\nvisibility: owner\nowner_field: credential\n---\n# community_member\n";
const ALICE_MEMBER: &str = "---\ntype: instance\nskill: community_member\nid: alice\n\
    credential: \"whatsapp:111\"\n---\n# Alice\n";
const BOB_MEMBER: &str = "---\ntype: instance\nskill: community_member\nid: bob\n\
    credential: \"whatsapp:222\"\n---\n# Bob\n";

const ALICE_PAGE: &str = "markdown/instances/community_member/alice.md";
const NO_SUCH_PAGE: &str = "markdown/instances/community_member/no-such-member.md";

/// Gateway with the write ACL ENFORCED and a live CRDT backend, so the
/// session tools run against the same policy `update_page` does.
async fn start_enforced() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            live_crdt: true,
            write_acl: Some(WriteAclMode::Enforce),
            ..Default::default()
        },
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("community_member", MEMBER_SKILL)
                .instance("community_member", "alice", ALICE_MEMBER)
                .instance("community_member", "bob", BOB_MEMBER)
                .done(),
        ),
    })
    .await
}

/// POST a `tools/call` with `token`; returns the full JSON-RPC envelope.
async fn call(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    let resp = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": args },
        }))
        .send()
        .await
        .expect("post");
    assert_eq!(resp.status(), 200, "http status");
    resp.json().await.unwrap()
}

async fn call_ok(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    let body = call(p, token, name, args).await;
    assert!(body.get("error").is_none(), "{name} errored: {body}");
    body["result"]["structuredContent"].clone()
}

// ------------------------------------------------ 1. open_session ACL ---

/// A non-owner must not open a live session on an owner-private page: the
/// op stream would edit a page `update_page` refuses them byte by byte.
/// The refusal carries the `forbidden` data code; the positive control in
/// the same test proves the gate is a decision, not a wall.
#[tokio::test]
async fn non_owner_cannot_open_session_on_an_owner_private_page() {
    let p = start_enforced().await;
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let bob = p.mint_token_with_sub(TENANT, Role::Agent, BOB);

    // The hole: Bob opening a session on Alice's page must be refused.
    // (Checked FIRST — the registry allows one open session per page, so
    // a grant to Bob here would block the control below.)
    let denied = call(&p, &bob, "open_session", json!({ "page_id": ALICE_PAGE })).await;
    assert!(
        denied["result"]["structuredContent"]["session"]
            .as_str()
            .is_none(),
        "bob must NOT receive a session on alice's page: {denied}"
    );
    assert_eq!(
        denied["error"]["data"]["code"],
        json!("forbidden"),
        "the refusal must carry the write-ACL `forbidden` code: {denied}"
    );

    // Positive control: the owner opens a session on her own page.
    let opened = call(&p, &alice, "open_session", json!({ "page_id": ALICE_PAGE })).await;
    assert!(
        opened["result"]["structuredContent"]["session"].is_string(),
        "control: alice must open a session on her own page: {opened}"
    );

    p.shutdown().await;
}

// -------------------------------------- 2. close_session re-checks ACL ---

/// The write policy is re-checked at COMMIT time: a session opened while
/// the caller could write the page must not commit after the page's
/// ownership changed underneath it. The refusal uses the same
/// `{ok: false, issues: [forbidden]}` shape `update_page` returns, and the
/// session stays open (discardable), mirroring the failing-indexer-write
/// contract.
#[tokio::test]
async fn acl_change_mid_session_denies_the_close_commit() {
    let p = start_enforced().await;
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let admin = p.mint_token(TENANT, Role::Admin);

    // Alice legitimately opens a session on her own page and types.
    let opened = call_ok(&p, &alice, "open_session", json!({ "page_id": ALICE_PAGE })).await;
    let session = opened["session"].as_str().expect("session id").to_owned();
    let doc = loro::LoroDoc::new();
    doc.get_text("body").insert(0, "alice's draft").unwrap();
    doc.commit();
    let op = {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD
            .encode(doc.export(loro::ExportMode::all_updates()).unwrap())
    };
    let applied = call_ok(
        &p,
        &alice,
        "apply_op",
        json!({ "session": session, "op": op }),
    )
    .await;
    assert_eq!(applied["ok"], json!(true), "seed op must apply: {applied}");

    // The page changes hands while the session is open (admin transfer).
    let transferred = "---\ntype: instance\nskill: community_member\nid: alice\n\
        credential: \"whatsapp:222\"\n---\n# Alice\n\nNow bob's record.\n";
    let r = call_ok(
        &p,
        &admin,
        "update_page",
        json!({ "page_id": ALICE_PAGE, "content": transferred }),
    )
    .await;
    assert_eq!(r["ok"], json!(true), "admin transfer must land: {r}");

    // Alice's commit must now be refused with update_page's denial shape…
    let closed = call_ok(
        &p,
        &alice,
        "close_session",
        json!({ "session": session, "commit": true }),
    )
    .await;
    assert_eq!(
        closed["ok"],
        json!(false),
        "the commit must be refused after the transfer: {closed}"
    );
    assert_eq!(
        closed["issues"][0]["code"],
        json!("forbidden"),
        "same error shape as update_page: {closed}"
    );

    // …the page body must be untouched…
    let page = call_ok(&p, &admin, "expand", json!({ "page_id": ALICE_PAGE })).await;
    let body = page["body"].as_str().unwrap_or_default();
    assert!(
        !body.contains("alice's draft"),
        "the refused commit must not have written through: {body}"
    );

    // …and the session must still be open: a discard still works.
    let discarded = call_ok(
        &p,
        &alice,
        "close_session",
        json!({ "session": session, "commit": false }),
    )
    .await;
    assert_eq!(
        discarded["ok"],
        json!(true),
        "a refused commit must leave the session open for a discard: {discarded}"
    );

    p.shutdown().await;
}

// ------------------------------------------- 3. list_snapshots read ACL ---

/// `list_snapshots` follows the page's read ACL, and denial reads as
/// absence: the refused answer is byte-identical to the answer for a page
/// that does not exist — no existence oracle (the `list_op_authors`
/// contract, applied to snapshot history).
#[tokio::test]
async fn list_snapshots_denial_reads_as_absence_not_as_an_error() {
    // The gateway's own CRDT store lives in a separate DuckDB from the
    // indexer, so real snapshot rows are seeded through the indexer the
    // server is handed (the `seed_snapshot_history` seam the demo uses).
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Arc::new(Indexer::new(Arc::clone(&store), embedder, conn, TENANT).unwrap());
    indexer
        .update_page("markdown/skills/community_member.md", MEMBER_SKILL)
        .await
        .unwrap();
    indexer.update_page(ALICE_PAGE, ALICE_MEMBER).await.unwrap();
    indexer
        .seed_snapshot_history(ALICE_PAGE, &[("2026-08-01T10:00:00Z", ALICE_MEMBER)])
        .await
        .unwrap();

    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            indexer: Some(Arc::clone(&indexer)),
            ..Default::default()
        },
        ..Default::default()
    })
    .await;
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let bob = p.mint_token_with_sub(TENANT, Role::Agent, BOB);

    // Positive control: the owner sees her real history.
    let mine = call_ok(
        &p,
        &alice,
        "list_snapshots",
        json!({ "page_id": ALICE_PAGE }),
    )
    .await;
    assert_eq!(
        mine["snapshots"].as_array().map(Vec::len),
        Some(1),
        "control: alice must see her own snapshot history: {mine}"
    );

    // The hole: a non-owner enumerating the owner-private history.
    let hidden = call(&p, &bob, "list_snapshots", json!({ "page_id": ALICE_PAGE })).await;
    let ghost = call(
        &p,
        &bob,
        "list_snapshots",
        json!({ "page_id": NO_SUCH_PAGE }),
    )
    .await;
    assert!(
        hidden.get("error").is_none(),
        "a denied listing must not fault: {hidden}"
    );
    let strip = |v: &Value| v["result"]["structuredContent"]["snapshots"].clone();
    assert_eq!(
        strip(&hidden),
        strip(&ghost),
        "hidden must be indistinguishable from absent: {hidden} vs {ghost}"
    );
    assert_eq!(
        strip(&hidden),
        json!([]),
        "the denied history must be empty: {hidden}"
    );

    p.shutdown().await;
}

// ------------------------------------------ 4. /ws tenant-suspend gate ---

/// `/ws` must honour the same suspend gate as `/mcp` (#247): a suspended
/// tenant rejects a non-admin upgrade with HTTP 403 BEFORE the socket
/// opens, while an admin still connects (to resume).
#[tokio::test]
async fn suspended_tenant_is_rejected_at_the_ws_upgrade() {
    const ACME: &str = "acme";
    let tenants_dir = TempDir::new().unwrap();
    let tenant_store: Arc<dyn TenantStore> =
        Arc::new(FsTenantStore::new(tenants_dir.path().to_path_buf()));
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            tenant_store: Some(tenant_store),
            ..Default::default()
        },
        fixtures: Some(FixtureBuilder::new().tenant(ACME).done()),
    })
    .await;
    let admin = p.mint_token(ACME, Role::Admin);
    let agent = p.mint_token(ACME, Role::Agent);

    let ws_req = |bearer: &str| {
        let mut req = p.ws_url().into_client_request().unwrap();
        req.headers_mut()
            .insert("authorization", format!("Bearer {bearer}").parse().unwrap());
        req
    };

    call_ok(
        &p,
        &admin,
        "tenant_create",
        json!({ "tenant_id": ACME, "display_name": "Acme" }),
    )
    .await;

    // Control: an agent upgrade works while the tenant is active.
    let ok = tokio_tungstenite::connect_async(ws_req(&agent)).await;
    assert!(ok.is_ok(), "active tenant must serve /ws: {ok:?}");
    drop(ok);

    // Suspend.
    call_ok(
        &p,
        &admin,
        "tenant_update",
        json!({ "tenant_id": ACME, "status": "suspended" }),
    )
    .await;

    // The hole: the agent upgrade must now be refused before the upgrade.
    match tokio_tungstenite::connect_async(ws_req(&agent)).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(resp)) => {
            assert_eq!(
                resp.status(),
                403,
                "a suspended tenant must reject the upgrade with 403"
            );
        }
        other => panic!("suspended tenant must refuse the WS upgrade, got {other:?}"),
    }

    // The gate never blocks admin — the operator can still act.
    let admin_sock = tokio_tungstenite::connect_async(ws_req(&admin)).await;
    assert!(
        admin_sock.is_ok(),
        "admin must still connect to /ws while suspended: {admin_sock:?}"
    );

    p.shutdown().await;
}
