//! The branch registry, tombstones and `merge_branch` (#512 §1–§4).
//!
//! escurel already had `base ∪ overlay` reads with a deterministic per-slug
//! override — a branch VIEW — and nothing else a branch needs. The issue
//! names the six gaps; these tests pin the four that are behaviour:
//!
//! 1. **No registry.** Scenarios were discovered by grepping frontmatter:
//!    no author, no base, no status, no lifecycle, no `list_branches`.
//! 2. **No tombstones.** An overlay could add or override but never delete,
//!    and a branch that cannot say "this instance was wrong, remove it" is
//!    not a branch.
//! 3. **The branch was a property of the PAGE, not of the write** — an agent
//!    "working on branch B" had to remember to stamp `scenario: B` into every
//!    page, and one forgotten stamp wrote to production. The issue calls this
//!    the single most dangerous property of the design, and it is the only
//!    part of the proposal that is not additive.
//! 4. **No merge verb.** An overlay could be read; it could never land.
//!
//! The trap named in `docs/notes/discovered/2026-05-29-scenario-overlay-qualify.md`
//! governs the tombstone half: the override picks the overlay row first via
//! `ORDER BY scenario NULLS LAST`, so a winning overlay marked deleted must
//! resolve to "not present" — and if the NULLS ordering is ever flipped, a
//! delete silently shows the base value instead, with no type error to catch
//! it. Every tombstone assertion below is therefore paired with a check that
//! the base twin does NOT reappear.
//!
//! Real gateway, real DuckDB, real HTTP. No mocks.

use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "stuttgart-ai";
const NOTE_SKILL: &str = "---\ntype: skill\nid: note\ndescription: A note.\n\
    visibility: public\n---\n# note\n";

fn page_id(id: &str) -> String {
    format!("markdown/instances/note/{id}.md")
}

fn note(id: &str, body: &str) -> String {
    format!("---\ntype: instance\nskill: note\nid: {id}\n---\n# {id}\n{body}\n")
}

/// Two base pages, so "the branch touched one of them" is expressible.
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
                .instance("note", "alpha", note("alpha", "base alpha.").as_str())
                .instance("note", "beta", note("beta", "base beta.").as_str())
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

/// The body of a page as the base timeline sees it, or `None` when the base
/// has no such page.
async fn base_body(p: &EscurelProcess, token: &str, id: &str) -> Option<String> {
    let out = call(p, token, "expand", json!({ "page_id": page_id(id) })).await;
    out["body"].as_str().map(str::to_owned)
}

/// The body of a slug as a BRANCH sees it.
///
/// `resolve` is what applies the per-slug override — it returns the winning
/// row's `page_id` — and `expand` then fetches it. That is the documented
/// scenario read model (`resolve` mirrors the override with `ORDER BY
/// scenario NULLS LAST LIMIT 1`), and going through it here is deliberate:
/// asserting against `expand(base_page_id, scenario)` would be asserting a
/// redirect the model does not promise.
async fn branch_body(p: &EscurelProcess, token: &str, id: &str, branch: &str) -> Option<String> {
    let resolved = call(
        p,
        token,
        "resolve",
        json!({ "wikilink": format!("[[note::{id}]]"), "scenario": branch }),
    )
    .await;
    let page_id = resolved["page"]["page_id"].as_str()?.to_owned();
    // The scenario travels into `expand` too: a base read (`scenario IS
    // NULL`) cannot see an overlay row at all, which is the existing model
    // and not something branches change.
    let out = call(
        p,
        token,
        "expand",
        json!({ "page_id": page_id, "scenario": branch }),
    )
    .await;
    out["body"].as_str().map(str::to_owned)
}

/// What `list_instances` returns for `skill: note`, optionally on a branch.
async fn listed(p: &EscurelProcess, token: &str, branch: Option<&str>) -> Vec<String> {
    let mut args = json!({ "skill": "note" });
    if let Some(b) = branch {
        args["scenario"] = json!(b);
    }
    let out = call(p, token, "list_instances", args).await;
    // The catalogue reports an instance's id inside its frontmatter — the
    // wire shape carries `page_id` + `frontmatter`, not a top-level `id`.
    out["instances"]
        .as_array()
        .map(|is| {
            is.iter()
                .filter_map(|i| i["frontmatter"]["id"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// §1: a branch is a registered thing with an author, a base and a status —
/// not a string somebody remembered to type into frontmatter.
#[tokio::test]
async fn a_branch_is_registered_with_its_author_base_and_status() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);

    let created = call(
        &p,
        &token,
        "create_branch",
        json!({ "name": "agent/inbox-scan" }),
    )
    .await;
    assert_eq!(created["ok"], json!(true), "{created}");
    let branch = &created["branch"];
    assert_eq!(branch["name"], json!("agent/inbox-scan"), "{branch}");
    assert_eq!(branch["status"], json!("open"), "{branch}");
    assert!(
        branch["author"].as_str().is_some_and(|a| !a.is_empty()),
        "a branch records WHO opened it — the first question about an isolated \
         workspace is whose it is: {branch}"
    );
    assert!(
        branch["base_version"]
            .as_str()
            .is_some_and(|b| !b.is_empty()),
        "and WHAT it forked from, or a merge has nothing to compare against: {branch}"
    );

    let all = call(&p, &token, "list_branches", json!({})).await;
    assert!(
        all["branches"]
            .as_array()
            .is_some_and(|bs| bs.iter().any(|b| b["name"] == json!("agent/inbox-scan"))),
        "a registry nobody can enumerate is frontmatter with extra steps: {all}"
    );

    // A name is a name: opening the same branch twice is refused rather than
    // silently joining somebody else's workspace.
    let twice = call(
        &p,
        &token,
        "create_branch",
        json!({ "name": "agent/inbox-scan" }),
    )
    .await;
    assert_eq!(twice["ok"], json!(false), "{twice}");
    assert!(
        twice["issues"]
            .as_array()
            .is_some_and(|i| i.iter().any(|i| i["code"] == json!("already_exists"))),
        "{twice}"
    );
}

/// §2 (the dangerous one): the branch is a property of the WRITE.
///
/// An agent working on a branch must not have to remember to stamp
/// `scenario:` into every page it writes — one forgotten stamp writes to
/// production. So the write carries the branch out of band and the SERVER
/// stamps it.
#[tokio::test]
async fn a_write_on_a_branch_never_touches_the_base_timeline() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);
    call(&p, &token, "create_branch", json!({ "name": "wip" })).await;

    // Note what is NOT in this content: any mention of `scenario`.
    let wrote = call(
        &p,
        &token,
        "update_page",
        json!({
            "page_id": page_id("alpha"),
            "content": note("alpha", "BRANCH alpha."),
            "branch": "wip",
        }),
    )
    .await;
    assert_eq!(wrote["ok"], json!(true), "{wrote}");

    // The base is untouched — this is the whole point.
    assert!(
        base_body(&p, &token, "alpha")
            .await
            .unwrap_or_default()
            .contains("base alpha."),
        "a branch write must not reach the base timeline"
    );

    // And the branch read sees the branch's version.
    let on_branch_body = branch_body(&p, &token, "alpha", "wip").await;
    assert!(
        on_branch_body
            .as_deref()
            .unwrap_or_default()
            .contains("BRANCH alpha."),
        "the branch must read its own version: {on_branch_body:?}"
    );
    let on_branch = listed(&p, &token, Some("wip")).await;
    assert_eq!(
        on_branch.iter().filter(|i| *i == "alpha").count(),
        1,
        "the overlay replaces its base twin rather than joining it: {on_branch:?}"
    );

    // A write naming a branch that was never opened is refused: a typo must
    // not silently create an isolated workspace nobody knows about.
    let typo = call(
        &p,
        &token,
        "update_page",
        json!({
            "page_id": page_id("beta"),
            "content": note("beta", "oops."),
            "branch": "wpi",
        }),
    )
    .await;
    assert_eq!(typo["ok"], json!(false), "{typo}");
    assert!(
        typo["issues"]
            .as_array()
            .is_some_and(|i| i.iter().any(|i| i["code"] == json!("unknown_branch"))),
        "{typo}"
    );
}

/// §3: a branch can DELETE. Without this an overlay can only add or override,
/// and "this instance was wrong, remove it" is inexpressible.
#[tokio::test]
async fn a_branch_can_tombstone_a_page_without_touching_the_base() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);
    call(&p, &token, "create_branch", json!({ "name": "cleanup" })).await;

    let deleted = call(
        &p,
        &token,
        "delete_page",
        json!({ "page_id": page_id("beta"), "branch": "cleanup" }),
    )
    .await;
    assert_eq!(deleted["ok"], json!(true), "{deleted}");

    // On the branch it is gone — and, the trap from the discovered note, the
    // BASE TWIN must not reappear in its place. Get the `NULLS LAST` ordering
    // wrong and the delete silently shows the base value, with no type error.
    let on_branch = listed(&p, &token, Some("cleanup")).await;
    assert!(
        !on_branch.contains(&"beta".to_owned()),
        "a tombstoned page must not be listed on its branch: {on_branch:?}"
    );
    assert!(
        on_branch.contains(&"alpha".to_owned()),
        "and the untouched page must still be: {on_branch:?}"
    );
    // Reading the BASE page id under the branch must not show the base
    // either: a tombstone is a statement about the SLUG, so the page reads as
    // absent on that branch rather than as its base self.
    let expanded = call(
        &p,
        &token,
        "expand",
        json!({ "page_id": page_id("beta"), "scenario": "cleanup" }),
    )
    .await;
    assert!(
        expanded
            .get("body")
            .and_then(Value::as_str)
            .is_none_or(|b| !b.contains("base beta.")),
        "the base twin must NOT show through a tombstone — that is the \
         NULLS-LAST trap: {expanded}"
    );
    assert!(
        branch_body(&p, &token, "beta", "cleanup")
            .await
            .is_none_or(|b| !b.contains("base beta.")),
        "and resolving the slug on the branch must not land on the base twin"
    );

    // The base timeline is untouched.
    assert!(
        base_body(&p, &token, "beta")
            .await
            .unwrap_or_default()
            .contains("base beta."),
        "a branch delete must not reach the base"
    );
    assert!(listed(&p, &token, None).await.contains(&"beta".to_owned()));
}

/// §4: a branch can LAND. An overlay that can only be read is a what-if, not
/// a branch — and the merge is the existing three-way merge, not a new one.
#[tokio::test]
async fn merging_a_branch_lands_its_pages_and_closes_it() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);
    call(&p, &token, "create_branch", json!({ "name": "ready" })).await;

    call(
        &p,
        &token,
        "update_page",
        json!({
            "page_id": page_id("alpha"),
            "content": note("alpha", "MERGED alpha."),
            "branch": "ready",
        }),
    )
    .await;
    call(
        &p,
        &token,
        "delete_page",
        json!({ "page_id": page_id("beta"), "branch": "ready" }),
    )
    .await;

    let merged = call(&p, &token, "merge_branch", json!({ "name": "ready" })).await;
    assert_eq!(merged["ok"], json!(true), "{merged}");
    assert_eq!(
        merged["results"].as_array().map(Vec::len),
        Some(2),
        "every page in the branch reports its own outcome: {merged}"
    );

    // The edit landed on the base…
    assert!(
        base_body(&p, &token, "alpha")
            .await
            .unwrap_or_default()
            .contains("MERGED alpha."),
        "the branch's edit must land"
    );
    // …and so did the delete. A merge that lands edits but silently drops
    // tombstones would leave the corpus in a state the branch never had.
    assert!(
        !listed(&p, &token, None).await.contains(&"beta".to_owned()),
        "the branch's tombstone must land too"
    );

    // The branch is closed, and closed once.
    let all = call(&p, &token, "list_branches", json!({})).await;
    let row = all["branches"]
        .as_array()
        .expect("branches")
        .iter()
        .find(|b| b["name"] == json!("ready"))
        .cloned()
        .unwrap_or_default();
    assert_eq!(row["status"], json!("merged"), "{all}");
    let again = call(&p, &token, "merge_branch", json!({ "name": "ready" })).await;
    assert_eq!(again["ok"], json!(false), "{again}");
    assert!(
        again["issues"]
            .as_array()
            .is_some_and(|i| i.iter().any(|i| i["code"] == json!("already_decided"))),
        "{again}"
    );
}

/// The merge blocks on a real disagreement, and blocks WHOLE: a branch that
/// lands half its pages leaves the corpus in a state nobody authored.
#[tokio::test]
async fn a_branch_whose_base_moved_on_the_same_key_blocks_entirely() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);
    call(&p, &token, "create_branch", json!({ "name": "stale" })).await;

    // Two pages on the branch. The disagreement has to be on a FRONTMATTER
    // KEY: two edits to the same body are a CRDT union (both survive), which
    // is a merge working rather than a conflict — the same distinction #509
    // §2's merge draws.
    call(
        &p,
        &token,
        "update_page",
        json!({
            "page_id": page_id("alpha"),
            "content": note("alpha", "BRANCH alpha."),
            "branch": "stale",
        }),
    )
    .await;
    call(
        &p,
        &token,
        "update_page",
        json!({
            "page_id": page_id("beta"),
            "content": "---\ntype: instance\nskill: note\nid: beta\nstatus: won\n---\n# beta\nbase beta.\n",
            "branch": "stale",
        }),
    )
    .await;
    // …and the BASE of that same page moves the SAME key, to a different
    // value.
    call(
        &p,
        &token,
        "update_page",
        json!({
            "page_id": page_id("beta"),
            "content": "---\ntype: instance\nskill: note\nid: beta\nstatus: lost\n---\n# beta\nbase beta.\n",
        }),
    )
    .await;

    let merged = call(&p, &token, "merge_branch", json!({ "name": "stale" })).await;
    assert_eq!(merged["ok"], json!(false), "{merged}");
    assert!(
        merged["issues"]
            .as_array()
            .is_some_and(|i| i.iter().any(|i| i["code"] == json!("conflict"))),
        "the refusal must name the conflict: {merged}"
    );

    // NOTHING landed — not even the page that would have merged cleanly.
    assert!(
        base_body(&p, &token, "alpha")
            .await
            .unwrap_or_default()
            .contains("base alpha."),
        "a blocked merge must land nothing"
    );
    let beta_now = call(&p, &token, "expand", json!({ "page_id": page_id("beta") })).await;
    assert_eq!(
        beta_now["frontmatter"]["status"],
        json!("lost"),
        "and must not clobber the concurrent base edit: {beta_now}"
    );

    // The branch stays open, so the conflict is recoverable.
    let all = call(&p, &token, "list_branches", json!({})).await;
    assert_eq!(
        all["branches"]
            .as_array()
            .expect("branches")
            .iter()
            .find(|b| b["name"] == json!("stale"))
            .map(|b| b["status"].clone()),
        Some(json!("open")),
        "{all}"
    );
}

/// Abandoning is a decision too: the pages stay readable on the branch (for
/// the record) and never reach the base.
#[tokio::test]
async fn abandoning_a_branch_closes_it_without_landing_anything() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);
    call(&p, &token, "create_branch", json!({ "name": "wrong" })).await;
    call(
        &p,
        &token,
        "update_page",
        json!({
            "page_id": page_id("alpha"),
            "content": note("alpha", "NEVER alpha."),
            "branch": "wrong",
        }),
    )
    .await;

    let abandoned = call(
        &p,
        &token,
        "abandon_branch",
        json!({ "name": "wrong", "reason": "wrong customer" }),
    )
    .await;
    assert_eq!(abandoned["ok"], json!(true), "{abandoned}");

    assert!(
        base_body(&p, &token, "alpha")
            .await
            .unwrap_or_default()
            .contains("base alpha."),
        "an abandoned branch lands nothing"
    );
    let all = call(&p, &token, "list_branches", json!({})).await;
    let row = all["branches"]
        .as_array()
        .expect("branches")
        .iter()
        .find(|b| b["name"] == json!("wrong"))
        .cloned()
        .unwrap_or_default();
    assert_eq!(row["status"], json!("abandoned"), "{all}");
    assert_eq!(row["reason"], json!("wrong customer"), "{all}");

    // And a decided branch accepts no further writes: an isolated workspace
    // somebody abandoned must not keep accumulating work.
    let late = call(
        &p,
        &token,
        "update_page",
        json!({
            "page_id": page_id("beta"),
            "content": note("beta", "too late."),
            "branch": "wrong",
        }),
    )
    .await;
    assert_eq!(late["ok"], json!(false), "{late}");
}

/// The regression that matters most: everything that does not name a branch
/// behaves exactly as it did. `scenario:` frontmatter still works, and the
/// base timeline is still the default.
#[tokio::test]
async fn nothing_that_ignores_branches_changes() {
    let p = start().await;
    let token = p.mint_token(TENANT, Role::Agent);

    // A plain write, no `branch` field.
    let wrote = call(
        &p,
        &token,
        "update_page",
        json!({ "page_id": page_id("alpha"), "content": note("alpha", "plain alpha.") }),
    )
    .await;
    assert_eq!(wrote["ok"], json!(true), "{wrote}");
    assert!(
        base_body(&p, &token, "alpha")
            .await
            .unwrap_or_default()
            .contains("plain alpha.")
    );

    // Author-supplied `scenario:` frontmatter still creates an overlay — the
    // pre-registry way, which existing tenants use and which this change
    // deprecates rather than breaks.
    let legacy = call(
        &p,
        &token,
        "update_page",
        json!({
            "page_id": "markdown/instances/note/alpha@legacy.md",
            "content": "---\ntype: instance\nskill: note\nid: alpha\n\
                        scenario: legacy\n---\n# alpha\nLEGACY alpha.\n",
        }),
    )
    .await;
    assert_eq!(legacy["ok"], json!(true), "{legacy}");
    let legacy_body = branch_body(&p, &token, "alpha", "legacy").await;
    assert!(
        legacy_body
            .as_deref()
            .unwrap_or_default()
            .contains("LEGACY alpha."),
        "an unregistered author-stamped overlay still reads as one: {legacy_body:?}"
    );
}

/// An agent sees its OWN branches, not every branch in the tenant.
///
/// `tool_list_branches` took `_caller: AclCaller<'_>` — underscore-prefixed,
/// deliberately unused — and returned the whole registry to anyone. Branch
/// names are the finding: by the repo's own convention they read like
/// `agent/acme-renegotiation`, so the listing disclosed what other agents
/// were working on, and on whose records, without reading a single page.
///
/// The registry already records an `author` per branch, which is the scope
/// drafts use. Admin still sees everything.
#[tokio::test]
async fn an_agent_sees_only_the_branches_it_authored() {
    let p = start().await;
    let alice = p.mint_token_with_groups(TENANT, "agent:alice", &[], false);
    let bob = p.mint_token_with_groups(TENANT, "agent:bob", &[], false);

    let mine = call(
        &p,
        &alice,
        "create_branch",
        json!({ "name": "agent/alice-workspace" }),
    )
    .await;
    assert!(
        mine["ok"] == json!(true),
        "premise: alice opens a branch: {mine}"
    );

    let theirs = call(
        &p,
        &bob,
        "create_branch",
        json!({ "name": "agent/bob-acme-renegotiation" }),
    )
    .await;
    assert!(
        theirs["ok"] == json!(true),
        "premise: bob opens a branch: {theirs}"
    );

    let listed = call(&p, &alice, "list_branches", json!({})).await;
    let names: Vec<String> = listed["branches"]
        .as_array()
        .expect("branches array")
        .iter()
        .filter_map(|b| b["name"].as_str().map(str::to_owned))
        .collect();

    assert!(
        names.iter().any(|n| n == "agent/alice-workspace"),
        "alice must see her own branch: {names:?}"
    );
    assert!(
        !names.iter().any(|n| n == "agent/bob-acme-renegotiation"),
        "alice must NOT see bob's branch — the name alone says what he is \
         working on and for whom: {names:?}"
    );

    p.shutdown().await;
}
