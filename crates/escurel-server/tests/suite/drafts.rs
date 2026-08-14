//! Draft versions — a held write that has not landed yet (CR-2, #354).
//!
//! **A draft is not a separate object. It is a version whose status is draft.**
//! Escurel already has versions (`v<hlc>`, `base_version`, `require_exact_base`),
//! so a proposed change to an existing page is simply the next version,
//! unpublished. Nothing new to reap, no diff to store, no second identity, and
//! the stale-base guard is the one that already exists.
//!
//! It completes a promise the codebase already makes. `Autonomy::Review` is
//! documented as *"the write is held for human approval before it lands"* and
//! nothing enforced it: `ESCUREL_AUTONOMY_LINT` validates the value and the
//! write lands anyway. Here, `autonomy: review` finally means what it says.
//!
//! **One verb, made richer.** No `propose_page`, no `apply_proposal`, no
//! `publish_draft` — `update_page` writes the draft, and `update_page` with
//! `approve` promotes it. The ACL keeps one concept too: `update`.
//!
//! Real `escurel-server`, real Indexer, real DuckDB, real CRDT backend, real
//! JWKS, over real HTTP. No mocks.

use escurel_test_support::{
    AuthMode, ConfigOverrides, DraftMode, EscurelProcess, FixtureBuilder, Opts, Role,
};
use serde_json::{Value, json};

const TENANT: &str = "stuttgart-ai";

/// Held for a human. This is the whole configuration — no new key.
const REVIEW_SKILL: &str = "---\ntype: skill\nid: triage\ndescription: Triage.\n\
    autonomy: review\n---\n# triage\n";
/// The control. Its writes must keep landing immediately, or this feature has
/// changed the behaviour of every page that never asked for a gate.
const AUTO_SKILL: &str = "---\ntype: skill\nid: note\ndescription: A note.\n\
    autonomy: auto\n---\n# note\n";

const TRIAGE_PAGE: &str = "markdown/instances/triage/t-1.md";
const NOTE_PAGE: &str = "markdown/instances/note/n-1.md";

fn published(skill: &str, id: &str, body: &str) -> String {
    format!("---\ntype: instance\nskill: {skill}\nid: {id}\n---\n# {id}\n{body}\n")
}

fn fixtures() -> FixtureBuilder {
    FixtureBuilder::new()
        .tenant(TENANT)
        .skill("triage", REVIEW_SKILL)
        .skill("note", AUTO_SKILL)
        .instance(
            "triage",
            "t-1",
            published("triage", "t-1", "Original.").as_str(),
        )
        .instance(
            "note",
            "n-1",
            published("note", "n-1", "Original.").as_str(),
        )
        .done()
}

async fn start(mode: DraftMode) -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(fixtures()),
        config_overrides: ConfigOverrides {
            drafts: Some(mode),
            // Versions are the whole mechanism here: without a CRDT backend
            // escurel tracks none at all, and a draft would have nowhere to be.
            live_crdt: true,
            ..Default::default()
        },
    })
    .await
}

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
    let body: Value = resp.json().await.unwrap();
    assert!(body.get("error").is_none(), "{name} error: {body}");
    body["result"]["structuredContent"].clone()
}

/// The published body every ordinary reader sees.
async fn body_of(p: &EscurelProcess, token: &str, page: &str) -> String {
    call(p, token, "expand", json!({ "page_id": page })).await["body"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

// =========================================================================
// The gate
// =========================================================================

/// `autonomy: review` holds the write, and the control proves it is the skill
/// that decides rather than the server having stopped writing.
#[tokio::test]
async fn a_write_to_a_review_skill_is_held_as_a_draft() {
    let p = start(DraftMode::Enforce).await;
    let agent = p.mint_token(TENANT, Role::Agent);

    let out = call(
        &p,
        &agent,
        "update_page",
        json!({
            "page_id": TRIAGE_PAGE,
            "content": published("triage", "t-1", "Proposed."),
        }),
    )
    .await;

    assert_eq!(out["ok"], json!(true), "the write itself succeeds: {out}");
    assert_eq!(
        out["draft"],
        json!(true),
        "…and reports that it was HELD, or a caller cannot tell a proposal \
         from a landed change: {out}"
    );
    assert!(
        out["new_version"].as_str().is_some_and(|v| !v.is_empty()),
        "the draft must be identified by a version, since that is what it \
         IS — and the approver has to name it: {out}"
    );

    // The reader still sees the published page. This is the requirement.
    assert!(
        body_of(&p, &agent, TRIAGE_PAGE).await.contains("Original."),
        "a held write must not be visible to an ordinary read"
    );

    // CONTROL — `autonomy: auto` still lands immediately. Without it this
    // passes against a server that has simply stopped writing anything.
    let out = call(
        &p,
        &agent,
        "update_page",
        json!({
            "page_id": NOTE_PAGE,
            "content": published("note", "n-1", "Landed."),
        }),
    )
    .await;
    assert_eq!(out["ok"], json!(true), "{out}");
    assert_ne!(
        out["draft"],
        json!(true),
        "control: an auto skill must NOT hold its writes: {out}"
    );
    assert!(
        body_of(&p, &agent, NOTE_PAGE).await.contains("Landed."),
        "control: an auto write must be visible immediately"
    );

    p.shutdown().await;
}

/// A draft is readable — but only when asked for.
///
/// Invisible-by-default is the point; unreadable would make it useless, since
/// the whole purpose is that a human reviews it before it lands.
#[tokio::test]
async fn a_draft_is_returned_only_when_it_is_asked_for() {
    let p = start(DraftMode::Enforce).await;
    let agent = p.mint_token(TENANT, Role::Agent);

    call(
        &p,
        &agent,
        "update_page",
        json!({
            "page_id": TRIAGE_PAGE,
            "content": published("triage", "t-1", "Proposed."),
        }),
    )
    .await;

    let plain = call(&p, &agent, "expand", json!({ "page_id": TRIAGE_PAGE })).await;
    assert!(
        plain["body"]
            .as_str()
            .unwrap_or_default()
            .contains("Original."),
        "the default read is the published version: {plain}"
    );

    let asked = call(
        &p,
        &agent,
        "expand",
        json!({ "page_id": TRIAGE_PAGE, "include_drafts": true }),
    )
    .await;
    assert!(
        asked["body"]
            .as_str()
            .unwrap_or_default()
            .contains("Proposed."),
        "asked for, the draft must come back — a proposal nobody can read \
         cannot be reviewed: {asked}"
    );
    assert_eq!(
        asked["draft"],
        json!(true),
        "…and must say that it is one, or a reviewer cannot tell what they \
         are looking at: {asked}"
    );

    p.shutdown().await;
}

// =========================================================================
// Approval — the same verb, richer
// =========================================================================

/// Approving promotes the draft, and what lands is the draft's bytes.
#[tokio::test]
async fn approving_a_draft_publishes_exactly_what_was_drafted() {
    let p = start(DraftMode::Enforce).await;
    let agent = p.mint_token(TENANT, Role::Agent);
    let human = p.mint_token_with_groups(TENANT, "consultant:alice", &[], true);

    let drafted = call(
        &p,
        &agent,
        "update_page",
        json!({
            "page_id": TRIAGE_PAGE,
            "content": published("triage", "t-1", "Proposed."),
        }),
    )
    .await;
    let version = drafted["new_version"].as_str().expect("version").to_owned();

    let approved = call(
        &p,
        &human,
        "update_page",
        json!({ "page_id": TRIAGE_PAGE, "approve": version }),
    )
    .await;
    assert_eq!(approved["ok"], json!(true), "{approved}");

    assert!(
        body_of(&p, &human, TRIAGE_PAGE).await.contains("Proposed."),
        "what was approved must be what shipped — re-rendering at approval \
         time would let the reviewed diff drift from what lands"
    );

    p.shutdown().await;
}

/// **An approval by the author is not a review.**
///
/// Maker/checker, enforced by the store from a fact it already records
/// (`last_written_by`, #357) rather than re-implemented by every consumer.
#[tokio::test]
async fn the_principal_who_drafted_may_not_approve_their_own_draft() {
    let p = start(DraftMode::Enforce).await;
    let agent = p.mint_token(TENANT, Role::Agent);
    let human = p.mint_token_with_groups(TENANT, "consultant:alice", &[], true);

    let drafted = call(
        &p,
        &agent,
        "update_page",
        json!({
            "page_id": TRIAGE_PAGE,
            "content": published("triage", "t-1", "Proposed."),
        }),
    )
    .await;
    let version = drafted["new_version"].as_str().expect("version").to_owned();

    let refused = call(
        &p,
        &agent,
        "update_page",
        json!({ "page_id": TRIAGE_PAGE, "approve": version.clone() }),
    )
    .await;
    assert_eq!(
        refused["ok"],
        json!(false),
        "the drafter must not be able to approve their own draft: {refused}"
    );
    assert!(
        body_of(&p, &agent, TRIAGE_PAGE).await.contains("Original."),
        "and nothing may have landed"
    );

    // POSITIVE CONTROL — a different principal approves the same draft. This
    // is what makes the refusal about identity rather than about approval
    // being broken.
    let approved = call(
        &p,
        &human,
        "update_page",
        json!({ "page_id": TRIAGE_PAGE, "approve": version }),
    )
    .await;
    assert_eq!(approved["ok"], json!(true), "control: {approved}");
    assert!(body_of(&p, &human, TRIAGE_PAGE).await.contains("Proposed."));

    p.shutdown().await;
}

/// A draft approved after the page moved is refused, not merged.
///
/// The guard already exists for ordinary writes; a draft is a version, so it
/// inherits it. Asserted because "inherits it" is a claim about code that has
/// to be true rather than a property of the design.
#[tokio::test]
async fn a_draft_whose_page_moved_underneath_it_is_refused() {
    let p = start(DraftMode::Enforce).await;
    let agent = p.mint_token(TENANT, Role::Agent);
    let human = p.mint_token_with_groups(TENANT, "consultant:alice", &[], true);

    let drafted = call(
        &p,
        &agent,
        "update_page",
        json!({
            "page_id": TRIAGE_PAGE,
            "content": published("triage", "t-1", "Proposed."),
        }),
    )
    .await;
    let version = drafted["new_version"].as_str().expect("version").to_owned();

    // The page moves under the draft — an admin publishes something else.
    call(
        &p,
        &human,
        "update_page",
        json!({
            "page_id": TRIAGE_PAGE,
            "content": published("triage", "t-1", "Someone else's change."),
            "approve": Value::Null,
        }),
    )
    .await;

    let refused = call(
        &p,
        &human,
        "update_page",
        json!({
            "page_id": TRIAGE_PAGE,
            "approve": version,
            "base_version": "v1",
            "require_exact_base": true,
        }),
    )
    .await;
    assert_eq!(
        refused["ok"],
        json!(false),
        "approving against a moved base must be refused rather than merged — \
         the reviewer approved a diff against a page that no longer exists: \
         {refused}"
    );

    p.shutdown().await;
}

// =========================================================================
// Rollout
// =========================================================================

/// `off` is today's behaviour, exactly.
///
/// Not a nicety: tenants already carry `autonomy: review` skill pages, written
/// when the key was documentation. Enforcing on upgrade would stop those pages
/// publishing, silently, and the first sign would be a consultant asking why
/// nothing saves.
#[tokio::test]
async fn drafts_are_off_by_default_and_a_review_skill_still_publishes() {
    let p = start(DraftMode::Off).await;
    let agent = p.mint_token(TENANT, Role::Agent);

    let out = call(
        &p,
        &agent,
        "update_page",
        json!({
            "page_id": TRIAGE_PAGE,
            "content": published("triage", "t-1", "Landed as before."),
        }),
    )
    .await;
    assert_eq!(out["ok"], json!(true), "{out}");
    assert_ne!(out["draft"], json!(true), "off must not hold writes: {out}");
    assert!(
        body_of(&p, &agent, TRIAGE_PAGE)
            .await
            .contains("Landed as before."),
        "at `off`, a review skill publishes exactly as it does today"
    );

    p.shutdown().await;
}

// =========================================================================
// The review queue
// =========================================================================

/// A reviewer can enumerate what is waiting, without knowing where to look.
///
/// `pending_draft` answers "does *this* page have one", which is the wrong
/// question for a review feed: a reviewer does not know which pages an agent
/// touched. This is the same verb that already lists instances, and therefore
/// the same ACL and the same scoping — a review queue is an ordinary query.
///
/// Deliberately not a search: "everything waiting" has no query text, and
/// anything ranked can drop an item without saying so. A queue that can
/// quietly omit work is worse than no queue.
#[tokio::test]
async fn drafts_only_lists_the_instances_awaiting_approval() {
    let p = start(DraftMode::Enforce).await;
    let agent = p.mint_token(TENANT, Role::Agent);

    let held = call(
        &p,
        &agent,
        "update_page",
        json!({
            "page_id": TRIAGE_PAGE,
            "content": published("triage", "t-1", "Proposed."),
        }),
    )
    .await;
    assert_eq!(held["draft"], json!(true), "precondition: {held}");

    let queue = call(
        &p,
        &agent,
        "list_instances",
        json!({ "skill_id": "triage", "drafts_only": true }),
    )
    .await;
    let ids: Vec<&str> = queue["instances"]
        .as_array()
        .expect("instances")
        .iter()
        .filter_map(|i| i["page_id"].as_str())
        .collect();
    assert_eq!(
        ids,
        vec![TRIAGE_PAGE],
        "only the instance with a pending draft may be listed: {queue}"
    );
    assert!(
        queue["instances"][0]["draft_version"]
            .as_str()
            .is_some_and(|v| !v.is_empty()),
        "and each row must name the version to approve, or the reviewer has \
         to go and ask for it page by page: {queue}"
    );

    // CONTROL — the ordinary listing is unchanged. The assertion above would
    // otherwise pass against a `list_instances` that had started returning
    // nothing at all.
    let all = call(
        &p,
        &agent,
        "list_instances",
        json!({ "skill_id": "triage" }),
    )
    .await;
    assert_eq!(
        all["instances"].as_array().map(Vec::len),
        Some(1),
        "control: the unfiltered listing must still show the published \
         instance: {all}"
    );

    p.shutdown().await;
}

/// **A draft that CREATES a record must reach the queue.**
///
/// Found by a control assertion rather than by design: a held write to a page
/// that does not exist yet leaves no `pages` row — that is what unpublished
/// means — so a queue built by filtering published instances shows nothing.
/// And the commonest held write is exactly this one: an agent filing something
/// new, which is the write no human has ever looked at.
///
/// The ACL for such a row is decided on the DRAFT's own frontmatter, because
/// there is nothing else to decide on. Deciding on nothing would either hide
/// every new record from its reviewer, or show it to everyone.
#[tokio::test]
async fn a_draft_that_creates_a_new_record_still_reaches_the_queue() {
    let p = start(DraftMode::Enforce).await;
    let agent = p.mint_token(TENANT, Role::Agent);

    let held = call(
        &p,
        &agent,
        "update_page",
        json!({
            "page_id": "markdown/instances/triage/t-new.md",
            "content": published("triage", "t-new", "A record nobody has seen."),
        }),
    )
    .await;
    assert_eq!(held["draft"], json!(true), "precondition: {held}");

    // It does not exist as an instance — nothing was published.
    let all = call(
        &p,
        &agent,
        "list_instances",
        json!({ "skill_id": "triage" }),
    )
    .await;
    let published_ids: Vec<&str> = all["instances"]
        .as_array()
        .expect("instances")
        .iter()
        .filter_map(|i| i["page_id"].as_str())
        .collect();
    assert!(
        !published_ids.contains(&"markdown/instances/triage/t-new.md"),
        "precondition: an unapproved new record must not be published: {all}"
    );

    // …and yet the reviewer must see it.
    let queue = call(
        &p,
        &agent,
        "list_instances",
        json!({ "skill_id": "triage", "drafts_only": true }),
    )
    .await;
    let queued: Vec<&str> = queue["instances"]
        .as_array()
        .expect("instances")
        .iter()
        .filter_map(|i| i["page_id"].as_str())
        .collect();
    assert_eq!(
        queued,
        vec!["markdown/instances/triage/t-new.md"],
        "a held CREATE must reach the review queue — otherwise the write \
         nobody has ever seen is also the one nobody is told about: {queue}"
    );

    p.shutdown().await;
}
