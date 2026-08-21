//! A live session must start from the page it is opened on (#421).
//!
//! `LiveDoc::open` hydrates from the CRDT backend — snapshot plus replayed ops
//! — and a page written through `update_page` has neither. So a session opened
//! on a page full of content starts as an **empty document**, and
//! `close_session {commit: true}` writes that document's text as the *whole
//! page*.
//!
//! Today the damage is usually masked: the doc's text is whatever ops the room
//! sent, which rarely parses as a page, and the commit fails on
//! `missing frontmatter`. That is validation catching it, not design — a room
//! whose edits happen to form a valid document replaces the page instead, and
//! the pre-existing content is gone with no error anywhere.
//!
//! It also means every device joining a live session sees an empty document
//! where the page's content should be, which is the same bug read from the
//! other end.
//!
//! Real gateway, real DuckDB indexer, real CRDT backend, real OIDC. No mocks.

use std::sync::Arc;

use duckdb::Connection;
use escurel_crdt::{CrdtBackend, DuckdbCrdtBackend};
use escurel_index::Migrator;
use escurel_test_support::crdt_testkit::loro_insert_op;
use escurel_test_support::{
    AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, WriteAclMode,
};
use serde_json::{Value, json};
use tokio::sync::Mutex;

const TENANT: &str = "stuttgart-ai";

const NOTE_SKILL: &str = "---\ntype: skill\nid: note\ndescription: A note.\n\
    acl:\n  read: [public]\n  create: [team-red]\n  update: [team-red]\n---\n# note\n";

/// The page a room opens. Its body is the thing that must survive.
const RED_NOTE: &str = "---\ntype: instance\nskill: note\nid: red-1\nteam: team-red\n---\n\
    # Red\n\nThe minutes of a long meeting nobody wants to lose.\n";
const RED_PAGE: &str = "markdown/instances/note/red-1.md";

/// What a client might send that happens to be a valid page. This is the whole
/// point of the fixture: a well-formed document does NOT trip the validator, so
/// nothing stands between it and the stored page.
const PLAUSIBLE: &str = "---\ntype: instance\nskill: note\nid: red-1\nteam: team-red\n---\n\
    # Red\n\nJust this line.\n";

async fn start() -> EscurelProcess {
    let conn = Connection::open_in_memory().expect("duckdb");
    Migrator::up(&conn).expect("migrations");
    let backend: Arc<dyn CrdtBackend> =
        Arc::new(DuckdbCrdtBackend::new(Arc::new(Mutex::new(conn))));
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            live_crdt: true,
            crdt_backend: Some(backend),
            write_acl: Some(WriteAclMode::Enforce),
            ..Default::default()
        },
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("note", NOTE_SKILL)
                .instance("note", "red-1", RED_NOTE)
                .done(),
        ),
    })
    .await
}

async fn call(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": args },
        }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json")
}

/// **The data loss.** A room's edit must not replace the page it was editing.
#[tokio::test]
async fn committing_a_session_does_not_replace_the_page_with_the_edit_alone() {
    let p = start().await;
    let member = p.mint_token_with_groups(TENANT, "member-1", &["team-red"], false);

    let before = call(&p, &member, "expand", json!({ "page_id": RED_PAGE })).await
        ["result"]["structuredContent"]["body"]
        .as_str()
        .expect("the page must exist to begin with")
        .to_owned();
    assert!(
        before.contains("The minutes of a long meeting"),
        "premise: the page starts with content: {before:?}"
    );

    let session = call(&p, &member, "open_session", json!({ "page_id": RED_PAGE })).await["result"]
        ["structuredContent"]["session"]
        .as_str()
        .expect("session")
        .to_owned();

    // One op carrying a document that VALIDATES. Nothing downstream will
    // object to it, which is exactly the problem.
    let applied = call(
        &p,
        &member,
        "apply_op",
        json!({ "session": session, "op": loro_insert_op(PLAUSIBLE) }),
    )
    .await;
    assert!(
        applied.get("error").is_none(),
        "the op itself is well-formed and must apply: {applied}"
    );

    let closed = call(
        &p,
        &member,
        "close_session",
        json!({ "session": session, "commit": true }),
    )
    .await;
    assert!(
        closed.get("error").is_none(),
        "the commit must succeed — a failure here would be validation masking \
         the defect rather than the defect being absent: {closed}"
    );

    let after = call(&p, &member, "expand", json!({ "page_id": RED_PAGE })).await
        ["result"]["structuredContent"]["body"]
        .as_str()
        .expect("the page must still exist")
        .to_owned();

    assert!(
        after.contains("The minutes of a long meeting"),
        "the page's content must survive a session commit. A session that \
         starts empty writes only what the room typed, and everything written \
         before the room opened is gone with no error: {after:?}"
    );

    p.shutdown().await;
}

/// The same defect from the joining device's side: a session opened on a page
/// with content must present that content, or every participant sees a blank
/// document where the minutes should be.
#[tokio::test]
async fn a_session_opened_on_a_page_starts_from_its_content() {
    let p = start().await;
    let member = p.mint_token_with_groups(TENANT, "member-1", &["team-red"], false);

    let session = call(&p, &member, "open_session", json!({ "page_id": RED_PAGE })).await["result"]
        ["structuredContent"]["session"]
        .as_str()
        .expect("session")
        .to_owned();

    // Applying an empty-ish op and committing is the observable proxy for
    // "what does the document hold", since the doc's text is not readable over
    // MCP. A hydrated session commits the page back unchanged.
    let closed = call(
        &p,
        &member,
        "close_session",
        json!({ "session": session, "commit": true }),
    )
    .await;
    assert!(closed.get("error").is_none(), "close: {closed}");

    let after = call(&p, &member, "expand", json!({ "page_id": RED_PAGE })).await
        ["result"]["structuredContent"]["body"]
        .as_str()
        .expect("page")
        .to_owned();
    assert!(
        after.contains("The minutes of a long meeting"),
        "an untouched session must commit the page back unchanged: {after:?}"
    );

    // POSITIVE CONTROL for the fixture itself: a member really can write here,
    // so the assertions above are about hydration and not about a page nobody
    // could have changed anyway.
    let wrote = call(
        &p,
        &member,
        "update_page",
        json!({ "page_id": RED_PAGE, "content": RED_NOTE }),
    )
    .await;
    assert_eq!(
        wrote["result"]["structuredContent"]["ok"],
        json!(true),
        "control: this member may write this page: {wrote}"
    );

    p.shutdown().await;
}

/// **A stranger must not be able to discard someone else's session (#425).**
///
/// `close_session {commit: true}` re-checks the write ACL. `{commit: false}`
/// checked nothing at all, deliberately: a caller whose grant was revoked
/// mid-session must still be able to abandon their own work rather than leave
/// the page wedged until the idle TTL.
///
/// That exemption did not distinguish **the opener** from **anyone holding the
/// session id**. A consultant on another engagement could terminate a live
/// workshop in a room they have no relationship to. No content is read, so it
/// is not a disclosure — it is a cross-engagement denial of service, and in a
/// room the symptom is the facilitator's session dying mid-sentence.
///
/// Found by an AI review of Heron's P5 work (2026-08-21), then reproduced
/// here: bob, on `team-blue`, closed a `team-red` session and its owner could
/// immediately reopen the page — proving the first session was destroyed.
#[tokio::test]
async fn an_outsider_cannot_discard_someone_elses_session() {
    let p = start().await;
    let member = p.mint_token_with_groups(TENANT, "member-1", &["team-red"], false);
    let outsider = p.mint_token_with_groups(TENANT, "outsider-1", &["team-blue"], false);

    let session = call(&p, &member, "open_session", json!({ "page_id": RED_PAGE })).await["result"]
        ["structuredContent"]["session"]
        .as_str()
        .expect("session")
        .to_owned();

    let discarded = call(
        &p,
        &outsider,
        "close_session",
        json!({ "session": session, "commit": false }),
    )
    .await;
    assert!(
        discarded["result"]["structuredContent"]["ok"] != json!(true),
        "an outsider must not discard a session on a page they may not write: \
         {discarded}"
    );

    // The session must SURVIVE. If it were destroyed, the page is free and a
    // fresh open succeeds — which is exactly how the defect was detected, and
    // is what the refusal above must prevent rather than merely report.
    let reopened = call(&p, &member, "open_session", json!({ "page_id": RED_PAGE })).await;
    assert!(
        reopened["result"]["structuredContent"]["session"]
            .as_str()
            .is_none(),
        "the owner's session must still hold the page — a second open \
         succeeding means the outsider's discard took effect: {reopened}"
    );

    // POSITIVE CONTROL: the owner can still discard their own session, which
    // is the property the exemption exists for. A fix that simply required
    // the write ACL here would strand a caller whose grant was revoked
    // mid-session, leaving the page wedged until the idle TTL.
    let own = call(
        &p,
        &member,
        "close_session",
        json!({ "session": session, "commit": false }),
    )
    .await;
    assert_eq!(
        own["result"]["structuredContent"]["ok"],
        json!(true),
        "control: the opener must still be able to abandon their own work: {own}"
    );
}
