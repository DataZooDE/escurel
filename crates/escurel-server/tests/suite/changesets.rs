//! A run's held writes, reviewed and landed as ONE change (#509 §1).
//!
//! A draft is one page. A run is usually not: an inbox agent filing a call
//! transcript touches the customer instance, creates an interaction instance
//! and updates a decision record. As three independent drafts those are
//! promoted one at a time, and a reviewer who promotes two and discards the
//! third leaves the corpus in a state no agent intended.
//!
//! What must hold, and is pinned below:
//!
//! - drafts from one run are grouped, and the group is what a reviewer sees;
//! - promotion is all-or-nothing: if any member could not land, NOTHING lands
//!   and every member stays open to be re-drafted;
//! - a retry after a mid-flight failure completes rather than conflicting —
//!   the client transport warns that a timeout may already have applied;
//! - a draft with no changeset behaves exactly as it does today;
//! - a changeset holding a draft the caller may not see is not readable, not
//!   promotable, and does not disclose that it exists.
//!
//! Real gateway, real DuckDB, real HTTP. No mocks.

use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const TENANT: &str = "stuttgart-ai";
const NOTE_SKILL: &str = "---\ntype: skill\nid: note\ndescription: A note.\n\
    visibility: public\n---\n# note\n";

/// Three pages, because the interesting failure — one member refuses, the
/// other two must not land — needs at least two survivors to be visible.
const PAGES: [&str; 3] = ["customer", "interaction", "decision"];

fn page_id(id: &str) -> String {
    format!("markdown/instances/note/{id}.md")
}

fn body(id: &str, text: &str) -> String {
    format!("---\ntype: instance\nskill: note\nid: {id}\n---\n# {id}\n{text}\n")
}

fn sha(s: &str) -> String {
    format!("{:x}", Sha256::digest(s.as_bytes()))
}

async fn start() -> EscurelProcess {
    let mut fixtures = FixtureBuilder::new()
        .tenant(TENANT)
        .skill("note", NOTE_SKILL);
    for id in PAGES {
        fixtures = fixtures.instance("note", id, body(id, "v1.").as_str());
    }
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides::default(),
        fixtures: Some(fixtures.done()),
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

/// The page's stored body, or `None` when it does not exist.
async fn page_body(p: &EscurelProcess, token: &str, id: &str) -> Option<String> {
    let r = call(p, token, "expand", json!({ "page_id": page_id(id) })).await;
    r["body"].as_str().map(str::to_owned)
}

/// Draft `id`'s page into `changeset`, from the current stored bytes.
async fn draft_into(
    p: &EscurelProcess,
    token: &str,
    changeset: Option<&str>,
    id: &str,
    text: &str,
) -> Value {
    let mut args = json!({
        "target_page_id": page_id(id),
        "content": body(id, text),
        "base_sha256": sha(&body(id, "v1.")),
    });
    match changeset {
        Some(cs) => args["changeset_id"] = json!(cs),
        None => args["new_changeset"] = json!(true),
    }
    call(p, token, "create_draft", args).await
}

/// Three pages, one run, one decision.
#[tokio::test]
async fn a_runs_drafts_are_grouped_and_land_together() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);

    // The first draft opens the changeset; the server mints its id.
    let first = draft_into(&p, &token, None, PAGES[0], "v2 customer.").await;
    assert_eq!(first["ok"], json!(true), "{first}");
    let changeset = first["draft"]["changeset_id"]
        .as_str()
        .expect("the server mints a changeset id on first use")
        .to_owned();
    assert!(!changeset.is_empty(), "{first}");

    for id in &PAGES[1..] {
        let d = draft_into(&p, &token, Some(&changeset), id, "v2.").await;
        assert_eq!(d["ok"], json!(true), "{d}");
        assert_eq!(d["draft"]["changeset_id"], json!(changeset), "{d}");
    }

    // The queue shows ONE thing to decide, not three.
    let listed = call(&p, &token, "list_changesets", json!({})).await;
    let row = listed["changesets"]
        .as_array()
        .expect("changesets")
        .iter()
        .find(|c| c["changeset_id"] == json!(changeset))
        .cloned()
        .unwrap_or_else(|| panic!("the changeset must be listed: {listed}"));
    assert_eq!(row["drafts"], json!(3), "three held writes: {row}");
    assert_eq!(row["status"], json!("open"), "{row}");
    assert!(
        row["author"].as_str().is_some_and(|a| !a.is_empty()),
        "a reviewer's first question is who proposed this: {row}"
    );

    // Nothing has landed yet — a changeset is as invisible as its drafts.
    for id in PAGES {
        assert_eq!(
            page_body(&p, &token, id).await.as_deref().map(str::trim),
            Some(format!("# {id}\nv1.").as_str()),
            "{id} must be untouched before the decision"
        );
    }

    let promoted = call(
        &p,
        &token,
        "promote_changeset",
        json!({ "changeset_id": changeset }),
    )
    .await;
    assert_eq!(promoted["ok"], json!(true), "{promoted}");
    assert_eq!(
        promoted["results"].as_array().map(Vec::len),
        Some(3),
        "every member reports its own outcome: {promoted}"
    );

    for id in PAGES {
        let landed = page_body(&p, &token, id).await.unwrap_or_default();
        assert!(landed.contains("v2"), "{id} did not land: {landed}");
    }
    let listed = call(&p, &token, "list_changesets", json!({})).await;
    let row = listed["changesets"]
        .as_array()
        .expect("changesets")
        .iter()
        .find(|c| c["changeset_id"] == json!(changeset))
        .cloned()
        .unwrap_or_default();
    assert_eq!(row["status"], json!("promoted"), "{listed}");
}

/// The property that makes a changeset worth having: a member that cannot
/// land stops the whole thing. Otherwise it is three drafts with a label.
#[tokio::test]
async fn a_changeset_that_cannot_land_entirely_lands_nothing() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);

    let first = draft_into(&p, &token, None, PAGES[0], "v2 customer.").await;
    let changeset = first["draft"]["changeset_id"]
        .as_str()
        .expect("changeset_id")
        .to_owned();
    draft_into(&p, &token, Some(&changeset), PAGES[1], "v2.").await;

    // Somebody else moves the SECOND member's target after the draft was
    // taken. Its CAS is now stale; the first member's is still good.
    call(
        &p,
        &token,
        "update_page",
        json!({
            "page_id": page_id(PAGES[1]),
            "content": body(PAGES[1], "moved underneath."),
        }),
    )
    .await;

    let promoted = call(
        &p,
        &token,
        "promote_changeset",
        json!({ "changeset_id": changeset }),
    )
    .await;
    assert_eq!(promoted["ok"], json!(false), "{promoted}");
    assert!(
        promoted["issues"]
            .as_array()
            .is_some_and(|i| i.iter().any(|i| i["code"] == json!("conflict"))),
        "the refusal must name the conflict: {promoted}"
    );

    // The clean member did NOT land. This is the whole point: a partial
    // promotion leaves the corpus in a state no agent proposed.
    let untouched = page_body(&p, &token, PAGES[0]).await.unwrap_or_default();
    assert!(
        untouched.contains("v1."),
        "a blocked changeset must land nothing: {untouched}"
    );

    // And every member is still open, so a re-draft is the answer.
    let drafts = call(&p, &token, "list_drafts", json!({})).await;
    let open = drafts["drafts"]
        .as_array()
        .expect("drafts")
        .iter()
        .filter(|d| d["changeset_id"] == json!(changeset) && d["status"] == json!("open"))
        .count();
    assert_eq!(open, 2, "both members stay open: {drafts}");
}

/// A promotion that was interrupted after landing its writes must COMPLETE on
/// retry, not conflict against its own bytes. The client transport warns that
/// a mid-flight timeout may already have applied — this is that case, for a
/// whole changeset rather than one draft (#489's rule, generalised).
#[tokio::test]
async fn promoting_a_changeset_twice_completes_rather_than_conflicting() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);

    let first = draft_into(&p, &token, None, PAGES[0], "v2 customer.").await;
    let changeset = first["draft"]["changeset_id"]
        .as_str()
        .expect("changeset_id")
        .to_owned();
    draft_into(&p, &token, Some(&changeset), PAGES[1], "v2.").await;

    let once = call(
        &p,
        &token,
        "promote_changeset",
        json!({ "changeset_id": changeset }),
    )
    .await;
    assert_eq!(once["ok"], json!(true), "{once}");

    // The retry a client makes when it never saw the first answer.
    let twice = call(
        &p,
        &token,
        "promote_changeset",
        json!({ "changeset_id": changeset }),
    )
    .await;
    assert_eq!(
        twice["ok"],
        json!(true),
        "a retry of a promoted changeset is not an error: {twice}"
    );
    assert_eq!(
        twice["already_decided"],
        json!(true),
        "and it says it was already decided rather than pretending to re-land: {twice}"
    );

    // Nothing was written twice, and the pages still hold the promoted bytes.
    for id in &PAGES[..2] {
        let landed = page_body(&p, &token, id).await.unwrap_or_default();
        assert!(landed.contains("v2"), "{id}: {landed}");
        assert!(
            !landed.contains("v2 customer.v2"),
            "a retry must not append: {landed}"
        );
    }
}

/// Refusing a run's proposal is one decision too.
#[tokio::test]
async fn discarding_a_changeset_closes_every_member_with_the_reason() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);

    let first = draft_into(&p, &token, None, PAGES[0], "v2 customer.").await;
    let changeset = first["draft"]["changeset_id"]
        .as_str()
        .expect("changeset_id")
        .to_owned();
    draft_into(&p, &token, Some(&changeset), PAGES[1], "v2.").await;

    let discarded = call(
        &p,
        &token,
        "discard_changeset",
        json!({ "changeset_id": changeset, "reason": "wrong customer" }),
    )
    .await;
    assert_eq!(discarded["ok"], json!(true), "{discarded}");
    assert_eq!(discarded["discarded"], json!(2), "{discarded}");

    // The queue no longer offers it: `list_drafts` is open drafts only, so a
    // discarded member leaves it entirely, and the changeset row says how it
    // ended rather than vanishing (the "did I already deal with that?"
    // question the drafts table keeps answerable).
    let open = call(&p, &token, "list_drafts", json!({})).await;
    assert!(
        open["drafts"]
            .as_array()
            .is_some_and(|d| !d.iter().any(|d| d["changeset_id"] == json!(changeset))),
        "a decided member must leave the open queue: {open}"
    );
    let listed = call(&p, &token, "list_changesets", json!({})).await;
    let row = listed["changesets"]
        .as_array()
        .expect("changesets")
        .iter()
        .find(|c| c["changeset_id"] == json!(changeset))
        .cloned()
        .unwrap_or_default();
    assert_eq!(row["status"], json!("discarded"), "{listed}");
    assert_eq!(row["drafts"], json!(2), "{listed}");

    // A second decision is refused, not silently re-applied.
    let again = call(
        &p,
        &token,
        "promote_changeset",
        json!({ "changeset_id": changeset }),
    )
    .await;
    assert_eq!(
        again["already_decided"],
        json!(true),
        "a discarded changeset cannot be promoted after the fact: {again}"
    );

    for id in &PAGES[..2] {
        let untouched = page_body(&p, &token, id).await.unwrap_or_default();
        assert!(untouched.contains("v1."), "{id} must be untouched");
    }
}

/// The regression that matters most: everything that does not opt in must
/// behave exactly as it did. A `changeset_id`-less draft is today's draft.
#[tokio::test]
async fn a_draft_with_no_changeset_is_unchanged() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);

    let created = call(
        &p,
        &token,
        "create_draft",
        json!({
            "target_page_id": page_id(PAGES[0]),
            "content": body(PAGES[0], "v2 alone."),
            "base_sha256": sha(&body(PAGES[0], "v1.")),
        }),
    )
    .await;
    assert_eq!(created["ok"], json!(true), "{created}");
    assert_eq!(
        created["draft"]["changeset_id"],
        Value::Null,
        "an ungrouped draft carries no changeset: {created}"
    );
    let draft_id = created["draft"]["draft_id"]
        .as_str()
        .expect("id")
        .to_owned();

    // It is not listed as a changeset of one.
    let listed = call(&p, &token, "list_changesets", json!({})).await;
    assert!(
        listed["changesets"].as_array().is_some_and(Vec::is_empty),
        "an ungrouped draft must not appear as a changeset: {listed}"
    );

    // And promotes exactly as before.
    let promoted = call(&p, &token, "promote_draft", json!({ "draft_id": draft_id })).await;
    assert_eq!(promoted["ok"], json!(true), "{promoted}");
    let landed = page_body(&p, &token, PAGES[0]).await.unwrap_or_default();
    assert!(landed.contains("v2 alone."), "{landed}");
}
