//! `list_skills` is caller-scoped, and its `acl` block is admin-only (#374).
//!
//! A running gateway (TestIssuer auth) + real Indexer + real DuckDB + real
//! JWKS; two principals in different engagement groups. No mocks, no LLM in
//! the decision.
//!
//! The gap this pins: `list_skills` was the one read verb in the dispatch
//! that took no `AclCaller`. It projected every skill row to every
//! authenticated caller, **including the full per-CRUD `acl:` block**. In
//! the shared-tenant deployment the group ACL exists for, groups are named
//! per engagement (`engagement-hoffmann`), so the grant list handed every
//! token holder the customer roster and the authorisation topology — the
//! two things #351 and #362 scoped the instance and event surfaces to hide.
//!
//! The rules under test:
//!
//!   * a non-admin caller never receives `acl` group names (redaction);
//!   * a skill whose declared `acl.read` excludes the caller is **absent**
//!     from the catalogue, not redacted-but-present (denial-as-absence,
//!     like every sibling verb);
//!   * a skill that declares no `acl:` block — and a skill carrying the
//!     legacy `visibility: owner` — stays discoverable by everyone, because
//!     a type whose *instances* are private is still a public type;
//!   * admin retains exactly what it receives today.
//!
//! Every negative assertion is paired with the positive control that would
//! catch a "hide everything from everyone" regression.

use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "stuttgart-ai";
const CAROL: &str = "consultant:carol";
const DAVE: &str = "consultant:dave";
const HOFFMANN_GROUP: &str = "engagement-hoffmann";
const ALPINA_GROUP: &str = "engagement-alpina";

/// The unrestricted type: no `acl:` block at all. Falls through to the
/// tenant default (`read: [public]`) and must stay in everyone's catalogue.
const ENGAGEMENT_SKILL: &str = "---\ntype: skill\nid: engagement\n\
    description: A customer engagement.\nvisibility: public\n---\n# engagement\n";

/// The shared type, granted to BOTH engagements. Visible to both — and the
/// carrier of the disclosure: its grant list names both customers.
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

/// A type deliberately restricted to ONE engagement. Carol may read it;
/// Dave must not learn that it exists.
const DOSSIER_SKILL: &str = r#"---
type: skill
id: hoffmann_dossier
description: The Hoffmann engagement's internal dossier.
acl:
  read: [engagement-hoffmann]
  update: [engagement-hoffmann]
  delete: [admin]
---
# hoffmann_dossier
"#;

/// **Backward compatibility.** The legacy `visibility: owner` field maps to
/// `acl.read: [owner]` inside the indexer. `owner` is an INSTANCE-grained
/// structural group — a skill page has no owner — so scoping must not read
/// it as a catalogue restriction, or every owner-visibility type in every
/// shipped tenant would vanish for every non-admin caller.
const JOURNAL_SKILL: &str = "---\ntype: skill\nid: private_journal\n\
    description: A member's own journal.\nvisibility: owner\nowner_field: credential\n\
    ---\n# private_journal\n";

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides::default(),
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("engagement", ENGAGEMENT_SKILL)
                .skill("customer_note", NOTE_SKILL)
                .skill("hoffmann_dossier", DOSSIER_SKILL)
                .skill("private_journal", JOURNAL_SKILL)
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

/// Skill ids in a `list_skills` result, sorted for a stable compare.
fn skill_ids(result: &Value) -> Vec<String> {
    let mut ids: Vec<String> = result["skills"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|s| s["id"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    ids.sort();
    ids
}

/// The row for `id`, or `None` when the catalogue does not carry it.
fn row(result: &Value, id: &str) -> Option<Value> {
    result["skills"]
        .as_array()?
        .iter()
        .find(|s| s["id"].as_str() == Some(id))
        .cloned()
}

/// Every group name appearing anywhere in the response, flattened. The
/// disclosure is about *names*, so this is what has to come back empty.
fn all_text(result: &Value) -> String {
    serde_json::to_string(result).unwrap()
}

/// **RED (a): the grant list is disclosed.** A non-admin caller receives
/// the `acl` block, and with it the names of every engagement group in the
/// tenant — including the one they are not staffed on.
///
/// The positive control is in the same test: the row itself must still
/// arrive, with its schema intact, or a fix that returns an empty
/// catalogue would pass.
#[tokio::test]
async fn a_non_admin_caller_receives_no_acl_group_names() {
    let p = start().await;
    let carol_t = carol(&p);

    let cat = call(&p, &carol_t, "list_skills", json!({})).await;

    // Positive control: the shared type is there, with its schema.
    let note = row(&cat, "customer_note").expect("carol must still see customer_note");
    assert_eq!(
        note["required_frontmatter"],
        json!(["engagement"]),
        "the schema must survive redaction: {note}"
    );

    assert!(
        note.get("acl").is_none() || note["acl"].is_null(),
        "no acl block for a non-admin caller: {note}"
    );
    let text = all_text(&cat);
    assert!(
        !text.contains(ALPINA_GROUP),
        "carol must not learn the alpina group exists: {text}"
    );
    assert!(
        !text.contains(HOFFMANN_GROUP),
        "the topology is not disclosed even for her OWN group: {text}"
    );
}

/// **RED (b): the catalogue is unscoped.** Dave is staffed on Alpina. A
/// skill whose declared `acl.read` names only `engagement-hoffmann` must be
/// absent from his catalogue — Heron's BR-SKL-1 asserts absence, which
/// redaction alone cannot deliver.
///
/// Positive controls, both directions: Carol still gets it, and Dave still
/// gets everything he is entitled to.
#[tokio::test]
async fn a_skill_the_caller_may_not_read_is_absent_from_the_catalogue() {
    let p = start().await;
    let carol_t = carol(&p);
    let dave_t = dave(&p);

    let hers = call(&p, &carol_t, "list_skills", json!({})).await;
    let his = call(&p, &dave_t, "list_skills", json!({})).await;

    assert!(
        skill_ids(&hers).contains(&"hoffmann_dossier".to_owned()),
        "carol's own engagement's type must still be listed: {hers}"
    );
    assert!(
        !skill_ids(&his).contains(&"hoffmann_dossier".to_owned()),
        "dave must not see the hoffmann dossier type: {his}"
    );
    // Positive control: Dave's catalogue is NOT empty — everything he is
    // entitled to is still there, `escurel` (the seeded meta-skill page)
    // included.
    assert_eq!(
        skill_ids(&his),
        vec![
            "customer_note".to_owned(),
            "engagement".to_owned(),
            "escurel".to_owned(),
            "private_journal".to_owned(),
        ],
        "dave keeps every type he may read: {his}"
    );
}

/// A skill with no `acl:` block, and one carrying the legacy
/// `visibility: owner`, both stay discoverable — `owner` is an
/// instance-grained structural group and never hides a *type*.
#[tokio::test]
async fn undeclared_and_legacy_visibility_skills_stay_discoverable() {
    let p = start().await;
    for (who, token) in [("carol", carol(&p)), ("dave", dave(&p))] {
        let cat = call(&p, &token, "list_skills", json!({})).await;
        let ids = skill_ids(&cat);
        assert!(
            ids.contains(&"engagement".to_owned()),
            "{who} must still see the unrestricted type: {cat}"
        );
        assert!(
            ids.contains(&"private_journal".to_owned()),
            "{who} must still see the owner-visibility type: {cat}"
        );
        // The instance-scoping metadata is schema, not a grant, and stays.
        let journal = row(&cat, "private_journal").expect("row");
        assert_eq!(journal["visibility"], json!("owner"), "{journal}");
        assert_eq!(journal["owner_field"], json!("credential"), "{journal}");
    }
}

/// Denial is **absence, not error** — the catalogue call itself succeeds
/// and is shaped exactly as a tenant without that skill would be.
#[tokio::test]
async fn denial_reads_as_absence_not_as_an_error() {
    let p = start().await;
    let body = raw(&p, &dave(&p), "list_skills", json!({})).await;
    assert!(
        body.get("error").is_none(),
        "a scoped catalogue must not fault: {body}"
    );
    assert!(
        body["result"]["structuredContent"]["skills"].is_array(),
        "the catalogue is still returned: {body}"
    );
}

/// **Admin retains what it receives today** — the whole catalogue, with
/// every `acl` block. This is what keeps an operator dashboard and the
/// explorer's ACL badge working.
#[tokio::test]
async fn admin_retains_the_full_catalogue_and_the_acl_block() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);

    let cat = call(&p, &admin, "list_skills", json!({})).await;
    let ids = skill_ids(&cat);
    for want in [
        "customer_note",
        "engagement",
        "hoffmann_dossier",
        "private_journal",
    ] {
        assert!(ids.contains(&want.to_owned()), "admin sees {want}: {cat}");
    }

    let note = row(&cat, "customer_note").expect("row");
    assert_eq!(
        note["acl"]["read"],
        json!([HOFFMANN_GROUP, ALPINA_GROUP]),
        "admin keeps the grant list: {note}"
    );
    assert_eq!(note["acl"]["delete"], json!(["admin"]), "{note}");
}
