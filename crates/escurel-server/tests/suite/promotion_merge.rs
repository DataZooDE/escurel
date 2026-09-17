//! Promotion merges instead of conflicting (#509 §2).
//!
//! The review path was strictly WORSE at merging than the unreviewed one,
//! which is backwards. `promote_draft` carries the draft's `base_sha256` into
//! `update_page`'s byte CAS, so a target that moved at all refuses — even when
//! the two changes touched entirely different frontmatter keys. The very same
//! content sent straight through `update_page` with a `base_version` would
//! have been three-way-merged (Loro), persisted, and answered
//! `auto_merged: true` (`docs/spec/protocol.md:830-843`).
//!
//! So a reviewer's approval could fail for a reason that has nothing to do
//! with the review: somebody else edited a different field of the same page.
//!
//! What is pinned here:
//!
//! - a draft taken against a page that then advances on a DIFFERENT key
//!   promotes, and says it merged;
//! - both sides' edits survive — the merge is not "last writer wins" wearing
//!   a merge's clothes;
//! - the SAME key on both sides still conflicts, and the draft stays open to
//!   be re-drafted (today's behaviour, deliberately unchanged);
//! - an auto-merged artifact is re-checked against the write guards, so a
//!   `promotable: true` the head gained cannot be laundered in through a
//!   merge nobody inspected (ADR-0008);
//! - with no CRDT backend there is nothing to merge WITH, and promotion
//!   behaves exactly as it does today.
//!
//! Real gateway, real DuckDB, real Loro. No mocks.

use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const TENANT: &str = "stuttgart-ai";
const NOTE_SKILL: &str = "---\ntype: skill\nid: note\ndescription: A note.\n\
    visibility: public\n---\n# note\n";
const PAGE: &str = "markdown/instances/note/plan.md";

/// The base the draft is taken against: two independent frontmatter keys, so
/// "a different key moved" is expressible.
const BASE: &str = "---\ntype: instance\nskill: note\nid: plan\n\
    owner: mara\nstatus: open\n---\n# Plan\nv1 body.\n";

fn note(owner: &str, status: &str, body: &str) -> String {
    format!(
        "---\ntype: instance\nskill: note\nid: plan\n\
         owner: {owner}\nstatus: {status}\n---\n# Plan\n{body}\n"
    )
}

fn sha(s: &str) -> String {
    format!("{:x}", Sha256::digest(s.as_bytes()))
}

/// `live_crdt` is what makes a merge possible at all — the base snapshot the
/// three-way merge reconstructs lives in the CRDT backend. Production wires
/// it; `a_promotion_without_a_crdt_backend_behaves_as_before` covers the
/// other case.
async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            live_crdt: true,
            ..Default::default()
        },
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

async fn stored(p: &EscurelProcess, token: &str) -> String {
    let out = call(p, token, "expand", json!({ "page_id": PAGE })).await;
    format!(
        "{}\n{}",
        out["frontmatter"],
        out["body"].as_str().unwrap_or_default()
    )
}

/// Write the base content through `update_page` once.
///
/// Auto-merge reconstructs the base snapshot the drafter branched from, and
/// snapshots are written by `update_page` — a page that has only ever been
/// SEEDED has none, and the protocol says so: "a `base_version` older than
/// the first `update_page` snapshot … always conflicts"
/// (`docs/spec/protocol.md:841-843`). So the merge tests below start from a
/// page that has been written at least once, which is the state any page an
/// agent has touched is in.
async fn seed_snapshot(p: &EscurelProcess, token: &str) {
    let out = call(
        p,
        token,
        "update_page",
        json!({ "page_id": PAGE, "content": BASE }),
    )
    .await;
    assert_eq!(out["ok"], json!(true), "seed snapshot: {out}");
}

/// Draft `content` against the page as it stands right now.
async fn draft(p: &EscurelProcess, token: &str, content: &str) -> String {
    let head = call(p, token, "expand", json!({ "page_id": PAGE })).await;
    let created = call(
        p,
        token,
        "create_draft",
        json!({
            "target_page_id": PAGE,
            "content": content,
            "base_sha256": head["content_sha256"].as_str().unwrap_or_default(),
        }),
    )
    .await;
    assert_eq!(created["ok"], json!(true), "create_draft: {created}");
    created["draft"]["draft_id"]
        .as_str()
        .expect("draft_id")
        .to_owned()
}

/// The case the issue is about: two people, two different keys, one page.
#[tokio::test]
async fn a_draft_promotes_over_a_head_that_moved_on_a_different_key() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);
    seed_snapshot(&p, &token).await;

    // A reviewer's draft changes `status`.
    let draft_id = draft(&p, &token, &note("mara", "won", "v1 body.")).await;

    // Meanwhile somebody else changes `owner` — a different key entirely.
    let moved = call(
        &p,
        &token,
        "update_page",
        json!({ "page_id": PAGE, "content": note("tom", "open", "v1 body.") }),
    )
    .await;
    assert_eq!(moved["ok"], json!(true), "{moved}");

    // The approval lands, and says it merged rather than pretending the
    // target never moved.
    let promoted = call(&p, &token, "promote_draft", json!({ "draft_id": draft_id })).await;
    assert_eq!(
        promoted["ok"],
        json!(true),
        "a disjoint change must not fail an approval: {promoted}"
    );
    assert_eq!(
        promoted["auto_merged"],
        json!(true),
        "and the reviewer must be told the result is a merge, not their bytes: {promoted}"
    );

    // BOTH edits survive. This is the assertion that separates a merge from
    // last-write-wins: either side alone would satisfy "the page changed".
    let now = stored(&p, &token).await;
    assert!(
        now.contains("won"),
        "the approved change must be there: {now}"
    );
    assert!(
        now.contains("tom"),
        "and so must the concurrent one — otherwise this silently discarded a \
         write nobody reviewed: {now}"
    );
}

/// The article's showcase, from the other side: the same key on both sides is
/// a real disagreement, and a human has to settle it.
#[tokio::test]
async fn the_same_key_on_both_sides_still_conflicts_and_the_draft_survives() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);
    seed_snapshot(&p, &token).await;

    let draft_id = draft(&p, &token, &note("mara", "won", "v1 body.")).await;
    call(
        &p,
        &token,
        "update_page",
        json!({ "page_id": PAGE, "content": note("mara", "lost", "v1 body.") }),
    )
    .await;

    let promoted = call(&p, &token, "promote_draft", json!({ "draft_id": draft_id })).await;
    assert_eq!(promoted["ok"], json!(false), "{promoted}");
    assert!(
        promoted["issues"]
            .as_array()
            .is_some_and(|i| i.iter().any(|i| i["code"] == json!("conflict"))),
        "a genuine disagreement is a conflict: {promoted}"
    );

    // Nothing landed, and the draft is still open to be re-drafted — which is
    // what makes a conflict recoverable rather than a lost queue entry.
    let now = stored(&p, &token).await;
    assert!(now.contains("lost") && !now.contains("won"), "{now}");
    let queue = call(&p, &token, "list_drafts", json!({})).await;
    assert!(
        queue["drafts"]
            .as_array()
            .expect("drafts")
            .iter()
            .any(|d| d["draft_id"] == json!(draft_id)),
        "the draft must survive its own failed promotion: {queue}"
    );
}

/// ADR-0008, applied to the merged artifact: whatever produced the final
/// bytes, what persists must pass the write guards. A `promotable: true` that
/// the HEAD gained must not ride into the corpus through a merge of a draft
/// that never carried it — the reviewer approved the draft, not the merge.
#[tokio::test]
async fn an_auto_merged_artifact_cannot_launder_a_curator_marker() {
    let p = start().await;
    let agent = p.mint_token(TENANT, Role::Agent);
    let curator = p.mint_token(TENANT, Role::Admin);
    seed_snapshot(&p, &agent).await;

    let draft_id = draft(&p, &agent, &note("mara", "won", "v1 body.")).await;

    // A curator legitimately marks the page promotable — on a different key
    // from the draft's, so the merge itself would be clean.
    let marked = call(
        &p,
        &curator,
        "update_page",
        json!({
            "page_id": PAGE,
            "content": "---\ntype: instance\nskill: note\nid: plan\n\
                        owner: mara\nstatus: open\npromotable: true\n---\n# Plan\nv1 body.\n",
        }),
    )
    .await;
    assert_eq!(marked["ok"], json!(true), "{marked}");

    // The AGENT's approval would now persist a document carrying the marker.
    let promoted = call(&p, &agent, "promote_draft", json!({ "draft_id": draft_id })).await;
    assert_eq!(
        promoted["ok"],
        json!(false),
        "a non-curator must not persist the marker, however the bytes arose: {promoted}"
    );
    assert!(
        promoted["issues"].as_array().is_some_and(|i| i
            .iter()
            .any(|i| i["code"] == json!("promotable_requires_curator"))),
        "{promoted}"
    );
}

/// With no CRDT backend there is no base snapshot to merge against, so there
/// is nothing this change can do — and it must then do exactly what it did
/// before rather than something new and worse.
#[tokio::test]
async fn a_promotion_without_a_crdt_backend_behaves_as_before() {
    let p = EscurelProcess::spawn(Opts {
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
    .await;
    let token = p.mint_token(TENANT, Role::Agent);

    let created = call(
        &p,
        &token,
        "create_draft",
        json!({
            "target_page_id": PAGE,
            "content": note("mara", "won", "v1 body."),
            "base_sha256": sha(BASE),
        }),
    )
    .await;
    let draft_id = created["draft"]["draft_id"]
        .as_str()
        .expect("id")
        .to_owned();

    // An untouched target still promotes cleanly.
    let promoted = call(&p, &token, "promote_draft", json!({ "draft_id": draft_id })).await;
    assert_eq!(promoted["ok"], json!(true), "{promoted}");
    assert_eq!(
        promoted.get("auto_merged").and_then(Value::as_bool),
        None.or(Some(false)),
        "nothing was merged, and nothing may claim to have been: {promoted}"
    );

    // And a moved target still conflicts on the byte CAS, as it always did.
    let second = call(
        &p,
        &token,
        "create_draft",
        json!({
            "target_page_id": PAGE,
            "content": note("mara", "lost", "v1 body."),
            "base_sha256": sha(BASE),
        }),
    )
    .await;
    let second_id = second["draft"]["draft_id"].as_str().expect("id").to_owned();
    let refused = call(
        &p,
        &token,
        "promote_draft",
        json!({ "draft_id": second_id }),
    )
    .await;
    assert_eq!(refused["ok"], json!(false), "{refused}");
    assert!(
        refused["issues"]
            .as_array()
            .is_some_and(|i| i.iter().any(|i| i["code"] == json!("conflict"))),
        "{refused}"
    );
}
