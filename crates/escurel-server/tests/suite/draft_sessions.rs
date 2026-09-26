//! A live personal draft: `open_session` on a DRAFT rather than on a page.
//!
//! A human editing an instance must not write the page as they type. The
//! existing session surface edits the page itself, so the only way to hold a
//! human's work for review was to finish it somewhere else and hand the whole
//! document to `create_draft` — which is not editing, it is submitting.
//!
//! What must hold, and is pinned below:
//!
//! - a session opened on a draft edits THAT DRAFT: the target page does not
//!   move while the session is open, and does not move when it commits;
//! - the edited bytes are what `promote_draft` lands, so the draft keeps every
//!   guard it already has (write ACL, CAS, `already_decided`);
//! - a draft is personal: another subject may not open a session on it, and
//!   must not learn whether it exists;
//! - a decided draft is not editable;
//! - the two targets are exclusive — a call must name a page or a draft.
//!
//! Real gateway, real `DuckdbCrdtBackend`, real DuckDB, real `/mcp`. No mocks.

use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use duckdb::Connection;
use escurel_crdt::{CrdtBackend, DuckdbCrdtBackend};
use escurel_index::Migrator;
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use loro::{ExportMode, LoroDoc};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::sync::Mutex;

const TENANT: &str = "acme";
const NOTE: &str = "---\ntype: skill\nid: note\ndescription: A note.\n\
    visibility: public\n---\n# note\n";
const BASE: &str = "---\ntype: instance\nskill: note\nid: plan\n---\n# Plan\n\nv1 body.\n";
const PAGE: &str = "markdown/instances/note/plan.md";
const AUTHOR: &str = "ada";
const STRANGER: &str = "grace";

struct Harness {
    process: EscurelProcess,
    _db_dir: TempDir,
}

async fn start() -> Harness {
    let db_dir = TempDir::new().unwrap();
    let conn = Connection::open(db_dir.path().join("crdt.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let shared = Arc::new(Mutex::new(conn));
    let crdt_backend: Arc<dyn CrdtBackend> = Arc::new(DuckdbCrdtBackend::new(Arc::clone(&shared)));

    let process = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("note", NOTE)
                .instance("note", "plan", BASE)
                .done(),
        ),
        config_overrides: ConfigOverrides {
            crdt_backend: Some(crdt_backend),
            ..Default::default()
        },
    })
    .await;
    Harness {
        process,
        _db_dir: db_dir,
    }
}

fn sha(s: &str) -> String {
    format!("{:x}", Sha256::digest(s.as_bytes()))
}

/// The whole JSON-RPC envelope, so a test can assert on a refusal as well as
/// on a result.
async fn raw(h: &Harness, token: &str, name: &str, args: Value) -> Value {
    reqwest::Client::new()
        .post(h.process.mcp_url())
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

async fn call(h: &Harness, token: &str, name: &str, args: Value) -> Value {
    let env = raw(h, token, name, args).await;
    assert!(env.get("error").is_none(), "{name} error: {env}");
    env["result"]["structuredContent"].clone()
}

fn author_token(h: &Harness) -> String {
    h.process.mint_token_with_sub(TENANT, Role::Agent, AUTHOR)
}

/// A draft of `PAGE` owned by [`AUTHOR`], holding `content`.
async fn draft_of(h: &Harness, content: &str) -> String {
    let created = call(
        h,
        &author_token(h),
        "create_draft",
        json!({ "target_page_id": PAGE, "content": content, "base_sha256": sha(BASE) }),
    )
    .await;
    assert_eq!(created["ok"], json!(true), "create_draft: {created}");
    created["draft"]["draft_id"]
        .as_str()
        .expect("draft_id")
        .to_owned()
}

/// A genuine Loro op carrying `body`, exactly as a live client sends it.
fn op_inserting(body: &str) -> String {
    let doc = LoroDoc::new();
    let vv = doc.oplog_vv();
    doc.get_text("body").insert(0, body).unwrap();
    doc.commit();
    B64.encode(doc.export(ExportMode::updates(&vv)).unwrap())
}

/// The page's stored hash, as `expand` publishes it.
async fn page_sha(h: &Harness, token: &str) -> Option<String> {
    let r = call(h, token, "expand", json!({ "page_id": PAGE })).await;
    r["content_sha256"].as_str().map(str::to_owned)
}

/// The edit goes to the draft, the page stays still, and promotion lands
/// exactly what the session produced.
///
/// The page assertions are the point: a session that quietly wrote through to
/// the page would satisfy "the draft changed" too.
#[tokio::test]
async fn a_session_on_a_draft_edits_the_draft_and_leaves_the_page_alone() {
    let h = start().await;
    let token = author_token(&h);
    let first = "---\ntype: instance\nskill: note\nid: plan\n---\n# Plan\n\nv2 draft.\n";
    let draft_id = draft_of(&h, first).await;
    let before = page_sha(&h, &token).await;

    let opened = call(&h, &token, "open_session", json!({ "draft_id": draft_id })).await;
    let sid = opened["session"].as_str().expect("session").to_owned();

    let edited = "---\ntype: instance\nskill: note\nid: plan\n---\n# Plan\n\nEDITED-LIVE.\n";
    let applied = raw(
        &h,
        &token,
        "apply_op",
        json!({ "session": sid, "op": op_inserting(edited) }),
    )
    .await;
    assert!(applied.get("error").is_none(), "apply_op: {applied}");

    // The page must not have moved while the session was open.
    assert_eq!(
        page_sha(&h, &token).await,
        before,
        "an open draft session must not touch the target page"
    );

    let closed = call(
        &h,
        &token,
        "close_session",
        json!({ "session": sid, "commit": true }),
    )
    .await;
    assert_eq!(closed["ok"], json!(true), "close_session: {closed}");

    // …nor when it commits: the commit belongs to the draft.
    assert_eq!(
        page_sha(&h, &token).await,
        before,
        "committing a draft session must not write the target page"
    );

    // The draft now holds what the session produced. Asserted on the row's own
    // bytes rather than on bytes computed here: the op inserts into the seeded
    // document, so what Loro merges is Loro's business — what this test pins is
    // that the merged result reached THE DRAFT.
    let diff = call(&h, &token, "diff_draft", json!({ "draft_id": draft_id })).await;
    assert_eq!(
        diff["ok"],
        json!(true),
        "diff_draft must work on a live draft: {diff}"
    );
    let drafts = call(&h, &token, "list_drafts", json!({})).await;
    let row = drafts["drafts"]
        .as_array()
        .expect("drafts")
        .iter()
        .find(|d| d["draft_id"] == json!(draft_id))
        .expect("the draft must still be listed")
        .clone();
    let held = row["content"].as_str().expect("content").to_owned();
    assert!(
        held.contains("EDITED-LIVE"),
        "the draft must hold the session's edit: {held:?}"
    );
    assert_ne!(
        held, first,
        "the draft must have moved off its original bytes"
    );
    assert_eq!(
        row["content_sha256"],
        json!(sha(&held)),
        "the draft's byte binding must match its bytes: {row}"
    );
    // The draft still records what it was made AGAINST: editing a proposal does
    // not change what it lands on top of, or the promote CAS would go blind.
    assert_eq!(
        row["base_sha256"],
        json!(sha(BASE)),
        "a session must not rewrite the draft's base: {row}"
    );

    // And promotion lands them, through the ordinary promote path.
    let promoted = call(&h, &token, "promote_draft", json!({ "draft_id": draft_id })).await;
    assert_eq!(promoted["ok"], json!(true), "promote_draft: {promoted}");
    assert_eq!(
        page_sha(&h, &token).await,
        Some(sha(&held)),
        "promotion must land exactly the edited draft"
    );
}

/// A draft is personal. Another subject may not open a session on it, and the
/// refusal must not distinguish "not yours" from "no such draft" — the two are
/// one answer on purpose.
#[tokio::test]
async fn another_subject_may_not_open_a_session_on_someone_elses_draft() {
    let h = start().await;
    let content = "---\ntype: instance\nskill: note\nid: plan\n---\n# Plan\n\nmine.\n";
    let draft_id = draft_of(&h, content).await;

    let stranger = h.process.mint_token_with_sub(TENANT, Role::Agent, STRANGER);
    let denied = raw(
        &h,
        &stranger,
        "open_session",
        json!({ "draft_id": draft_id }),
    )
    .await;
    let code = denied["error"]["data"]["code"].as_str().unwrap_or_default();
    assert_eq!(code, "forbidden", "expected a refusal, got: {denied}");

    let nonexistent = raw(
        &h,
        &stranger,
        "open_session",
        json!({ "draft_id": "01J0NOSUCHDRAFT000000000" }),
    )
    .await;
    assert_eq!(
        nonexistent["error"]["data"]["code"], denied["error"]["data"]["code"],
        "a draft that is not yours must read the same as one that does not exist"
    );
    assert_eq!(
        nonexistent["error"]["message"], denied["error"]["message"],
        "…including the message: {nonexistent} vs {denied}"
    );
}

/// A decided draft is not editable: the review is over.
#[tokio::test]
async fn a_decided_draft_has_no_session() {
    let h = start().await;
    let token = author_token(&h);
    let content = "---\ntype: instance\nskill: note\nid: plan\n---\n# Plan\n\ndone.\n";
    let draft_id = draft_of(&h, content).await;
    let promoted = call(&h, &token, "promote_draft", json!({ "draft_id": draft_id })).await;
    assert_eq!(promoted["ok"], json!(true), "promote_draft: {promoted}");

    let denied = raw(&h, &token, "open_session", json!({ "draft_id": draft_id })).await;
    assert_eq!(
        denied["error"]["data"]["code"],
        json!("already_decided"),
        "a promoted draft must not open a session: {denied}"
    );
}

/// The two targets are exclusive: a session edits a page or a draft, never
/// both and never neither.
#[tokio::test]
async fn open_session_names_exactly_one_target() {
    let h = start().await;
    let token = author_token(&h);
    let content = "---\ntype: instance\nskill: note\nid: plan\n---\n# Plan\n\nboth.\n";
    let draft_id = draft_of(&h, content).await;

    let neither = raw(&h, &token, "open_session", json!({})).await;
    assert!(
        neither.get("error").is_some(),
        "a session with no target must refuse: {neither}"
    );

    let both = raw(
        &h,
        &token,
        "open_session",
        json!({ "page_id": PAGE, "draft_id": draft_id }),
    )
    .await;
    assert!(
        both.get("error").is_some(),
        "a session naming both a page and a draft must refuse: {both}"
    );
}

/// A session id is not a capability over a personal draft.
///
/// Both ends of a page session fall back to "may this caller write the page?"
/// when the caller is not the opener — which is right for a page and wrong for
/// a draft: a draft key names no page, so that fallback would have let anyone
/// holding the session id type into, or abandon, someone else's unfinished
/// work. Session ids travel in tool results and logs.
#[tokio::test]
async fn a_session_id_does_not_let_another_subject_touch_a_personal_draft() {
    let h = start().await;
    let token = author_token(&h);
    let content = "---\ntype: instance\nskill: note\nid: plan\n---\n# Plan\n\nmine.\n";
    let draft_id = draft_of(&h, content).await;
    let opened = call(&h, &token, "open_session", json!({ "draft_id": draft_id })).await;
    let sid = opened["session"].as_str().expect("session").to_owned();

    let stranger = h.process.mint_token_with_sub(TENANT, Role::Agent, STRANGER);
    let applied = raw(
        &h,
        &stranger,
        "apply_op",
        json!({ "session": sid, "op": op_inserting("INJECTED.") }),
    )
    .await;
    assert_eq!(
        applied["error"]["data"]["code"],
        json!("forbidden"),
        "a stranger holding the session id must not apply ops: {applied}"
    );

    let discarded = raw(
        &h,
        &stranger,
        "close_session",
        json!({ "session": sid, "commit": false }),
    )
    .await;
    assert_eq!(
        discarded["error"]["data"]["code"],
        json!("forbidden"),
        "a stranger must not abandon someone else's draft session: {discarded}"
    );

    // The author's own session is untouched by the attempts above.
    let closed = call(
        &h,
        &token,
        "close_session",
        json!({ "session": sid, "commit": true }),
    )
    .await;
    assert_eq!(closed["ok"], json!(true), "close_session: {closed}");
    let drafts = call(&h, &token, "list_drafts", json!({})).await;
    let held = drafts["drafts"]
        .as_array()
        .expect("drafts")
        .iter()
        .find(|d| d["draft_id"] == json!(draft_id))
        .and_then(|d| d["content"].as_str())
        .unwrap_or_default()
        .to_owned();
    assert!(
        !held.contains("INJECTED"),
        "the refused op must not be in the draft: {held:?}"
    );
}
