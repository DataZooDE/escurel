//! Event-surface ACL over real HTTP: an event is visible only to the
//! principal it belongs to. A running gateway (TestIssuer auth) + real
//! Indexer; tokens carry the owning subject. No mocks, no LLM in the path.
//!
//! The rule under test (see `Indexer::may_read_event`):
//!
//!   * an event that names an instance follows THAT instance's read ACL;
//!   * an event that names none — an un-triaged inbox item — is visible
//!     only to the subject that captured it;
//!   * admin bypasses, and a legacy event with no recorded capturer stays
//!     ungated (compat, exactly like `may_access_chat`).
//!
//! Gated by `ESCUREL_EVENT_ACL` (off = legacy open event bus).

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "stuttgart-ai";
const ALICE: &str = "consultant:alice";
const BOB: &str = "consultant:bob";

const MEMBER_SKILL: &str = "---\ntype: skill\nid: community_member\n\
    description: A member.\nvisibility: owner\nowner_field: credential\n---\n# community_member\n";
const ALICE_MEMBER: &str = "---\ntype: instance\nskill: community_member\nid: alice\n\
    credential: \"consultant:alice\"\n---\n# Alice\n";
const BOB_MEMBER: &str = "---\ntype: instance\nskill: community_member\nid: bob\n\
    credential: \"consultant:bob\"\n---\n# Bob\n";

const ALICE_PAGE: &str = "markdown/instances/community_member/alice.md";

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("community_member", MEMBER_SKILL)
                .instance("community_member", "alice", ALICE_MEMBER)
                .instance("community_member", "bob", BOB_MEMBER)
                .done(),
        ),
        ..Default::default()
    })
    .await
}

/// Raw JSON-RPC call → the whole response body (so callers can inspect
/// `result` vs `error`).
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

fn capture_args(event_id: &str, body: &str) -> Value {
    json!({
        "event_id": event_id,
        "source": "heron-share",
        "mime": "text/plain",
        "label_skill": "community_member",
        "title": "unreviewed customer text",
        "body": body,
    })
}

/// The event ids visible in a caller's inbox listing.
fn inbox_ids(resp: &Value) -> Vec<String> {
    resp["result"]["structuredContent"]["events"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .filter_map(|e| e["event_id"].as_str().map(str::to_owned))
        .collect()
}

/// THE LEAK. Alice captures unreviewed customer text; Bob — a different,
/// non-admin, same-tenant caller — lists the inbox. He must not see it.
///
/// The positive control is in the same test on purpose: Alice MUST still
/// see her own event, or a fix that hides everything from everyone passes.
#[tokio::test]
async fn inbox_does_not_leak_another_callers_capture() {
    let p = start().await;
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let bob = p.mint_token_with_sub(TENANT, Role::Agent, BOB);

    let cap = call(
        &p,
        &alice,
        "capture_event",
        capture_args("EVT-ALICE-1", "alice's customer said something private"),
    )
    .await;
    assert!(cap.get("error").is_none(), "alice captures: {cap}");

    // Positive control: the owner CAN still see her own capture.
    let mine = call(&p, &alice, "list_inbox", json!({ "limit": 100 })).await;
    assert_eq!(
        inbox_ids(&mine),
        vec!["EVT-ALICE-1".to_owned()],
        "alice must see her own capture: {mine}"
    );

    // The leak: bob must NOT see it.
    let theirs = call(&p, &bob, "list_inbox", json!({ "limit": 100 })).await;
    assert_eq!(
        inbox_ids(&theirs),
        Vec::<String>::new(),
        "bob must not see alice's inbox event: {theirs}"
    );
}

/// The same leak on the by-event lookup: `list_events{event_id}` is a
/// direct read of one event and must be filtered identically.
#[tokio::test]
async fn get_event_by_id_does_not_leak_another_callers_capture() {
    let p = start().await;
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let bob = p.mint_token_with_sub(TENANT, Role::Agent, BOB);

    call(
        &p,
        &alice,
        "capture_event",
        capture_args("EVT-ALICE-2", "private"),
    )
    .await;

    let mine = call(&p, &alice, "list_events", json!({ "event_id": "EVT-ALICE-2" })).await;
    assert_eq!(
        inbox_ids(&mine),
        vec!["EVT-ALICE-2".to_owned()],
        "alice reads back her own event: {mine}"
    );

    let theirs = call(&p, &bob, "list_events", json!({ "event_id": "EVT-ALICE-2" })).await;
    assert_eq!(
        inbox_ids(&theirs),
        Vec::<String>::new(),
        "bob must not read alice's event by id: {theirs}"
    );
}

/// Claiming an event you may not read must be refused as **not found**,
/// never as forbidden — a distinguishable error is an existence oracle.
/// The positive control: alice's own claim of the same event succeeds.
#[tokio::test]
async fn assign_event_refuses_unreadable_event_as_not_found() {
    let p = start().await;
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let bob = p.mint_token_with_sub(TENANT, Role::Agent, BOB);

    call(
        &p,
        &alice,
        "capture_event",
        capture_args("EVT-ALICE-3", "private"),
    )
    .await;

    // Bob's claim of alice's event is indistinguishable from a claim of an
    // event that does not exist at all.
    let stolen = call(
        &p,
        &bob,
        "assign_event",
        json!({ "event_id": "EVT-ALICE-3", "instance_page_id": "markdown/instances/community_member/bob.md" }),
    )
    .await;
    let ghost = call(
        &p,
        &bob,
        "assign_event",
        json!({ "event_id": "EVT-NO-SUCH-EVENT", "instance_page_id": "markdown/instances/community_member/bob.md" }),
    )
    .await;
    assert!(
        stolen.get("error").is_some(),
        "bob's claim of alice's event must fail: {stolen}"
    );
    assert_eq!(
        stolen["error"]["code"], ghost["error"]["code"],
        "same code as a genuinely-missing event: {stolen} vs {ghost}"
    );
    assert_eq!(
        stolen["error"]["message"]
            .as_str()
            .unwrap()
            .replace("EVT-ALICE-3", "X"),
        ghost["error"]["message"]
            .as_str()
            .unwrap()
            .replace("EVT-NO-SUCH-EVENT", "X"),
        "the message must not distinguish hidden from absent: {stolen} vs {ghost}"
    );

    // Positive control: alice's own claim of that same event succeeds, so
    // the refusal above is about visibility and not about a broken CAS.
    let ok = call(
        &p,
        &alice,
        "assign_event",
        json!({ "event_id": "EVT-ALICE-3", "instance_page_id": ALICE_PAGE }),
    )
    .await;
    assert!(ok.get("error").is_none(), "alice claims her own: {ok}");
}
