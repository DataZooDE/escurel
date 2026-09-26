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

/// A draft may propose a page that does not exist yet, and that draft must be
/// editable like any other.
///
/// `open_session` refuses a PAGE that is not there — absence must not read
/// differently from "not yours". A draft carries its own authorisation, so the
/// same refusal applied to a create-draft would have made exactly the drafts
/// that need the most editing the ones that cannot be edited.
#[tokio::test]
async fn a_draft_that_creates_a_page_can_still_be_edited() {
    let h = start().await;
    let token = author_token(&h);
    let fresh = "markdown/instances/note/brand-new.md";
    let proposed = "---\ntype: instance\nskill: note\nid: brand-new\n---\n# New\n\nfirst.\n";
    let created = call(
        &h,
        &token,
        "create_draft",
        json!({ "target_page_id": fresh, "content": proposed, "base_sha256": "" }),
    )
    .await;
    assert_eq!(created["ok"], json!(true), "create_draft: {created}");
    let draft_id = created["draft"]["draft_id"].as_str().expect("draft_id");

    let opened = raw(&h, &token, "open_session", json!({ "draft_id": draft_id })).await;
    assert!(
        opened.get("error").is_none(),
        "a draft for a page that does not exist yet must still open: {opened}"
    );
}

/// A decided draft must not leave its session holding a quota slot.
///
/// The commit refuses — the work is not landable — and the earlier shape of that
/// refusal returned before closing, so the session stayed in the registry with
/// its `concurrent_sessions` permit and its one-session-per-draft reservation
/// until the idle TTL (30 minutes) expired.
#[tokio::test]
async fn a_draft_decided_under_an_open_session_does_not_wedge_it() {
    let h = start().await;
    let token = author_token(&h);
    let content = "---\ntype: instance\nskill: note\nid: plan\n---\n# Plan\n\nracing.\n";
    let draft_id = draft_of(&h, content).await;
    let opened = call(&h, &token, "open_session", json!({ "draft_id": draft_id })).await;
    let sid = opened["session"].as_str().expect("session").to_owned();
    let applied = raw(
        &h,
        &token,
        "apply_op",
        json!({ "session": sid, "op": op_inserting("TOO-LATE.") }),
    )
    .await;
    assert!(applied.get("error").is_none(), "apply_op: {applied}");

    // Decided out from under the live session.
    let promoted = call(&h, &token, "promote_draft", json!({ "draft_id": draft_id })).await;
    assert_eq!(promoted["ok"], json!(true), "promote_draft: {promoted}");

    let closed = call(
        &h,
        &token,
        "close_session",
        json!({ "session": sid, "commit": true }),
    )
    .await;
    assert_eq!(
        closed["ok"],
        json!(false),
        "the commit must refuse: {closed}"
    );
    assert_eq!(
        closed["issues"][0]["code"],
        json!("already_decided"),
        "…as already decided: {closed}"
    );

    // The session must be gone, not merely refused: a second call on the same
    // id can no longer find it, which is what proves the slot was released.
    let again = raw(
        &h,
        &token,
        "close_session",
        json!({ "session": sid, "commit": false }),
    )
    .await;
    assert_eq!(
        again["error"]["data"]["code"],
        json!("unknown_session"),
        "the refused commit must have closed the session: {again}"
    );
}

/// Committing nothing is still closing someone's session, so it takes the same
/// identity as committing something — and the refusal must not name the draft.
#[tokio::test]
async fn a_stranger_cannot_close_an_empty_draft_session_or_learn_its_draft() {
    let h = start().await;
    let token = author_token(&h);
    let content = "---\ntype: instance\nskill: note\nid: plan\n---\n# Plan\n\nuntouched.\n";
    let draft_id = draft_of(&h, content).await;
    let opened = call(&h, &token, "open_session", json!({ "draft_id": draft_id })).await;
    let sid = opened["session"].as_str().expect("session").to_owned();

    // No op applied: the session's content is empty, which used to skip the
    // whole authorisation block on the way to closing it.
    let stranger = h.process.mint_token_with_sub(TENANT, Role::Agent, STRANGER);
    let refused = call(
        &h,
        &stranger,
        "close_session",
        json!({ "session": sid, "commit": true }),
    )
    .await;
    assert_eq!(
        refused["ok"],
        json!(false),
        "a stranger must not close an empty draft session: {refused}"
    );
    assert_eq!(refused["issues"][0]["code"], json!("forbidden"));
    let message = refused["issues"][0]["message"].as_str().unwrap_or_default();
    assert!(
        !message.contains(&draft_id),
        "the refusal must not name the draft: {message:?}"
    );

    // The author's session survived the attempt.
    let closed = call(
        &h,
        &token,
        "close_session",
        json!({ "session": sid, "commit": false }),
    )
    .await;
    assert_eq!(closed["ok"], json!(true), "close_session: {closed}");
}

/// `page_id` is a page path, and the draft key namespace is not addressable
/// through it: a forged key would seize the real author's one-session-per-draft
/// reservation and route a commit into the draft store.
#[tokio::test]
async fn a_draft_key_cannot_be_forged_through_page_id() {
    let h = start().await;
    let token = author_token(&h);
    let content = "---\ntype: instance\nskill: note\nid: plan\n---\n# Plan\n\nmine.\n";
    let draft_id = draft_of(&h, content).await;

    let stranger = h.process.mint_token_with_sub(TENANT, Role::Agent, STRANGER);
    let forged = raw(
        &h,
        &stranger,
        "open_session",
        json!({ "page_id": format!("draft:{draft_id}") }),
    )
    .await;
    assert!(
        forged.get("error").is_some(),
        "a `draft:` page id must refuse: {forged}"
    );

    // The author's own session is still available, which is what the forgery
    // would have taken away.
    let opened = raw(&h, &token, "open_session", json!({ "draft_id": draft_id })).await;
    assert!(
        opened.get("error").is_none(),
        "the author must still be able to open their draft: {opened}"
    );
}

// --- the live channel ------------------------------------------
//
// A draft session exists to be typed into, which happens over `/ws`, not over
// `apply_op`. The attach gate reads the session's key — so a key that is not a
// page had to be given an authority of its own, and these two tests are what
// say it is the right one.

type Sock =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

fn ws_request(
    url: &str,
    bearer: &str,
) -> tokio_tungstenite::tungstenite::handshake::client::Request {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut req = url.into_client_request().unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {bearer}").parse().unwrap());
    req
}

async fn send_frame(sock: &mut Sock, v: Value) {
    use futures::SinkExt;
    use tokio_tungstenite::tungstenite::protocol::Message;
    sock.send(Message::Text(v.to_string())).await.unwrap();
}

async fn recv_frame(sock: &mut Sock) -> Value {
    use futures::StreamExt;
    use tokio_tungstenite::tungstenite::protocol::Message;
    let msg = tokio::time::timeout(std::time::Duration::from_secs(5), sock.next())
        .await
        .expect("recv timed out")
        .expect("stream ended")
        .expect("ws error");
    let txt = match msg {
        Message::Text(t) => t,
        Message::Binary(b) => String::from_utf8(b).unwrap(),
        other => panic!("expected a text frame, got {other:?}"),
    };
    serde_json::from_str(&txt).expect("json frame")
}

/// The author may attach to their own draft session over `/ws`.
///
/// The attach gate resolves the session's key to a page and asks the page's read
/// ACL. A draft key resolves to no page, so the gate failed closed and live
/// editing of a draft — the reason to open one at all — was unreachable.
#[tokio::test]
async fn the_author_may_attach_to_their_own_draft_session_over_ws() {
    let h = start().await;
    let token = author_token(&h);
    let content = "---\ntype: instance\nskill: note\nid: plan\n---\n# Plan\n\nlive.\n";
    let draft_id = draft_of(&h, content).await;
    let opened = call(&h, &token, "open_session", json!({ "draft_id": draft_id })).await;
    let sid = opened["session"].as_str().expect("session").to_owned();

    let (mut sock, _) = tokio_tungstenite::connect_async(ws_request(&h.process.ws_url(), &token))
        .await
        .expect("ws connect");
    send_frame(&mut sock, json!({ "type": "hello", "session": sid })).await;
    // Presence echoes only on an attached session, so it is the cheapest proof
    // that the gate let this connection in.
    send_frame(
        &mut sock,
        json!({ "type": "presence", "session": sid, "user": AUTHOR, "anchor": "#plan" }),
    )
    .await;
    let echo = recv_frame(&mut sock).await;
    assert_eq!(echo["type"], json!("presence"), "expected presence: {echo}");
    assert_eq!(echo["session"], json!(sid));

    sock.close(None).await.ok();
}

/// …and nobody else may, however they came by the session id — nor learn which
/// draft it belongs to from the refusal.
#[tokio::test]
async fn another_subject_may_not_attach_to_a_draft_session_over_ws() {
    let h = start().await;
    let token = author_token(&h);
    let content = "---\ntype: instance\nskill: note\nid: plan\n---\n# Plan\n\nprivate.\n";
    let draft_id = draft_of(&h, content).await;
    let opened = call(&h, &token, "open_session", json!({ "draft_id": draft_id })).await;
    let sid = opened["session"].as_str().expect("session").to_owned();

    let stranger = h.process.mint_token_with_sub(TENANT, Role::Agent, STRANGER);
    let (mut sock, _) =
        tokio_tungstenite::connect_async(ws_request(&h.process.ws_url(), &stranger))
            .await
            .expect("ws connect");
    send_frame(&mut sock, json!({ "type": "hello", "session": sid })).await;
    let refused = recv_frame(&mut sock).await;
    assert_eq!(
        refused["code"],
        json!("forbidden"),
        "a stranger must not attach to a draft session: {refused}"
    );
    let message = refused["message"].as_str().unwrap_or_default();
    assert!(
        !message.contains(&draft_id),
        "the refusal must not name the draft: {message:?}"
    );

    sock.close(None).await.ok();
}

/// A client can only emit an op that MERGES if it shares the session document's
/// history — so `open_session` hands back the document's Loro snapshot.
///
/// Without it a client has the text (from the draft row) but not the history: an
/// op built on a locally-reconstructed document depends on ops the session has
/// never seen, so Loro buffers it as pending, the text does not move, and the
/// commit writes the bytes back unchanged. That failure is silent — `apply_op`
/// answers `ok` with an advanced `merged_version` — which is why it is pinned
/// here rather than left to a client to discover.
#[tokio::test]
async fn open_session_hands_back_a_snapshot_a_client_can_build_an_op_on() {
    let h = start().await;
    let token = author_token(&h);
    let seeded = "---\ntype: instance\nskill: note\nid: plan\n---\n# Plan\n\nseeded body.\n";
    let draft_id = draft_of(&h, seeded).await;

    let opened = call(&h, &token, "open_session", json!({ "draft_id": draft_id })).await;
    let snapshot = opened["snapshot"]
        .as_str()
        .expect("open_session must return the session document's snapshot");
    let sid = opened["session"].as_str().expect("session").to_owned();

    // Exactly what a client does: import the snapshot, edit, export only what
    // the session has not seen.
    let doc = LoroDoc::new();
    doc.import(&B64.decode(snapshot).expect("snapshot is base64"))
        .expect("a client must be able to import the snapshot");
    assert_eq!(
        doc.get_text("body").to_string(),
        seeded,
        "the snapshot must carry the draft's bytes, not an empty document"
    );
    let before = doc.oplog_vv();
    // Edited where a client edits: inside the body, leaving the document
    // parseable, so this test measures the snapshot mechanism and nothing else.
    let at = seeded.find("seeded body.").expect("seed body");
    doc.get_text("body")
        .insert(at, "EDITED-VIA-SNAPSHOT. ")
        .unwrap();
    doc.commit();
    let op = B64.encode(doc.export(ExportMode::updates(&before)).unwrap());

    let applied = raw(&h, &token, "apply_op", json!({ "session": sid, "op": op })).await;
    assert!(applied.get("error").is_none(), "apply_op: {applied}");
    let closed = call(
        &h,
        &token,
        "close_session",
        json!({ "session": sid, "commit": true }),
    )
    .await;
    assert_eq!(closed["ok"], json!(true), "close_session: {closed}");

    // The edit landed as an EDIT: the draft is the seeded text with the prefix,
    // not the seed twice over and not the seed untouched.
    let drafts = call(&h, &token, "list_drafts", json!({})).await;
    let held = drafts["drafts"]
        .as_array()
        .expect("drafts")
        .iter()
        .find(|d| d["draft_id"] == json!(draft_id))
        .and_then(|d| d["content"].as_str())
        .unwrap_or_else(|| panic!("the draft must still be listed: {drafts}"))
        .to_owned();
    assert_eq!(
        held,
        seeded.replace("seeded body.", "EDITED-VIA-SNAPSHOT. seeded body."),
        "the client's op must merge into the session's document"
    );
}

/// A page session needs the snapshot for the same reason.
#[tokio::test]
async fn a_page_session_also_hands_back_its_snapshot() {
    let h = start().await;
    let token = author_token(&h);
    let opened = call(&h, &token, "open_session", json!({ "page_id": PAGE })).await;
    let snapshot = opened["snapshot"].as_str().expect("snapshot");
    let doc = LoroDoc::new();
    doc.import(&B64.decode(snapshot).expect("base64"))
        .expect("import");
    assert_eq!(
        doc.get_text("body").to_string(),
        BASE,
        "a page session's snapshot must carry the page's stored bytes"
    );
}

/// A draft mid-edit is often not parseable for a moment. Its author must not
/// lose sight of it when that happens.
///
/// `may_see` decides a draft's visibility from its content's frontmatter, and
/// failing closed on unparseable content hid the draft from EVERYONE — so a
/// human editing live could make their own work vanish from the queue and from
/// review with one keystroke, unable to see it, diff it, promote it or discard
/// it until it happened to parse again.
#[tokio::test]
async fn an_unparseable_draft_is_still_visible_to_its_author() {
    let h = start().await;
    let token = author_token(&h);
    let seeded = "---\ntype: instance\nskill: note\nid: plan\n---\n# Plan\n\nvalid for now.\n";
    let draft_id = draft_of(&h, seeded).await;

    // Break the frontmatter the way a half-typed edit does, through the session.
    let opened = call(&h, &token, "open_session", json!({ "draft_id": draft_id })).await;
    let sid = opened["session"].as_str().expect("session").to_owned();
    let doc = LoroDoc::new();
    doc.import(
        &B64.decode(opened["snapshot"].as_str().expect("snapshot"))
            .unwrap(),
    )
    .expect("import");
    let before = doc.oplog_vv();
    doc.get_text("body").insert(0, "oops").unwrap();
    doc.commit();
    let op = B64.encode(doc.export(ExportMode::updates(&before)).unwrap());
    let applied = raw(&h, &token, "apply_op", json!({ "session": sid, "op": op })).await;
    assert!(applied.get("error").is_none(), "apply_op: {applied}");
    let closed = call(
        &h,
        &token,
        "close_session",
        json!({ "session": sid, "commit": true }),
    )
    .await;
    assert_eq!(closed["ok"], json!(true), "close_session: {closed}");

    let drafts = call(&h, &token, "list_drafts", json!({})).await;
    let row = drafts["drafts"]
        .as_array()
        .expect("drafts")
        .iter()
        .find(|d| d["draft_id"] == json!(draft_id))
        .cloned()
        .unwrap_or_else(|| panic!("the author must still see their own draft: {drafts}"));
    assert!(
        row["content"]
            .as_str()
            .unwrap_or_default()
            .starts_with("oops"),
        "…holding the unparseable bytes: {row}"
    );

    // Still decidable: the author can get rid of it.
    let discarded = call(
        &h,
        &token,
        "discard_draft",
        json!({ "draft_id": draft_id, "reason": "broke it" }),
    )
    .await;
    assert_eq!(discarded["ok"], json!(true), "discard_draft: {discarded}");

    // …and it stays invisible to everyone else, which is the half that must not
    // have loosened: an undeterminable ACL still fails closed for them.
    let stranger = h.process.mint_token_with_sub(TENANT, Role::Agent, STRANGER);
    let theirs = call(&h, &stranger, "list_drafts", json!({})).await;
    assert!(
        !theirs["drafts"]
            .as_array()
            .expect("drafts")
            .iter()
            .any(|d| d["draft_id"] == json!(draft_id)),
        "an unparseable draft must stay hidden from everyone else: {theirs}"
    );
}
