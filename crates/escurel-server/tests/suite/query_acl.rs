//! A stored query is an instance with its own `acl.read` (here: owner-scoped). `expand` of it was
//! denied to a caller who may not read it, but `query_instance` checked only the TARGET's ACL, so the
//! same caller could still RUN it and read its rows. Real gateway, real DuckDB, real tokens.

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const ALICE: &str = "consultant:alice";
const BOB: &str = "consultant:bob";

const QUERY_SKILL: &str = "---\nkind: skill\nid: query\ndescription: A stored query.\n\
    visibility: owner\nowner_field: credential\n---\n# query\n";
const COMPANY_SKILL: &str = "---\nkind: skill\nid: company\ndescription: A company.\n\
    visibility: public\n---\n# company\n";
const CONTACT_SKILL: &str = "---\nkind: skill\nid: contact\ndescription: Someone.\n\
    visibility: public\n---\n# contact\n";

const MINE: &str = "\
---
kind: instance
skill: query
id: mine
credential: \"consultant:alice\"
target: corpus
traversal:
  start: {skill: company, id: globex}
  steps:
    - {relation: works_at, direction: in, as: contact}
  return: [contact.name]
  max_depth: 2
  limit: 50
---
# mine
";

async fn call(p: &EscurelProcess, token: &str, tool: &str, args: Value) -> Value {
    reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": { "name": tool, "arguments": args } }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

#[tokio::test]
async fn running_a_stored_query_needs_read_access_to_the_query_page() {
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("query", QUERY_SKILL)
                .skill("company", COMPANY_SKILL)
                .skill("contact", CONTACT_SKILL)
                .instance(
                    "company",
                    "globex",
                    "---\nkind: instance\nskill: company\nid: globex\nname: Globex\n---\n# g\n",
                )
                .instance(
                    "contact",
                    "wile",
                    "---\nkind: instance\nskill: contact\nid: wile\nname: Wile\nworks_at: \"[[company::globex]]\"\n---\n# w\n",
                )
                .instance("query", "mine", MINE)
                .done(),
        ),
        ..Default::default()
    })
    .await;
    let alice = p.mint_token_with_groups(TENANT, ALICE, &[], false);
    let bob = p.mint_token_with_groups(TENANT, BOB, &[], false);

    // The query page itself is Alice's: Bob cannot expand it ...
    let seen = call(
        &p,
        &bob,
        "expand",
        json!({ "page_id": "markdown/instances/query/mine.md" }),
    )
    .await;
    assert!(
        seen["result"]["structuredContent"]["page"].is_null(),
        "bob must not read alice's query page: {seen}"
    );
    // ... nor run it.
    let ran = call(&p, &bob, "query_instance", json!({ "ref": "mine" })).await;
    let text = ran.to_string();
    assert!(
        text.contains("query_not_found") && !text.contains("Wile"),
        "bob must not run a query page he may not read: {text}"
    );
    // Alice can.
    let mine = call(&p, &alice, "query_instance", json!({ "ref": "mine" })).await;
    assert!(mine.to_string().contains("Wile"), "{mine}");
}
