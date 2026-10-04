//! The wire rename `page_type` -> `page_kind`: tools answer with `page_kind`, `search` filters by
//! it, and a caller still sending the removed `page_type` is refused loudly instead of silently
//! searching every page (a silent filter drop is the failure a hard cut must not have).
//! Real gateway, real DuckDB, real OIDC. No mocks.

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "acme";

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill(
                    "customer",
                    "---\nkind: skill\nid: customer\ndescription: A buyer.\n---\n# customer\n",
                )
                .instance(
                    "customer",
                    "acme-corp",
                    "---\nkind: instance\nskill: customer\nid: acme-corp\n---\n# Acme\n\nAcme buys.\n",
                )
                .done(),
        ),
        ..Default::default()
    })
    .await
}

async fn call(p: &EscurelProcess, name: &str, args: Value) -> Value {
    let token = p.mint_token(TENANT, Role::Agent);
    reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": args },
        }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json")
}

#[tokio::test]
async fn search_filters_by_page_kind_and_answers_with_it() {
    let p = start().await;
    let r = call(
        &p,
        "search",
        json!({ "q": "acme", "page_kind": "instance" }),
    )
    .await;
    let hits = r["result"]["structuredContent"]["hits"]
        .as_array()
        .expect("hits");
    assert!(!hits.is_empty(), "{r}");
    for h in hits {
        assert_eq!(h["page_kind"], "instance", "{h}");
        assert!(
            h.get("page_type").is_none(),
            "the old key is gone from the wire: {h}"
        );
    }
    let skills = call(
        &p,
        "search",
        json!({ "q": "customer", "page_kind": "skill" }),
    )
    .await;
    for h in skills["result"]["structuredContent"]["hits"]
        .as_array()
        .expect("hits")
    {
        assert_eq!(h["page_kind"], "skill", "{h}");
    }
}

#[tokio::test]
async fn expand_and_resolve_answer_with_page_kind() {
    let p = start().await;
    let e = call(
        &p,
        "expand",
        json!({ "page_id": "markdown/instances/customer/acme-corp.md" }),
    )
    .await;
    let page = &e["result"]["structuredContent"]["page"];
    assert_eq!(page["page_kind"], "instance", "{e}");
    assert!(page.get("page_type").is_none(), "{page}");
}

#[tokio::test]
async fn a_caller_still_sending_page_type_is_refused_naming_the_rename() {
    let p = start().await;
    let r = call(
        &p,
        "search",
        json!({ "q": "acme", "page_type": "instance" }),
    )
    .await;
    let err = &r["error"];
    assert!(
        !err.is_null(),
        "must be refused, not silently searched unfiltered: {r}"
    );
    let msg = err["message"].as_str().unwrap_or_default();
    assert!(msg.contains("page_kind"), "names the replacement: {msg}");
    assert!(msg.contains("page_type"), "names what was removed: {msg}");
}
