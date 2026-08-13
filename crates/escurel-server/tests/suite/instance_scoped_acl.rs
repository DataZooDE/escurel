//! Instance-level `acl:` overrides (#351), enforced at the MCP boundary
//! over real HTTP. A running gateway (TestIssuer auth) + real Indexer +
//! real DuckDB; tokens carry the subject AND the engagement groups. No
//! mocks, no LLM in the decision.
//!
//! The gap this pins: group ACL v1 declares `acl:` on the SKILL page, so a
//! group grant necessarily spans every instance of that skill. Only
//! `owner` was instance-grained. Two consultants staffed on different
//! engagements therefore could not share one `customer_note` type without
//! seeing each other's customers.
//!
//! The rule under test:
//!
//!   * an instance carrying its own `acl:` block has THAT block decide, on
//!     every read verb (`expand`, `search`, `list_instances`, `resolve`,
//!     `neighbours`) and on writes;
//!   * an instance WITHOUT one falls through to the skill's block, so no
//!     existing page changes behaviour;
//!   * denial stays absence, never a distinguishable error.
//!
//! Every negative assertion here is paired with the positive control that
//! would catch a "hide everything from everyone" regression.

use escurel_server::WriteAclMode;
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "stuttgart-ai";
const CAROL: &str = "consultant:carol";
const DAVE: &str = "consultant:dave";
const HOFFMANN_GROUP: &str = "engagement-hoffmann";
const ALPINA_GROUP: &str = "engagement-alpina";

// A public "type" page for the engagements themselves — the notes wikilink
// to these, which is what gives `neighbours` an edge to filter.
const ENGAGEMENT_SKILL: &str = "---\ntype: skill\nid: engagement\n\
    description: A customer engagement.\nvisibility: public\n---\n# engagement\n";
const HOFFMANN: &str = "---\ntype: instance\nskill: engagement\nid: hoffmann\n---\n# Hoffmann\n";
const ALPINA: &str = "---\ntype: instance\nskill: engagement\nid: alpina\n---\n# Alpina\n";

// The shared type. Its skill-level block grants BOTH engagements — which
// is exactly the v1 shape the issue calls too coarse: without an
// instance-level override, either group reads every note.
const NOTE_SKILL: &str = r#"---
type: skill
id: customer_note
description: A note filed against an engagement.
required_frontmatter: [engagement]
acl:
  read: [engagement-hoffmann, engagement-alpina]
  create: [engagement-hoffmann, engagement-alpina]
  update: [engagement-hoffmann, engagement-alpina]
  delete: [admin]
---
# customer_note
"#;

// Two notes of that one skill, each narrowed to its own engagement.
const HOFFMANN_NOTE: &str = r#"---
type: instance
skill: customer_note
id: hoffmann-1
engagement: "[[engagement::hoffmann]]"
acl:
  read: [engagement-hoffmann]
  update: [engagement-hoffmann]
---
# Hoffmann
Zwischenbericht: die Migration liegt hinter dem Plan.
"#;
const ALPINA_NOTE: &str = r#"---
type: instance
skill: customer_note
id: alpina-1
engagement: "[[engagement::alpina]]"
acl:
  read: [engagement-alpina]
  update: [engagement-alpina]
---
# Alpina
Zwischenbericht: die Verlaengerung ist gefaehrdet.
"#;
// The backward-compatibility page: NO `acl:` block, so the skill's block
// decides and both engagements keep reading it exactly as they do today.
const SHARED_NOTE: &str = r#"---
type: instance
skill: customer_note
id: shared-1
engagement: "[[engagement::hoffmann]]"
---
# Shared
Zwischenbericht: allgemeiner Hinweis fuer beide Mandate.
"#;

const HOFFMANN_NOTE_PAGE: &str = "markdown/instances/customer_note/hoffmann-1.md";
const ALPINA_NOTE_PAGE: &str = "markdown/instances/customer_note/alpina-1.md";
const SHARED_NOTE_PAGE: &str = "markdown/instances/customer_note/shared-1.md";
const ALPINA_PAGE: &str = "markdown/instances/engagement/alpina.md";

async fn start() -> EscurelProcess {
    start_with(WriteAclMode::Off).await
}

async fn start_with(write_acl: WriteAclMode) -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            write_acl: Some(write_acl),
            ..Default::default()
        },
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("engagement", ENGAGEMENT_SKILL)
                .skill("customer_note", NOTE_SKILL)
                .instance("engagement", "hoffmann", HOFFMANN)
                .instance("engagement", "alpina", ALPINA)
                .instance("customer_note", "hoffmann-1", HOFFMANN_NOTE)
                .instance("customer_note", "alpina-1", ALPINA_NOTE)
                .instance("customer_note", "shared-1", SHARED_NOTE)
                .done(),
        ),
    })
    .await
}

/// The whole JSON-RPC response body, so a caller can inspect `result` vs
/// `error` (denial must never arrive as the latter).
async fn raw(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
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

/// `structuredContent`, asserting the call did not fault.
async fn call(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    let body = raw(p, token, name, args).await;
    assert!(body.get("error").is_none(), "{name} error: {body}");
    body["result"]["structuredContent"].clone()
}

fn carol(p: &EscurelProcess) -> String {
    p.mint_token_with_groups(TENANT, CAROL, &[HOFFMANN_GROUP], false)
}

fn dave(p: &EscurelProcess) -> String {
    p.mint_token_with_groups(TENANT, DAVE, &[ALPINA_GROUP], false)
}

/// Page ids in a `search` result, in rank order.
fn hit_pages(result: &Value) -> Vec<String> {
    result["hits"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|h| h["page_id"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Instance ids in a `list_instances` result, sorted for a stable compare.
fn listed_ids(result: &Value) -> Vec<String> {
    let mut ids: Vec<String> = result["instances"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|i| i["frontmatter"]["id"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    ids.sort();
    ids
}

/// THE GAP, on the simplest read verb. Carol is staffed on Hoffmann only.
/// The skill grants both engagements, so before instance-level `acl:` is
/// honoured she reads Alpina's note too.
///
/// The positive control is in the same test on purpose: Carol MUST still
/// expand her OWN engagement's note, or a fix that denies everything to
/// everyone would pass.
#[tokio::test]
async fn expand_honours_the_instances_own_acl_block() {
    let p = start().await;
    let carol_t = carol(&p);
    let dave_t = dave(&p);

    // Positive control, both directions.
    let mine = call(
        &p,
        &carol_t,
        "expand",
        json!({ "page_id": HOFFMANN_NOTE_PAGE }),
    )
    .await;
    assert!(
        mine["page"].is_object(),
        "carol reads her own engagement's note: {mine}"
    );
    let theirs = call(
        &p,
        &dave_t,
        "expand",
        json!({ "page_id": ALPINA_NOTE_PAGE }),
    )
    .await;
    assert!(
        theirs["page"].is_object(),
        "dave reads his own engagement's note: {theirs}"
    );

    // The leak: the same skill, a different engagement.
    let leak = call(
        &p,
        &carol_t,
        "expand",
        json!({ "page_id": ALPINA_NOTE_PAGE }),
    )
    .await;
    assert!(
        leak["page"].is_null(),
        "carol must NOT read alpina's note: {leak}"
    );
    let leak2 = call(
        &p,
        &dave_t,
        "expand",
        json!({ "page_id": HOFFMANN_NOTE_PAGE }),
    )
    .await;
    assert!(
        leak2["page"].is_null(),
        "dave must NOT read hoffmann's note: {leak2}"
    );
}

/// Denial is **absence, not error** — the same shape a genuinely missing
/// page returns, so the refusal is not an existence oracle.
#[tokio::test]
async fn denial_reads_as_absence_not_as_an_error() {
    let p = start().await;
    let carol_t = carol(&p);

    let hidden = raw(
        &p,
        &carol_t,
        "expand",
        json!({ "page_id": ALPINA_NOTE_PAGE }),
    )
    .await;
    let ghost = raw(
        &p,
        &carol_t,
        "expand",
        json!({ "page_id": "markdown/instances/customer_note/no-such-note.md" }),
    )
    .await;
    assert!(
        hidden.get("error").is_none(),
        "a denied read must not fault: {hidden}"
    );
    assert_eq!(
        hidden["result"]["structuredContent"], ghost["result"]["structuredContent"],
        "hidden must be indistinguishable from absent: {hidden} vs {ghost}"
    );
}

/// `search` is a second read path onto the same rows and must be filtered
/// identically — a fix that lands `expand` and leaks through retrieval is
/// not a fix. All three notes share the query term.
#[tokio::test]
async fn search_does_not_surface_another_engagements_note() {
    let p = start().await;
    let carol_t = carol(&p);

    let hits = call(
        &p,
        &carol_t,
        "search",
        json!({ "q": "Zwischenbericht", "k": 20 }),
    )
    .await;
    let pages = hit_pages(&hits);
    // Positive control: retrieval works and reaches her own note.
    assert!(
        pages.iter().any(|p| p == HOFFMANN_NOTE_PAGE),
        "carol's search must find her own note: {hits}"
    );
    assert!(
        !pages.iter().any(|p| p == ALPINA_NOTE_PAGE),
        "carol's search must NOT surface alpina's note: {hits}"
    );
}

/// Enumeration must not leak what a direct read denies — and must still
/// list the fall-through page, which is the backward-compatibility half.
#[tokio::test]
async fn list_instances_filters_by_the_instance_acl() {
    let p = start().await;
    let carol_t = carol(&p);
    let dave_t = dave(&p);

    let hers = call(
        &p,
        &carol_t,
        "list_instances",
        json!({ "skill_id": "customer_note" }),
    )
    .await;
    assert_eq!(
        listed_ids(&hers),
        vec!["hoffmann-1".to_owned(), "shared-1".to_owned()],
        "carol enumerates her engagement's note + the unscoped one: {hers}"
    );

    let his = call(
        &p,
        &dave_t,
        "list_instances",
        json!({ "skill_id": "customer_note" }),
    )
    .await;
    assert_eq!(
        listed_ids(&his),
        vec!["alpina-1".to_owned(), "shared-1".to_owned()],
        "dave enumerates his engagement's note + the unscoped one: {his}"
    );
}

/// `resolve` must not disclose the page_id / existence of an instance the
/// caller may not read.
#[tokio::test]
async fn resolve_hides_an_instance_scoped_note() {
    let p = start().await;
    let carol_t = carol(&p);

    let own = call(
        &p,
        &carol_t,
        "resolve",
        json!({ "wikilink": "[[customer_note::hoffmann-1]]" }),
    )
    .await;
    assert_eq!(own["exists"], json!(true), "carol resolves her own: {own}");

    let other = call(
        &p,
        &carol_t,
        "resolve",
        json!({ "wikilink": "[[customer_note::alpina-1]]" }),
    )
    .await;
    assert_eq!(
        other["exists"],
        json!(false),
        "carol must not resolve alpina's note: {other}"
    );
    assert!(other["page"].is_null(), "no page_id leaked: {other}");
}

/// The graph verb leaks by edge rather than by row: the Alpina engagement
/// page is public, and its IN-edges name the notes filed against it.
#[tokio::test]
async fn neighbours_filters_edges_from_instance_scoped_notes() {
    let p = start().await;
    let carol_t = carol(&p);
    let dave_t = dave(&p);

    // Positive control: the owner of the engagement sees the edge.
    let his = call(
        &p,
        &dave_t,
        "neighbours",
        json!({ "page_id": ALPINA_PAGE, "direction": "in" }),
    )
    .await;
    assert!(
        !his["edges"].as_array().unwrap().is_empty(),
        "dave sees the edge from his own note: {his}"
    );

    let hers = call(
        &p,
        &carol_t,
        "neighbours",
        json!({ "page_id": ALPINA_PAGE, "direction": "in" }),
    )
    .await;
    assert_eq!(
        hers["edges"].as_array().unwrap().len(),
        0,
        "carol must not see edges from alpina's note: {hers}"
    );
}

/// **Backward compatibility, proved rather than asserted.** An instance
/// with no `acl:` block behaves exactly as it does today: the skill's
/// block decides, so both engagements read it.
#[tokio::test]
async fn an_instance_without_an_acl_block_falls_through_to_the_skill() {
    let p = start().await;
    for (who, token) in [("carol", carol(&p)), ("dave", dave(&p))] {
        let r = call(&p, &token, "expand", json!({ "page_id": SHARED_NOTE_PAGE })).await;
        assert!(
            r["page"].is_object(),
            "{who} must still read the unscoped note (skill grant): {r}"
        );
    }
}

/// Admin bypasses, as everywhere — what keeps an operator dashboard and a
/// service-token worker working across engagements.
#[tokio::test]
async fn admin_bypasses_the_instance_acl() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);
    let all = call(
        &p,
        &admin,
        "list_instances",
        json!({ "skill_id": "customer_note" }),
    )
    .await;
    assert_eq!(
        listed_ids(&all),
        vec![
            "alpina-1".to_owned(),
            "hoffmann-1".to_owned(),
            "shared-1".to_owned()
        ],
        "admin sees every note: {all}"
    );
}

/// `update_page` over MCP; returns the `{ok, issues}` payload.
async fn update(p: &EscurelProcess, token: &str, page_id: &str, content: &str) -> Value {
    call(
        p,
        token,
        "update_page",
        json!({ "page_id": page_id, "content": content }),
    )
    .await
}

const HOFFMANN_NOTE_EDIT: &str = r#"---
type: instance
skill: customer_note
id: hoffmann-1
engagement: "[[engagement::hoffmann]]"
acl:
  read: [engagement-hoffmann]
  update: [engagement-hoffmann]
---
# Hoffmann
Zwischenbericht: nachgetragen von Carol.
"#;
const ALPINA_NOTE_EDIT: &str = r#"---
type: instance
skill: customer_note
id: alpina-1
engagement: "[[engagement::alpina]]"
acl:
  read: [engagement-alpina]
  update: [engagement-alpina]
---
# Alpina
Zwischenbericht: nachgetragen.
"#;

/// The write half of the same scoping. With `ESCUREL_WRITE_ACL=enforce`,
/// a consultant may edit a colleague's note on HER engagement (which
/// `owner_field` could never express) and may not touch another one's.
#[tokio::test]
async fn instance_acl_scopes_writes() {
    let p = start_with(WriteAclMode::Enforce).await;
    let carol_t = carol(&p);
    let dave_t = dave(&p);

    // Positive control: the group grant on the instance authorises the
    // edit even though carol is not the `owner` of anything.
    let ok = update(&p, &carol_t, HOFFMANN_NOTE_PAGE, HOFFMANN_NOTE_EDIT).await;
    assert_eq!(ok["ok"], json!(true), "carol edits her engagement: {ok}");
    let ok2 = update(&p, &dave_t, ALPINA_NOTE_PAGE, ALPINA_NOTE_EDIT).await;
    assert_eq!(ok2["ok"], json!(true), "dave edits his engagement: {ok2}");

    // The cross-engagement write must be refused.
    let denied = update(&p, &carol_t, ALPINA_NOTE_PAGE, ALPINA_NOTE_EDIT).await;
    assert_eq!(
        denied["ok"],
        json!(false),
        "carol must NOT write alpina's note: {denied}"
    );
    assert_eq!(
        denied["issues"][0]["code"],
        json!("forbidden"),
        "refused as forbidden: {denied}"
    );
}
