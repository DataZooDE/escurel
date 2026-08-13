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

use escurel_test_support::{
    AuthMode, ConfigOverrides, EscurelProcess, EventAclMode, FixtureBuilder, Opts, Role,
};
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
    start_with(EventAclMode::Enforce).await
}

async fn start_with(mode: EventAclMode) -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            event_acl: Some(mode),
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

    let mine = call(
        &p,
        &alice,
        "list_events",
        json!({ "event_id": "EVT-ALICE-2" }),
    )
    .await;
    assert_eq!(
        inbox_ids(&mine),
        vec!["EVT-ALICE-2".to_owned()],
        "alice reads back her own event: {mine}"
    );

    let theirs = call(
        &p,
        &bob,
        "list_events",
        json!({ "event_id": "EVT-ALICE-2" }),
    )
    .await;
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

/// The compare-and-set the ACL pre-check must not have replaced: an event
/// already claimed by another instance is still an `already assigned`
/// conflict for a caller who CAN read it, and re-claiming to the same
/// instance is still the idempotent `Ok` the runner's recovery path needs.
#[tokio::test]
async fn cas_outcomes_survive_the_acl_precheck() {
    let p = start().await;
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);

    call(
        &p,
        &alice,
        "capture_event",
        capture_args("EVT-ALICE-4", "private"),
    )
    .await;
    let first = call(
        &p,
        &alice,
        "assign_event",
        json!({ "event_id": "EVT-ALICE-4", "instance_page_id": ALICE_PAGE }),
    )
    .await;
    assert!(first.get("error").is_none(), "first claim wins: {first}");

    // Idempotent re-run: same event, same instance → still Ok.
    let again = call(
        &p,
        &alice,
        "assign_event",
        json!({ "event_id": "EVT-ALICE-4", "instance_page_id": ALICE_PAGE }),
    )
    .await;
    assert!(
        again.get("error").is_none(),
        "re-run is idempotent: {again}"
    );

    // A different target for an already-processed event is still the
    // typed conflict, reported as such and NOT collapsed into not-found.
    let conflict = call(
        &p,
        &alice,
        "assign_event",
        json!({ "event_id": "EVT-ALICE-4", "instance_page_id": "markdown/instances/community_member/bob.md" }),
    )
    .await;
    let msg = conflict["error"]["message"].as_str().unwrap_or_default();
    assert!(
        msg.contains("already"),
        "the already-claimed outcome is preserved, got: {conflict}"
    );
}

/// Limb 2 of the rule: once an event is FILED into an instance, it is as
/// visible as that instance — no more (alice's private member page hides
/// it from bob) and no less (alice still reads her own history).
#[tokio::test]
async fn a_filed_event_follows_its_instances_acl() {
    let p = start().await;
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let bob = p.mint_token_with_sub(TENANT, Role::Agent, BOB);

    call(
        &p,
        &alice,
        "capture_event",
        capture_args("EVT-ALICE-5", "private"),
    )
    .await;
    call(
        &p,
        &alice,
        "assign_event",
        json!({ "event_id": "EVT-ALICE-5", "instance_page_id": ALICE_PAGE }),
    )
    .await;

    let mine = call(
        &p,
        &alice,
        "list_events",
        json!({ "instance_page_id": ALICE_PAGE }),
    )
    .await;
    assert_eq!(
        inbox_ids(&mine),
        vec!["EVT-ALICE-5".to_owned()],
        "alice reads her own instance's event history: {mine}"
    );

    let theirs = call(
        &p,
        &bob,
        "list_events",
        json!({ "instance_page_id": ALICE_PAGE }),
    )
    .await;
    assert_eq!(
        inbox_ids(&theirs),
        Vec::<String>::new(),
        "bob must not read the history of alice's private instance: {theirs}"
    );
}

/// Admin bypasses — which is what keeps an operator dashboard, and a
/// worker draining the shared inbox under a service token, working.
#[tokio::test]
async fn admin_sees_every_event() {
    let p = start().await;
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let bob = p.mint_token_with_sub(TENANT, Role::Agent, BOB);
    let admin = p.mint_token(TENANT, Role::Admin);

    call(&p, &alice, "capture_event", capture_args("EVT-A", "a")).await;
    call(&p, &bob, "capture_event", capture_args("EVT-B", "b")).await;

    let all = call(&p, &admin, "list_inbox", json!({ "limit": 100 })).await;
    let mut ids = inbox_ids(&all);
    ids.sort();
    assert_eq!(
        ids,
        vec!["EVT-A".to_owned(), "EVT-B".to_owned()],
        "admin drains the whole inbox: {all}"
    );
}

/// The rollout gate's off position is the shipped default and keeps the
/// legacy open event bus — so the fix cannot break an existing deployment
/// until its operator opts in.
#[tokio::test]
async fn off_mode_leaves_the_event_bus_open() {
    let p = start_with(EventAclMode::Off).await;
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let bob = p.mint_token_with_sub(TENANT, Role::Agent, BOB);

    call(
        &p,
        &alice,
        "capture_event",
        capture_args("EVT-ALICE-6", "private"),
    )
    .await;
    let theirs = call(&p, &bob, "list_inbox", json!({ "limit": 100 })).await;
    assert_eq!(
        inbox_ids(&theirs),
        vec!["EVT-ALICE-6".to_owned()],
        "off mode keeps the event bus open: {theirs}"
    );
}

/// `capture_event` is idempotent on `event_id` and returns the STORED
/// (first-writer) event — which makes a guessed id a read. Heron's
/// idempotency key is client-chosen, so this is a guess a client can make.
/// A caller who may not see the stored event must get its own submission
/// back, indistinguishable from a first capture, and never the other
/// caller's title/body.
#[tokio::test]
async fn idempotent_recapture_does_not_read_back_another_callers_event() {
    let p = start().await;
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let bob = p.mint_token_with_sub(TENANT, Role::Agent, BOB);

    call(
        &p,
        &alice,
        "capture_event",
        capture_args("EVT-COLLIDE", "alice's customer said something private"),
    )
    .await;

    // Bob guesses the id. He must not learn what alice stored under it.
    let guess = call(
        &p,
        &bob,
        "capture_event",
        capture_args("EVT-COLLIDE", "bob's own text"),
    )
    .await;
    let ev = &guess["result"]["structuredContent"];
    assert_eq!(ev["event_id"], json!("EVT-COLLIDE"), "id echoes: {guess}");
    assert_eq!(ev["body"], json!("bob's own text"), "bob's own: {guess}");
    assert!(
        !guess.to_string().contains("something private"),
        "alice's stored text must not come back to bob: {guess}"
    );

    // Positive control: alice's OWN idempotent re-capture still reads back
    // the authoritative first-writer row, which is what makes a retry
    // converge instead of forking.
    let retry = call(
        &p,
        &alice,
        "capture_event",
        capture_args("EVT-COLLIDE", "a retry with drifted text"),
    )
    .await;
    assert_eq!(
        retry["result"]["structuredContent"]["body"],
        json!("alice's customer said something private"),
        "first-writer-wins is preserved for the owner: {retry}"
    );

    // And the collision did not plant anything in bob's inbox.
    let theirs = call(&p, &bob, "list_inbox", json!({ "limit": 100 })).await;
    assert_eq!(
        inbox_ids(&theirs),
        Vec::<String>::new(),
        "bob's inbox stays empty: {theirs}"
    );
}

/// The stamp is server-owned: a caller who supplies its own `captured_by`
/// in `provenance` does not get to name someone else as the capturer (and
/// so cannot plant an event in another consultant's inbox), and the rest
/// of its provenance survives the stamp.
#[tokio::test]
async fn caller_supplied_captured_by_is_overwritten() {
    let p = start().await;
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let bob = p.mint_token_with_sub(TENANT, Role::Agent, BOB);

    let mut args = capture_args("EVT-FORGED", "planted");
    args["provenance"] = json!({ "captured_by": BOB, "runner": { "depth": 0 } });
    let cap = call(&p, &alice, "capture_event", args).await;
    let prov = &cap["result"]["structuredContent"]["provenance"];
    assert_eq!(prov["captured_by"], json!(ALICE), "stamp wins: {cap}");
    assert_eq!(
        prov["runner"]["depth"],
        json!(0),
        "the caller's own provenance survives: {cap}"
    );

    let theirs = call(&p, &bob, "list_inbox", json!({ "limit": 100 })).await;
    assert_eq!(
        inbox_ids(&theirs),
        Vec::<String>::new(),
        "bob cannot be made the owner of alice's capture: {theirs}"
    );
}
