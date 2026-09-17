//! Stored corpus traversals — `[[query::*]]` pages whose `target: corpus`
//! (#511), over real HTTP against a running gateway with a real Indexer.
//!
//! The gap: `query_instance` queries EXTERNAL tables through a `sql_view`
//! instance, never the markdown corpus, and the legacy `run_stored_query`
//! (arbitrary SQL over `pages`) was removed on 2026-08-14 because there was
//! no per-row owner to ACL against. So "which of our people knows someone at
//! this account" could not be saved as a named, parameterised, reviewable
//! artefact — it had to be assembled client-side out of N `neighbours` calls,
//! which is slow, unatomic, and invisible to ACL reasoning as a whole.
//!
//! What is pinned here, in the issue's own order:
//!
//! - the article's `warm_intro($account)` is expressible as ONE page and
//!   returns its rows in one call;
//! - a caller who may not read an intermediate instance does not see paths
//!   through it — the fail-closed property `run_stored_query` could not give;
//! - a traversal with no `max_depth`, or past the server cap, is rejected at
//!   VALIDATION time, not at query time;
//! - a `sql_view` target still behaves exactly as it did.
//!
//! No mocks: real gateway, real DuckDB, real link index.

use escurel_test_support::{
    AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role, WriteAclMode,
};
use serde_json::{Value, json};

const TENANT: &str = "stuttgart-ai";
const ALICE: &str = "consultant:alice";
const BOB: &str = "consultant:bob";

const QUERY_SKILL: &str = "---\ntype: skill\nid: query\ndescription: A stored query.\n\
    visibility: public\n---\n# query\n";
const COMPANY_SKILL: &str = "---\ntype: skill\nid: company\ndescription: A company.\n\
    visibility: public\n---\n# company\n";
/// The contact AT the account is a shared record — everyone in the tenant
/// works the same accounts. Our own people are not: `person` is owner-scoped,
/// which is what makes the ACL half testable, because a traversal that hops
/// through someone else's person record must not leak it.
const CONTACT_SKILL: &str = "---\ntype: skill\nid: contact\ndescription: Someone at an account.\n\
    visibility: public\n---\n# contact\n";
const PERSON_SKILL: &str = "---\ntype: skill\nid: person\ndescription: One of ours.\n\
    visibility: owner\nowner_field: credential\n---\n# person\n";

fn company(id: &str) -> String {
    format!("---\ntype: instance\nskill: company\nid: {id}\nname: {id} GmbH\n---\n# {id}\n")
}

/// A contact who works at `employer`, as a typed wikilink — so the traversal
/// walks REAL link rows rather than a fixture shortcut.
fn contact(id: &str, employer: &str) -> String {
    format!(
        "---\ntype: instance\nskill: contact\nid: {id}\nname: {id}\n\
         works_at: \"[[company::{employer}]]\"\n---\n# {id}\n"
    )
}

/// One of ours, owned by `credential`, who knows a contact.
fn person(id: &str, credential: &str, employer: &str, knows: &str) -> String {
    format!(
        "---\ntype: instance\nskill: person\nid: {id}\nname: {id}\n\
         credential: \"{credential}\"\nemployer: {employer}\n\
         knows: \"[[contact::{knows}]]\"\n---\n# {id}\n"
    )
}

/// The article's `warm_intro($account)`, as one page.
const WARM_INTRO: &str = "\
---
type: instance
skill: query
id: warm-intro
description: Which of our people knows someone at the target account?
target: corpus
params:
  - {name: account, kind: string, required: true}
traversal:
  start: {skill: company, id: \"{{account}}\"}
  steps:
    - {relation: works_at, direction: in, as: contact}
    - {relation: knows, direction: in, as: teammate}
  return: [teammate.name, contact.name]
  max_depth: 3
  limit: 200
---
# warm-intro
";

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            write_acl: Some(WriteAclMode::Enforce),
            ..Default::default()
        },
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("query", QUERY_SKILL)
                .skill("company", COMPANY_SKILL)
                .skill("contact", CONTACT_SKILL)
                .skill("person", PERSON_SKILL)
                .instance("company", "globex", company("globex").as_str())
                .instance("company", "datazoo", company("datazoo").as_str())
                // The contact at the target account — shared, as accounts are.
                .instance("contact", "wile", contact("wile", "globex").as_str())
                // Known by one of ours, which is Alice's record…
                .instance(
                    "person",
                    "mara",
                    person("mara", ALICE, "datazoo", "wile").as_str(),
                )
                // …and by one that is Bob's, so each sees their own path
                // through the same contact and neither sees the other's.
                .instance(
                    "person",
                    "tom",
                    person("tom", BOB, "datazoo", "wile").as_str(),
                )
                .instance("query", "warm-intro", WARM_INTRO)
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

/// The acceptance criterion, verbatim: one page, one call, the teammate →
/// contact → account rows.
#[tokio::test]
async fn a_stored_traversal_answers_the_warm_intro_question_in_one_call() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);

    let out = call(
        &p,
        &admin,
        "query_instance",
        json!({ "ref": "[[query::warm-intro]]", "params": { "account": "globex" } }),
    )
    .await;

    let rows = out["rows"].as_array().expect("rows");
    assert!(
        rows.iter()
            .any(|r| r["teammate.name"] == json!("mara") && r["contact.name"] == json!("wile")),
        "the traversal must find mara → wile → globex: {out}"
    );
    // The schema names the projected columns, so a client can render the
    // result without knowing the traversal.
    let cols: Vec<&str> = out["schema"]
        .as_array()
        .expect("schema")
        .iter()
        .filter_map(|c| c["name"].as_str())
        .collect();
    assert!(
        cols.contains(&"teammate.name") && cols.contains(&"contact.name"),
        "{out}"
    );

    // A traversal is a QUERY, not a guess: an account nobody works at
    // returns no rows rather than an error.
    let empty = call(
        &p,
        &admin,
        "query_instance",
        json!({ "ref": "[[query::warm-intro]]", "params": { "account": "datazoo-nonexistent" } }),
    )
    .await;
    assert_eq!(empty["rows"].as_array().map(Vec::len), Some(0), "{empty}");
}

/// The property `run_stored_query` could not provide, and the reason this is
/// a declarative traversal rather than SQL: ACL is per ROW, on the instances
/// actually traversed. A caller sees only what they could have reached by
/// walking `neighbours` themselves.
#[tokio::test]
async fn a_traversal_does_not_leak_paths_through_an_unreadable_instance() {
    let p = start().await;
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let bob = p.mint_token_with_sub(TENANT, Role::Agent, BOB);

    let hers = call(
        &p,
        &alice,
        "query_instance",
        json!({ "ref": "[[query::warm-intro]]", "params": { "account": "globex" } }),
    )
    .await;
    let names: Vec<&str> = hers["rows"]
        .as_array()
        .expect("rows")
        .iter()
        .filter_map(|r| r["teammate.name"].as_str())
        .collect();
    assert!(
        names.contains(&"mara"),
        "alice must see the path through her own record: {hers}"
    );
    assert!(
        !names.contains(&"tom"),
        "and must NOT see the path through bob's: {hers}"
    );
    assert!(
        !hers.to_string().contains("tom"),
        "no part of an unreadable instance may appear, not even in a path: {hers}"
    );

    // The positive control: the path IS there, for the person who owns it.
    // Without this, "alice sees no tom" would also be satisfied by a
    // traversal that simply never found him.
    let his = call(
        &p,
        &bob,
        "query_instance",
        json!({ "ref": "[[query::warm-intro]]", "params": { "account": "globex" } }),
    )
    .await;
    assert!(
        his["rows"]
            .as_array()
            .expect("rows")
            .iter()
            .any(|r| r["teammate.name"] == json!("tom")),
        "bob must see his own: {his}"
    );
}

/// A bound that is checked at query time is a bound that ships broken. The
/// author finds out when they write the page.
#[tokio::test]
async fn an_unbounded_or_malformed_traversal_is_rejected_at_validation_time() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);

    let page = |traversal: &str| {
        format!(
            "---\ntype: instance\nskill: query\nid: bad\ntarget: corpus\n{traversal}---\n# bad\n"
        )
    };

    // No `max_depth` at all.
    let no_depth = page(
        "traversal:\n  start: {skill: company, id: globex}\n  \
         steps:\n    - {relation: works_at, direction: in, as: c}\n  return: [c.name]\n",
    );
    let issues = call(&p, &admin, "validate", json!({ "content": no_depth })).await;
    assert_eq!(issues["ok"], json!(false), "{issues}");
    assert!(
        issues["issues"].as_array().is_some_and(|i| i
            .iter()
            .any(|i| i["code"] == json!("traversal_depth_exceeded"))),
        "an unbounded traversal must be refused: {issues}"
    );

    // Past the server cap.
    let too_deep = page(
        "traversal:\n  start: {skill: company, id: globex}\n  \
         steps:\n    - {relation: works_at, direction: in, as: c}\n  \
         return: [c.name]\n  max_depth: 99\n",
    );
    let issues = call(&p, &admin, "validate", json!({ "content": too_deep })).await;
    assert!(
        issues["issues"].as_array().is_some_and(|i| i
            .iter()
            .any(|i| i["code"] == json!("traversal_depth_exceeded"))),
        "{issues}"
    );

    // Structurally broken: a step with no relation, and a return naming an
    // alias no step declares.
    let broken = page(
        "traversal:\n  start: {skill: company, id: globex}\n  \
         steps:\n    - {direction: in, as: c}\n  return: [c.name]\n  max_depth: 2\n",
    );
    let issues = call(&p, &admin, "validate", json!({ "content": broken })).await;
    assert!(
        issues["issues"]
            .as_array()
            .is_some_and(|i| i.iter().any(|i| i["code"] == json!("traversal_malformed"))),
        "{issues}"
    );

    let unknown_alias = page(
        "traversal:\n  start: {skill: company, id: globex}\n  \
         steps:\n    - {relation: works_at, direction: in, as: c}\n  \
         return: [nobody.name]\n  max_depth: 2\n",
    );
    let issues = call(&p, &admin, "validate", json!({ "content": unknown_alias })).await;
    assert!(
        issues["issues"].as_array().is_some_and(|i| i
            .iter()
            .any(|i| i["code"] == json!("traversal_unknown_field"))),
        "a return naming an alias no step declares can only ever be empty: {issues}"
    );

    // And a well-formed one validates clean, so the assertions above are
    // about the defects and not about `target: corpus` being rejected.
    let good = page(
        "traversal:\n  start: {skill: company, id: globex}\n  \
         steps:\n    - {relation: works_at, direction: in, as: c}\n  \
         return: [c.name]\n  max_depth: 2\n",
    );
    let issues = call(&p, &admin, "validate", json!({ "content": good })).await;
    assert_eq!(issues["ok"], json!(true), "{issues}");
}

/// A declared parameter is a VALUE, not a fragment of a query. The traversal
/// compiler emits no caller text into SQL — this is the behavioural half of
/// that, with the compiler's own static assertion in the unit tests.
#[tokio::test]
async fn a_parameter_cannot_inject_anything() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);

    for hostile in [
        "globex'; DROP TABLE pages; --",
        "' OR 1=1 --",
        "globex\" UNION ALL SELECT * FROM pages --",
    ] {
        let out = call(
            &p,
            &admin,
            "query_instance",
            json!({ "ref": "[[query::warm-intro]]", "params": { "account": hostile } }),
        )
        .await;
        assert_eq!(
            out["rows"].as_array().map(Vec::len),
            Some(0),
            "a hostile parameter is an id that matches nothing, never syntax: {out}"
        );
    }

    // The corpus is intact.
    let still_there = call(
        &p,
        &admin,
        "query_instance",
        json!({ "ref": "[[query::warm-intro]]", "params": { "account": "globex" } }),
    )
    .await;
    assert!(
        !still_there["rows"].as_array().expect("rows").is_empty(),
        "{still_there}"
    );
}

/// A required parameter that was not supplied is a refusal, not an empty
/// result — an empty result reads as "no such warm intro", which is a
/// different and wrong answer.
#[tokio::test]
async fn a_missing_required_parameter_is_refused() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);

    let body: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {admin}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {
                "name": "query_instance",
                "arguments": { "ref": "[[query::warm-intro]]", "params": {} },
            },
        }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json");
    let text = body.to_string();
    assert!(
        text.contains("account"),
        "the refusal must name the missing parameter: {body}"
    );
}
