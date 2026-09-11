//! Held writes as a first-class escurel state (`create_draft` … `promote_draft`).
//!
//! escurel already declared `autonomy: review` and could approve exact bytes
//! (`base_sha256`, #354), but had nowhere to PUT a finished-and-not-yet-wanted
//! change. Consumers invented private conventions for it — heron modelled a
//! pending change as an ordinary instance of its own `proposal` skill — which
//! put consumer-shaped objects in the knowledge base and made "what is waiting
//! for me?" answerable only by that consumer.
//!
//! What must hold, and is pinned below:
//!
//! - a draft is NOT knowledge: invisible to `expand` until promoted;
//! - promotion is a CAS: a target that moved underneath refuses and the draft
//!   stays open to be re-drafted;
//! - a decision happens once;
//! - a draft that could never land must not be accepted, or a human reviews
//!   something unlandable.
//!
//! Real gateway, real DuckDB, real HTTP. No mocks.

use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const TENANT: &str = "stuttgart-ai";
const NOTE_SKILL: &str = "---\ntype: skill\nid: note\ndescription: A note.\n\
    visibility: public\n---\n# note\n";
const BASE: &str = "---\ntype: instance\nskill: note\nid: plan\n---\n# Plan\nv1 body.\n";
const PAGE: &str = "markdown/instances/note/plan.md";

fn sha(s: &str) -> String {
    format!("{:x}", Sha256::digest(s.as_bytes()))
}

fn body(id: &str, text: &str) -> String {
    format!("---\ntype: instance\nskill: note\nid: {id}\n---\n# Plan\n{text}\n")
}

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides::default(),
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("note", NOTE_SKILL)
                .instance("note", "plan", BASE)
                .done(),
        ),
    })
    .await
}

async fn call(p: &EscurelProcess, token: &str, tool: &str, args: Value) -> Value {
    let body: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": tool, "arguments": args },
        }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json");
    assert!(body.get("error").is_none(), "{tool} error: {body}");
    body["result"]["structuredContent"].clone()
}

/// The hash of the page's STORED bytes, as `expand` publishes it — the same
/// value `base_sha256` compares against. Compared by hash rather than by body
/// because `expand` returns a parsed page, and a reconstruction from its parts
/// would be asserting our own re-serialisation, not the stored bytes.
async fn page_sha(p: &EscurelProcess, token: &str, page_id: &str) -> Option<String> {
    let r = call(p, token, "expand", json!({ "page_id": page_id })).await;
    r["content_sha256"].as_str().map(str::to_owned)
}

/// A draft is held, not written — and the SAME bytes land the moment it is
/// promoted. The promote half is the positive control: without it, "not
/// visible" would also be satisfied by a draft that silently did nothing.
#[tokio::test]
async fn a_draft_is_invisible_until_promoted_and_lands_exactly_its_bytes() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);
    let proposed = body("plan", "DRAFTED body.");

    let created = call(
        &p,
        &token,
        "create_draft",
        json!({
            "target_page_id": PAGE,
            "content": proposed,
            "base_sha256": sha(BASE),
        }),
    )
    .await;
    assert_eq!(created["ok"], json!(true), "create_draft: {created}");
    let draft_id = created["draft"]["draft_id"]
        .as_str()
        .expect("draft_id")
        .to_owned();
    assert_eq!(
        created["draft"]["content_sha256"],
        json!(sha(&proposed)),
        "the approval's byte binding must be the draft's own hash: {created}"
    );
    // Whatever the issuer's subject is, the draft carries it — a reviewer's
    // first question is "who proposed this?", and a caller-supplied answer to
    // it is not evidence. Pinned as non-empty rather than to a literal, which
    // would only restate the test issuer's own constant.
    let author = created["draft"]["author"].as_str().unwrap_or_default();
    assert!(
        !author.is_empty(),
        "the draft must record its author: {created}"
    );

    // Not knowledge yet: the page still reads as it did before.
    assert_eq!(
        page_sha(&p, &token, PAGE).await,
        Some(sha(BASE)),
        "an unpromoted draft must not be readable as the page"
    );

    // ...and it IS waiting, where a human can find it.
    let waiting = call(&p, &token, "list_drafts", json!({})).await;
    assert_eq!(
        waiting["drafts"]
            .as_array()
            .expect("drafts array")
            .iter()
            .filter(|d| d["draft_id"] == json!(draft_id))
            .count(),
        1,
        "the open draft must be listed: {waiting}"
    );

    let promoted = call(&p, &token, "promote_draft", json!({ "draft_id": draft_id })).await;
    assert_eq!(promoted["ok"], json!(true), "promote_draft: {promoted}");
    assert_eq!(
        page_sha(&p, &token, PAGE).await,
        Some(sha(&proposed)),
        "promotion must land the reviewed bytes, byte for byte"
    );

    // A decided draft leaves the queue.
    let after = call(&p, &token, "list_drafts", json!({})).await;
    assert!(
        after["drafts"]
            .as_array()
            .expect("drafts array")
            .iter()
            .all(|d| d["draft_id"] != json!(draft_id)),
        "a promoted draft must not still be waiting: {after}"
    );
}

/// A target that moved under a pending draft conflicts, and the draft stays
/// OPEN — a lost queue entry is the failure that costs a human twice.
#[tokio::test]
async fn promotion_conflicts_when_the_target_moved_and_the_draft_survives() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);
    let proposed = body("plan", "DRAFTED against v1.");

    let created = call(
        &p,
        &token,
        "create_draft",
        json!({ "target_page_id": PAGE, "content": proposed, "base_sha256": sha(BASE) }),
    )
    .await;
    let draft_id = created["draft"]["draft_id"]
        .as_str()
        .expect("id")
        .to_owned();

    // Someone else edits the page after the draft was written.
    let moved = body("plan", "a concurrent edit.");
    let w = call(
        &p,
        &token,
        "update_page",
        json!({ "page_id": PAGE, "content": moved }),
    )
    .await;
    assert_eq!(w["ok"], json!(true), "concurrent write: {w}");

    let conflict = call(&p, &token, "promote_draft", json!({ "draft_id": draft_id })).await;
    assert_eq!(conflict["ok"], json!(false), "must conflict: {conflict}");
    assert_eq!(
        conflict["issues"][0]["code"],
        json!("conflict"),
        "{conflict}"
    );
    assert_eq!(
        page_sha(&p, &token, PAGE).await,
        Some(sha(&moved)),
        "a conflicted promotion must not have overwritten the concurrent edit"
    );

    let waiting = call(&p, &token, "list_drafts", json!({})).await;
    assert!(
        waiting["drafts"]
            .as_array()
            .expect("drafts")
            .iter()
            .any(|d| d["draft_id"] == json!(draft_id)),
        "a conflicted draft must stay open to be re-drafted: {waiting}"
    );

    // Positive control: re-drafted against the NEW head, the same approval
    // path lands. Without this the assertions above would also pass if
    // promotion were broken outright.
    let redraft = body("plan", "re-drafted against the concurrent edit.");
    let created2 = call(
        &p,
        &token,
        "create_draft",
        json!({ "target_page_id": PAGE, "content": redraft, "base_sha256": sha(&moved) }),
    )
    .await;
    let id2 = created2["draft"]["draft_id"]
        .as_str()
        .expect("id")
        .to_owned();
    let ok = call(&p, &token, "promote_draft", json!({ "draft_id": id2 })).await;
    assert_eq!(ok["ok"], json!(true), "re-draft must promote: {ok}");
    assert_eq!(page_sha(&p, &token, PAGE).await, Some(sha(&redraft)));
}

/// A draft that names a page nobody wrote must not be accepted: validation is
/// what makes "approved" mean "landable", and a human is the wrong component
/// to discover a dangling wikilink.
#[tokio::test]
async fn a_draft_that_could_never_land_is_refused_at_draft_time() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);

    let bad = body("plan", "see [[nosuchskill::invented-gmbh]] for details.");
    let refused = call(
        &p,
        &token,
        "create_draft",
        json!({ "target_page_id": PAGE, "content": bad, "base_sha256": sha(BASE) }),
    )
    .await;
    assert_eq!(refused["ok"], json!(false), "must refuse: {refused}");
    assert!(
        refused["issues"]
            .as_array()
            .expect("issues")
            .iter()
            .any(|i| {
                i["code"] == json!("dangling_wikilink") || i["code"] == json!("unknown_skill")
            }),
        "the refusal must name the reason: {refused}"
    );
    let waiting = call(&p, &token, "list_drafts", json!({})).await;
    assert!(
        waiting["drafts"].as_array().expect("drafts").is_empty(),
        "a refused draft must not be queued for review: {waiting}"
    );

    // Positive control, same shape minus the dangling link: the refusal above
    // is about the content, not about `create_draft` being inert.
    let good = body("plan", "no links at all.");
    let ok = call(
        &p,
        &token,
        "create_draft",
        json!({ "target_page_id": PAGE, "content": good, "base_sha256": sha(BASE) }),
    )
    .await;
    assert_eq!(ok["ok"], json!(true), "valid draft must be accepted: {ok}");
}

/// The create sentinel has to be TRUE. An empty `base_sha256` says "there is
/// no page here yet"; against a page that exists it is a draft born
/// un-approvable, and the only place that shows up is a human tapping Approve
/// and watching nothing happen.
///
/// Measured in the lab on 2026-09-09: seven runs, seven drafts, every one with
/// an empty base against a page written days earlier. Every approve refused
/// `conflict` — correctly — and the review feed just sat there.
#[tokio::test]
async fn an_empty_base_against_an_existing_page_is_refused_at_draft_time() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);

    let refused = call(
        &p,
        &token,
        "create_draft",
        json!({
            "target_page_id": PAGE,
            "content": body("plan", "drafted without reading the target."),
            "base_sha256": "",
        }),
    )
    .await;
    assert_eq!(refused["ok"], json!(false), "must refuse: {refused}");
    let issue = &refused["issues"][0];
    assert_eq!(issue["code"], json!("conflict"), "{refused}");
    assert_eq!(issue["location"], json!("base_sha256"), "{refused}");
    assert!(
        issue["message"]
            .as_str()
            .is_some_and(|m| m.contains("expand") && m.contains("content_sha256")),
        "the refusal must say what to do instead, while the agent can still \
         do it: {refused}"
    );
    let waiting = call(&p, &token, "list_drafts", json!({})).await;
    assert!(
        waiting["drafts"].as_array().expect("drafts").is_empty(),
        "a draft nobody could approve must not reach the review queue: {waiting}"
    );

    // CONTROL 1: the same empty base is CORRECT for a page that does not
    // exist — that is what the sentinel is for, and refusing it would break
    // every first write.
    let fresh = call(
        &p,
        &token,
        "create_draft",
        json!({
            "target_page_id": "markdown/instances/plan/brand-new.md",
            "content": body("brand-new", "the first draft of a page nobody wrote."),
            "base_sha256": "",
        }),
    )
    .await;
    assert_eq!(
        fresh["ok"],
        json!(true),
        "an empty base against a page that really is absent must be accepted: {fresh}"
    );

    // CONTROL 2: the real head hash against the existing page is accepted, so
    // the refusal above is about the SENTINEL and not about that page.
    let ok = call(
        &p,
        &token,
        "create_draft",
        json!({
            "target_page_id": PAGE,
            "content": body("plan", "drafted against what expand returned."),
            "base_sha256": sha(BASE),
        }),
    )
    .await;
    assert_eq!(ok["ok"], json!(true), "{ok}");
}

/// Promotion retires the event that produced the draft.
///
/// A `review` run leaves its event in the inbox on purpose — the run produced
/// no state, so the event is still waiting on a human. Promotion IS that
/// human. Until this, nothing said so: the event stayed unassigned for ever,
/// and a runner with an ephemeral ledger re-dispatched it on its next restart
/// and drafted the same page again. Measured in the lab on 2026-09-10: seven
/// approved emails came back as seven fresh drafts, several byte-identical to
/// the page that had just landed.
#[tokio::test]
async fn promoting_a_draft_takes_its_event_out_of_the_inbox() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);

    let captured = call(
        &p,
        &token,
        "capture_event",
        json!({
            "source": "manual",
            "mime": "text/plain",
            "label_skill": "note",
            "title": "a thing that happened",
            "body": "the body",
        }),
    )
    .await;
    let event_id = captured["event_id"].as_str().expect("event_id").to_owned();

    // Still waiting on a human: the run drafted, it did not write.
    let draft = call(
        &p,
        &token,
        "create_draft",
        json!({
            "target_page_id": PAGE,
            "content": body("plan", "folded in."),
            "base_sha256": sha(BASE),
            "event_id": event_id,
        }),
    )
    .await;
    assert_eq!(draft["ok"], json!(true), "{draft}");
    let in_inbox = |inbox: &Value| {
        inbox["events"]
            .as_array()
            .expect("events")
            .iter()
            .any(|e| e["event_id"] == json!(event_id))
    };
    assert!(
        in_inbox(&call(&p, &token, "list_inbox", json!({})).await),
        "a drafted event stays in the inbox until a human decides — that is \
         the gate, and it must not change"
    );

    call(
        &p,
        &token,
        "promote_draft",
        json!({ "draft_id": draft["draft"]["draft_id"] }),
    )
    .await;

    let inbox = call(&p, &token, "list_inbox", json!({})).await;
    assert!(
        !in_inbox(&inbox),
        "the promoted event must leave the inbox, or every restart drafts it \
         again: {inbox}"
    );
    let bound = call(&p, &token, "list_events", json!({ "event_id": event_id })).await;
    assert_eq!(
        bound["events"][0]["instance_page_id"],
        json!(PAGE),
        "…onto the page the draft wrote, not merely gone: {bound}"
    );
    assert_eq!(bound["events"][0]["status"], json!("processed"), "{bound}");
}

/// A draft with NO event behind it promotes exactly as before.
///
/// Not every draft comes from an event — a consultant editing a page by hand
/// makes one — and a promotion that required one would refuse the oldest path
/// this surface has.
#[tokio::test]
async fn a_draft_without_an_event_still_promotes() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);

    let draft = call(
        &p,
        &token,
        "create_draft",
        json!({
            "target_page_id": PAGE,
            "content": body("plan", "hand-written."),
            "base_sha256": sha(BASE),
        }),
    )
    .await;
    let promoted = call(
        &p,
        &token,
        "promote_draft",
        json!({ "draft_id": draft["draft"]["draft_id"] }),
    )
    .await;
    assert_eq!(promoted["ok"], json!(true), "{promoted}");
    assert_eq!(
        page_sha(&p, &token, PAGE).await,
        Some(sha(&body("plan", "hand-written.")))
    );
}

/// A decision is taken once. A discarded draft writes nothing, and neither a
/// second discard nor a promotion can resurrect it.
#[tokio::test]
async fn a_draft_is_decided_exactly_once() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);
    let proposed = body("plan", "REJECTED body.");

    let created = call(
        &p,
        &token,
        "create_draft",
        json!({ "target_page_id": PAGE, "content": proposed, "base_sha256": sha(BASE) }),
    )
    .await;
    let draft_id = created["draft"]["draft_id"]
        .as_str()
        .expect("id")
        .to_owned();

    let discarded = call(
        &p,
        &token,
        "discard_draft",
        json!({ "draft_id": draft_id, "reason": "not knowledge" }),
    )
    .await;
    assert_eq!(discarded["ok"], json!(true), "discard: {discarded}");
    assert_eq!(
        page_sha(&p, &token, PAGE).await,
        Some(sha(BASE)),
        "a discarded draft must never reach the page"
    );

    let again = call(&p, &token, "discard_draft", json!({ "draft_id": draft_id })).await;
    assert_eq!(again["ok"], json!(false), "second discard: {again}");

    let promote = call(&p, &token, "promote_draft", json!({ "draft_id": draft_id })).await;
    assert_eq!(
        promote["ok"],
        json!(false),
        "promote after discard: {promote}"
    );
    assert_eq!(
        promote["issues"][0]["code"],
        json!("already_decided"),
        "the answer must say which decision was taken: {promote}"
    );
    assert_eq!(
        page_sha(&p, &token, PAGE).await,
        Some(sha(BASE)),
        "a promotion after a discard must still write nothing"
    );
}

/// A draft may not stage a write its author could never make.
///
/// The whole point of a held write is that a DIFFERENT, more privileged
/// subject lands it later. If the ACL only ran at promotion, an unauthorised
/// author could queue an edit to someone else's record and have a reviewer's
/// authority carry it — the approver checks the content, not who was allowed
/// to write it. So the check runs at `create_draft`, with the same
/// `may_write_page` call, arguments and modes `update_page` uses.
mod acl {
    use super::*;
    use escurel_server::WriteAclMode;

    const ALICE: &str = "whatsapp:111";
    const BOB: &str = "whatsapp:222";
    const MEMBER_SKILL: &str = "---\ntype: skill\nid: community_member\n\
        description: A member.\nvisibility: owner\nowner_field: credential\n---\n# community_member\n";
    const ALICE_MEMBER: &str = "---\ntype: instance\nskill: community_member\nid: alice\n\
        credential: \"whatsapp:111\"\n---\n# Alice\n";
    const ALICE_PAGE: &str = "markdown/instances/community_member/alice.md";
    const ALICE_EDIT: &str = "---\ntype: instance\nskill: community_member\nid: alice\n\
        credential: \"whatsapp:111\"\n---\n# Alice\nEdited.\n";

    async fn start_acl() -> EscurelProcess {
        EscurelProcess::spawn(Opts {
            auth: AuthMode::TestIssuer,
            config_overrides: ConfigOverrides {
                write_acl: Some(WriteAclMode::Enforce),
                ..Default::default()
            },
            fixtures: Some(
                FixtureBuilder::new()
                    .tenant(TENANT)
                    .skill("community_member", MEMBER_SKILL)
                    .instance("community_member", "alice", ALICE_MEMBER)
                    .done(),
            ),
        })
        .await
    }

    #[tokio::test]
    async fn a_non_owner_cannot_draft_against_someone_elses_record() {
        let p = start_acl().await;
        let bob = p.mint_token_with_sub(TENANT, Role::Agent, BOB);

        let denied = call(
            &p,
            &bob,
            "create_draft",
            json!({ "target_page_id": ALICE_PAGE, "content": ALICE_EDIT }),
        )
        .await;
        assert_eq!(denied["ok"], json!(false), "bob must be refused: {denied}");
        assert_eq!(denied["issues"][0]["code"], json!("forbidden"), "{denied}");

        // Nothing was queued — a refusal that still leaves a row would put the
        // unauthorised edit in front of a reviewer anyway.
        let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);
        let waiting = call(&p, &alice, "list_drafts", json!({})).await;
        assert!(
            waiting["drafts"].as_array().expect("drafts").is_empty(),
            "a denied draft must not be queued: {waiting}"
        );

        // Positive control: the owner drafting the SAME bytes is accepted, so
        // the refusal above is about authority and not about the content or
        // about `create_draft` refusing everything under Enforce.
        let ok = call(
            &p,
            &alice,
            "create_draft",
            json!({ "target_page_id": ALICE_PAGE, "content": ALICE_EDIT }),
        )
        .await;
        assert_eq!(ok["ok"], json!(true), "the owner may draft: {ok}");
    }
}

/// The review queue is SCOPED, and it must be.
///
/// escurel's deployment model is one shared tenant with several people in
/// it, so an unfiltered `list_drafts` would show every consultant every
/// other consultant's held writes — content included, before anyone
/// approved anything. The draft carries no owner column on purpose: who may
/// see it is answered from the proposed CONTENT, the same question the page
/// it would become will answer, from the same source.
mod scope {
    use super::*;
    use escurel_server::WriteAclMode;

    const ALICE: &str = "whatsapp:111";
    const BOB: &str = "whatsapp:222";
    const MEMBER_SKILL: &str = "---\ntype: skill\nid: community_member\n\
        description: A member.\nvisibility: owner\nowner_field: credential\n---\n# community_member\n";
    const TALK_SKILL: &str = "---\ntype: skill\nid: talk\ndescription: A talk.\n\
        visibility: public\n---\n# talk\n";
    const ALICE_MEMBER: &str = "---\ntype: instance\nskill: community_member\nid: alice\n\
        credential: \"whatsapp:111\"\n---\n# Alice\n";
    const ALICE_PAGE: &str = "markdown/instances/community_member/alice.md";
    const ALICE_EDIT: &str = "---\ntype: instance\nskill: community_member\nid: alice\n\
        credential: \"whatsapp:111\"\n---\n# Alice\nPRIVATE edit.\n";
    const KEYNOTE: &str = "---\ntype: instance\nskill: talk\nid: keynote\n---\n# Keynote\n";
    const KEYNOTE_PAGE: &str = "markdown/instances/talk/keynote.md";
    const KEYNOTE_EDIT: &str =
        "---\ntype: instance\nskill: talk\nid: keynote\n---\n# Keynote\nPUBLIC edit.\n";

    async fn start_scoped() -> EscurelProcess {
        EscurelProcess::spawn(Opts {
            auth: AuthMode::TestIssuer,
            config_overrides: ConfigOverrides {
                write_acl: Some(WriteAclMode::Enforce),
                ..Default::default()
            },
            fixtures: Some(
                FixtureBuilder::new()
                    .tenant(TENANT)
                    .skill("community_member", MEMBER_SKILL)
                    .skill("talk", TALK_SKILL)
                    .instance("community_member", "alice", ALICE_MEMBER)
                    .instance("talk", "keynote", KEYNOTE)
                    .done(),
            ),
        })
        .await
    }

    #[tokio::test]
    async fn one_persons_held_write_is_not_in_another_persons_queue() {
        let p = start_scoped().await;
        let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);
        let bob = p.mint_token_with_sub(TENANT, Role::Agent, BOB);

        // Alice drafts against her own owner-private record.
        let private = call(
            &p,
            &alice,
            "create_draft",
            json!({ "target_page_id": ALICE_PAGE, "content": ALICE_EDIT }),
        )
        .await;
        assert_eq!(private["ok"], json!(true), "{private}");
        let private_id = private["draft"]["draft_id"]
            .as_str()
            .expect("id")
            .to_owned();

        // Bob sees nothing of it — not the row, not the content.
        let bobs_queue = call(&p, &bob, "list_drafts", json!({})).await;
        let raw = bobs_queue.to_string();
        assert!(
            !raw.contains(&private_id) && !raw.contains("PRIVATE edit"),
            "another person's held write must not appear in this queue: {bobs_queue}"
        );

        // ...and cannot decide it either. Denial reads as absence.
        let steal = call(&p, &bob, "promote_draft", json!({ "draft_id": private_id })).await;
        assert_eq!(steal["ok"], json!(false), "{steal}");
        assert_eq!(steal["issues"][0]["code"], json!("not_found"), "{steal}");
        let kill = call(&p, &bob, "discard_draft", json!({ "draft_id": private_id })).await;
        assert_eq!(kill["ok"], json!(false), "{kill}");

        // The draft survived Bob entirely: still open, still Alice's.
        let alices_queue = call(&p, &alice, "list_drafts", json!({})).await;
        assert!(
            alices_queue["drafts"]
                .as_array()
                .expect("drafts")
                .iter()
                .any(|d| d["draft_id"] == json!(private_id)),
            "the owner must still see their own open draft: {alices_queue}"
        );

        // Positive control: filtering is by ACL, not by author. A draft
        // against a PUBLIC page is visible to Bob even though someone else
        // wrote it — without this the assertions above would also pass if
        // `list_drafts` simply returned nothing to anyone.
        //
        // Drafted by an admin because a public / no-`owner_field` instance is
        // admin-write-only under `Enforce` (see `write_acl.rs`), and
        // `create_draft` runs exactly that ACL — which is itself the rule
        // this file's `acl` module pins.
        let admin = p.mint_token(TENANT, Role::Admin);
        let public = call(
            &p,
            &admin,
            "create_draft",
            json!({ "target_page_id": KEYNOTE_PAGE, "content": KEYNOTE_EDIT }),
        )
        .await;
        assert_eq!(public["ok"], json!(true), "{public}");
        let public_id = public["draft"]["draft_id"].as_str().expect("id").to_owned();
        let bobs_queue = call(&p, &bob, "list_drafts", json!({})).await;
        assert!(
            bobs_queue["drafts"]
                .as_array()
                .expect("drafts")
                .iter()
                .any(|d| d["draft_id"] == json!(public_id)),
            "a draft against a readable page must be in the queue: {bobs_queue}"
        );
    }
}

/// A draft whose frontmatter does not PARSE must be refused at draft time.
///
/// Found end to end, with a real model on real mail: an unquoted
/// `subject: Re: Workshop…` is invalid YAML. `validate` reported it,
/// `create_draft` accepted it anyway — the blocking set had no reason to
/// carry `frontmatter_parse`, because `update_page` fails on such content
/// before validation can matter — and the reviewer saw an ordinary card whose
/// promotion then died with an internal error naming a line number.
///
/// That is the exact failure the draft-time validation exists to prevent: a
/// human is the wrong component to discover a YAML quoting bug.
#[tokio::test]
async fn a_draft_whose_frontmatter_does_not_parse_is_refused() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);

    // The real shape, minimised: a colon in an unquoted scalar.
    let unparseable = "---\ntype: instance\nskill: note\nid: plan\n\
        subject: Re: Workshop Groz-Beckert am 29.07.2026\n---\n# Plan\nBody.\n";
    let refused = call(
        &p,
        &token,
        "create_draft",
        json!({ "target_page_id": PAGE, "content": unparseable }),
    )
    .await;
    assert_eq!(refused["ok"], json!(false), "must refuse: {refused}");
    assert!(
        refused["issues"]
            .as_array()
            .expect("issues")
            .iter()
            .any(|i| i["code"] == json!("frontmatter_parse")),
        "the refusal must name the reason: {refused}"
    );
    let waiting = call(&p, &token, "list_drafts", json!({})).await;
    assert!(
        waiting["drafts"].as_array().expect("drafts").is_empty(),
        "an unpromotable draft must never reach a reviewer: {waiting}"
    );

    // The same content through `update_page` now refuses TYPED as well,
    // instead of the internal error it used to raise. A caller can act on
    // `{ok:false, issues}`; it cannot act on -32603.
    let direct = call(
        &p,
        &token,
        "update_page",
        json!({ "page_id": PAGE, "content": unparseable }),
    )
    .await;
    assert_eq!(direct["ok"], json!(false), "{direct}");

    // Positive control: quote the value and both paths accept it, so the
    // refusals above are about the YAML and not about colons in a subject.
    let quoted = "---\ntype: instance\nskill: note\nid: plan\n\
        subject: \"Re: Workshop Groz-Beckert am 29.07.2026\"\n---\n# Plan\nBody.\n";
    let ok = call(
        &p,
        &token,
        "create_draft",
        json!({ "target_page_id": PAGE, "content": quoted, "base_sha256": sha(BASE) }),
    )
    .await;
    assert_eq!(ok["ok"], json!(true), "quoted frontmatter must pass: {ok}");
}

/// An instance page with no `skill:` is refused — it would be invisible.
///
/// Found end to end, and only visible from the client's side: an agent
/// drafted a `note` under `markdown/instances/note/…`, a human approved it,
/// the `about:` edge into the customer was really there — and
/// `list_instances --skill note` did not return it, because that query reads
/// the FRONTMATTER, not the path. The page was real, linked, and could never
/// appear in any catalogue view a reader browses.
///
/// `validate` reported nothing at all, which is why this is a validator fix
/// and not a corpus one: the page id looks like it declares the skill and
/// does not, so every author will make this mistake eventually.
#[tokio::test]
async fn an_instance_with_no_skill_is_refused_because_it_would_be_unbrowsable() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);

    let skill_less = "---\ntype: instance\nid: orphan\ntitle: \"A note\"\n---\n# A note\nBody.\n";
    let page = "markdown/instances/note/orphan.md";

    let refused = call(
        &p,
        &token,
        "create_draft",
        json!({ "target_page_id": page, "content": skill_less, "base_sha256": "" }),
    )
    .await;
    assert_eq!(refused["ok"], json!(false), "must refuse: {refused}");
    assert!(
        refused["issues"]
            .as_array()
            .expect("issues")
            .iter()
            .any(|i| {
                i["code"] == json!("frontmatter_required_key_missing")
                    && i["location"] == json!("frontmatter.skill")
            }),
        "the refusal must name the missing key: {refused}"
    );

    // The same content through `update_page` is refused too — one rule, not
    // a draft-only courtesy that direct writes walk past.
    let direct = call(
        &p,
        &token,
        "update_page",
        json!({ "page_id": page, "content": skill_less }),
    )
    .await;
    assert_eq!(direct["ok"], json!(false), "{direct}");

    // Positive control, and the assertion that gives the rule its meaning:
    // add `skill: note` and the page not only writes, it is FINDABLE by
    // type, which is the whole thing the missing key costs.
    let with_skill =
        "---\ntype: instance\nskill: note\nid: orphan\ntitle: \"A note\"\n---\n# A note\nBody.\n";
    let ok = call(
        &p,
        &token,
        "update_page",
        json!({ "page_id": page, "content": with_skill }),
    )
    .await;
    assert_eq!(ok["ok"], json!(true), "declared skill must write: {ok}");
    let listed = call(&p, &token, "list_instances", json!({ "skill_id": "note" })).await;
    assert!(
        listed["instances"]
            .as_array()
            .expect("instances")
            .iter()
            .any(|i| i["page_id"] == json!(page)),
        "with `skill:` the page is browsable by type: {listed}"
    );
}

/// An unquoted wikilink in frontmatter is REPORTED, and still written.
///
/// `about: [[customer::acme]]` parses as a nested YAML list rather than a
/// string. The obvious conclusion — that the edge is lost — is wrong, and
/// measuring it is what stopped this from shipping as a refusal: two
/// otherwise identical pages, one quoted and one not, both produce the
/// in-edge, because edge extraction reads the raw frontmatter text.
///
/// What the shape does change is what a CONSUMER reading the field as a
/// value receives. So it is a warning: reported, never blocking, and a page
/// that has been fine for a year does not suddenly refuse to save.
#[tokio::test]
async fn an_unquoted_wikilink_in_frontmatter_warns_but_still_writes() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);

    let unquoted = "---\ntype: instance\nskill: note\nid: plan\n\
        about: [[note::plan]]\n---\n# Plan\nBody.\n";
    let reported = call(&p, &token, "validate", json!({ "content": unquoted })).await;
    let issue = reported["issues"]
        .as_array()
        .expect("issues")
        .iter()
        .find(|i| i["code"] == json!("frontmatter_wikilink_unquoted"))
        .unwrap_or_else(|| panic!("must report it: {reported}"));
    assert_eq!(issue["severity"], json!("warning"), "{issue}");
    assert_eq!(issue["location"], json!("frontmatter.about"), "{issue}");
    assert!(
        issue["suggestion"]
            .as_str()
            .is_some_and(|s| s.contains("\"[[note::plan]]\"")),
        "the fix is one pair of quotes; say so: {issue}"
    );

    // ...and it writes. A warning that blocked would refuse pages that have
    // been correct for a year, to fix something nothing has lost.
    let ok = call(
        &p,
        &token,
        "create_draft",
        json!({ "target_page_id": PAGE, "content": unquoted, "base_sha256": sha(BASE) }),
    )
    .await;
    assert_eq!(ok["ok"], json!(true), "a warning must not block: {ok}");
}

/// A skill that REQUIRES `engagement`, so the draft path has something to hold
/// the line on. `update_page`'s looser rule is asserted against this same
/// skill below, which is the point of declaring it here.
const SCOPED_SKILL: &str = "---\ntype: skill\nid: scoped\ndescription: A scoped note.\n\
    visibility: public\nrequired_frontmatter: [id, skill, engagement]\n---\n# scoped\n";
const SCOPED_BASE: &str = "---\ntype: instance\nskill: scoped\nid: plan\n\
    engagement: engagement-groz\n---\n# Plan\nv1 body.\n";
const SCOPED_PAGE: &str = "markdown/instances/scoped/plan.md";

/// A draft missing a key its own skill declares required is refused.
///
/// Measured end to end with a real model on 2026-09-06: a Gemini run drafted a
/// page with no `engagement:`, `create_draft` answered `ok`, and the draft was
/// then invisible to every consultant — Heron scopes the review queue by
/// exactly that field and fails closed on its absence. The run still had turns
/// left and could have acted on a refusal.
///
/// The draft path holds a stricter line than `update_page` deliberately, and
/// the second half of this test pins that difference: an older corpus may
/// legitimately lack a declared key, and breaking those writes is a migration
/// rather than a fix. A draft has no such history.
#[tokio::test]
async fn a_draft_missing_a_key_its_skill_requires_is_refused() {
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides::default(),
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("scoped", SCOPED_SKILL)
                .instance("scoped", "plan", SCOPED_BASE)
                .done(),
        ),
    })
    .await;
    let token = p.mint_token(TENANT, Role::Agent);

    let unattributable = "---\ntype: instance\nskill: scoped\nid: plan\n---\n\
        # Plan\nDrafted without saying whose this is.\n";
    let refused = call(
        &p,
        &token,
        "create_draft",
        json!({ "target_page_id": SCOPED_PAGE, "content": unattributable }),
    )
    .await;
    assert_eq!(refused["ok"], json!(false), "must refuse: {refused}");
    assert!(
        refused["issues"]
            .as_array()
            .expect("issues")
            .iter()
            .any(|i| i["code"] == json!("frontmatter_required_key_missing")
                && i["location"] == json!("frontmatter.engagement")),
        "the refusal must name the missing field, so an agent can act on it: {refused}"
    );
    let waiting = call(&p, &token, "list_drafts", json!({})).await;
    assert!(
        waiting["drafts"].as_array().expect("drafts").is_empty(),
        "an unattributable draft must never reach a reviewer: {waiting}"
    );

    // POSITIVE CONTROL: the same content WITH the field is accepted, so the
    // refusal above is about the missing key and not about this skill or page.
    let attributed = "---\ntype: instance\nskill: scoped\nid: plan\n\
        engagement: engagement-groz\n---\n# Plan\nDrafted properly.\n";
    let ok = call(
        &p,
        &token,
        "create_draft",
        json!({
            "target_page_id": SCOPED_PAGE,
            "content": attributed,
            "base_sha256": sha(SCOPED_BASE),
        }),
    )
    .await;
    assert_eq!(ok["ok"], json!(true), "control: {ok}");

    // And `update_page` is UNCHANGED: the same missing key is not blocking
    // there. A stricter draft path is the decision; a stricter write path
    // would be a migration nobody asked for.
    let direct = call(
        &p,
        &token,
        "update_page",
        json!({ "page_id": SCOPED_PAGE, "content": unattributable }),
    )
    .await;
    assert_eq!(
        direct["ok"],
        json!(true),
        "update_page must keep its looser required_frontmatter rule: {direct}"
    );

    p.shutdown().await;
}

// ===========================================================================
// Who decided (heron's BR-HIL-6).
// ===========================================================================

/// A gateway that verified a human can say WHICH human approved.
///
/// A service in front of escurel authenticates a person and then writes with
/// its own credential, because escurel's write ACL matches on groups and a
/// person's minted bearer carries none. That is correct, and it had a cost
/// nobody had noticed: the decision was recorded against the SERVICE, so the
/// person who actually approved was stored nowhere. Measured on lab —
/// every page promoted from a draft read `last_written_by: heron-onbehalf`,
/// and no record named a consultant.
///
/// The assertion is that the stamp MOVES. A single approval recording
/// "consultant:alice" is equally satisfied by a field nobody writes and by a
/// constant, so two people decide two drafts and the record has to follow.
#[tokio::test]
async fn an_admin_can_name_the_human_who_decided_and_the_record_follows() {
    let p = start().await;
    let gateway = p.mint_token(TENANT, Role::Admin);

    let mut decided = Vec::new();
    for (page, who) in [("plan", "consultant:alice"), ("plan", "consultant:bob")] {
        let head = page_sha(&p, &gateway, &format!("markdown/instances/note/{page}.md")).await;
        let created = call(
            &p,
            &gateway,
            "create_draft",
            json!({
                "target_page_id": format!("markdown/instances/note/{page}.md"),
                "content": body(page, &format!("Revised for {who}.")),
                "base_sha256": head,
            }),
        )
        .await;
        let id = created["draft"]["draft_id"].as_str().expect("draft_id");
        let out = call(
            &p,
            &gateway,
            "promote_draft",
            json!({ "draft_id": id, "decided_by": who }),
        )
        .await;
        assert_eq!(out["ok"], json!(true), "promotion landed: {out}");
        decided.push(out["decided_by"].as_str().unwrap_or_default().to_owned());
    }

    assert_eq!(
        decided,
        vec!["consultant:alice".to_owned(), "consultant:bob".to_owned()],
        "the record must follow the person who decided, not the service that \
         wrote — a constant or an unwritten field satisfies one approval and \
         fails this pair"
    );
}

/// …and a caller may not vouch for someone else unless it is trusted to.
///
/// "This person approved it" is a claim ABOUT SOMEONE ELSE. A caller able to
/// make it about itself could write any name into the audit trail, which is
/// worse than no audit trail because it reads as one. Refused, not ignored:
/// a gateway that silently lost the attribution is how this went missing in
/// the first place.
#[tokio::test]
async fn an_ordinary_caller_may_not_vouch_for_another_subject() {
    let p = start().await;
    let agent = p.mint_token_with_sub(TENANT, Role::Agent, "agent:solo");

    let created = call(
        &p,
        &agent,
        "create_draft",
        json!({
            "target_page_id": "markdown/instances/note/plan.md",
            "content": body("plan", "Revised."),
            "base_sha256": page_sha(&p, &agent, "markdown/instances/note/plan.md").await,
        }),
    )
    .await;
    let id = created["draft"]["draft_id"]
        .as_str()
        .expect("draft_id")
        .to_owned();

    let refused: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {agent}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "promote_draft", "arguments": {
                "draft_id": &id, "decided_by": "consultant:alice",
            }},
        }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json");
    assert!(
        refused.get("error").is_some(),
        "a non-admin naming another subject must be refused: {refused}"
    );

    // Control: the same caller, the same draft, without the claim — so the
    // refusal above is about vouching and not about this caller's right to
    // promote at all.
    let out = call(&p, &agent, "promote_draft", json!({ "draft_id": &id })).await;
    assert_eq!(out["ok"], json!(true), "control: {out}");
    assert_eq!(
        out["decided_by"].as_str(),
        Some("agent:solo"),
        "with no claim, the decider is the caller itself: {out}"
    );
}
