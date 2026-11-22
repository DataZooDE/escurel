//! Search hygiene: keys a skill page puts where they do nothing (#861), the
//! reserved `escurel-` skill-id prefix (#857), and system skills out of default
//! search (#845). Real gateway, real HTTP.

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "acme";

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(FixtureBuilder::new().tenant(TENANT).done()),
        ..Default::default()
    })
    .await
}

async fn rpc(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    let v: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": { "name": name, "arguments": args } }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json");
    v["result"]["structuredContent"].clone()
}

fn codes(v: &Value) -> Vec<String> {
    v["issues"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|i| i["code"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn deliveries(extra_top: &str, extra_backend: &str) -> String {
    format!(
        "---\nkind: skill\nid: deliveries\ndescription: Deliveries.\nbackend:\n  kind: sql_view\n  \
         source: {{ connector: parquet_dir, relation: /unused }}\n{extra_backend}{extra_top}---\n# deliveries\n"
    )
}

/// #861: `search_text` / `project` / `projection_limit` beside `backend:` are
/// ignored by the binding, so the lab's SQL search was empty for weeks without a
/// word. They are refused where they are written; nested under `backend:` they
/// are accepted.
#[tokio::test]
async fn backend_keys_beside_the_backend_block_are_refused() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);
    for key in [
        "search_text: [supplier]\n",
        "project: { name: supplier }\n",
        "projection_limit: 20\n",
    ] {
        let page = deliveries(key, "");
        let v = rpc(&p, &admin, "validate", json!({ "content": page })).await;
        assert!(
            codes(&v).contains(&"misplaced_backend_key".to_owned()),
            "validate {key}: {v}"
        );
        let w = rpc(
            &p,
            &admin,
            "update_page",
            json!({ "page_id": "markdown/skills/deliveries.md", "content": page }),
        )
        .await;
        assert_eq!(w["ok"], false, "update_page stored {key}: {w}");
        assert!(
            codes(&w).contains(&"misplaced_backend_key".to_owned()),
            "{w}"
        );
    }
    let nested = deliveries("", "  search_text: [supplier]\n");
    let w = rpc(
        &p,
        &admin,
        "update_page",
        json!({ "page_id": "markdown/skills/deliveries.md", "content": nested }),
    )
    .await;
    assert_eq!(w["ok"], true, "the nested form is the right one: {w}");
}

/// #857: `escurel-` is reserved for the skills escurel ships. A tenant skill
/// with that prefix is hidden by every consumer that filters system skills by
/// prefix (datazoo-agent-template #368), so it is refused, for an admin too.
#[tokio::test]
async fn a_skill_id_under_the_reserved_prefix_is_refused() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);
    let page = "---\nkind: skill\nid: escurel-notes\ndescription: Notes.\n---\n# escurel-notes\n";
    let w = rpc(
        &p,
        &admin,
        "update_page",
        json!({ "page_id": "markdown/skills/escurel-notes.md", "content": page }),
    )
    .await;
    assert_eq!(w["ok"], false, "{w}");
    assert!(codes(&w).contains(&"skill_id_reserved".to_owned()), "{w}");
    // An ordinary id is fine.
    let ok = rpc(
        &p,
        &admin,
        "update_page",
        json!({ "page_id": "markdown/skills/notes.md",
                "content": "---\nkind: skill\nid: notes\ndescription: Notes.\n---\n# notes\n" }),
    )
    .await;
    assert_eq!(ok["ok"], true, "{ok}");
}

/// #845: the system skills are seeded into every tenant and outranked tenant
/// content in a free-text search. Default search leaves them out (before the
/// top-k cut, so k stays filled); naming one with `skill:` brings it back.
#[tokio::test]
async fn system_skills_stay_out_of_default_search() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);
    let agent = p.mint_token(TENANT, Role::Agent);
    let w = rpc(
        &p,
        &admin,
        "update_page",
        json!({ "page_id": "markdown/skills/customer.md",
                "content": "---\nkind: skill\nid: customer\ndescription: A customer account.\n---\n# customer\n" }),
    )
    .await;
    assert_eq!(w["ok"], true, "{w}");
    for (id, body) in [
        (
            "acme-corp",
            "Acme Corp is an EU widgets customer with three open orders.",
        ),
        ("initech", "Initech buys staplers and TPS report covers."),
    ] {
        let w = rpc(
            &p,
            &admin,
            "update_page",
            json!({ "page_id": format!("markdown/instances/customer/{id}.md"),
                    "content": format!("---\nkind: instance\nskill: customer\nid: {id}\n---\n# {id}\n{body}\n") }),
        )
        .await;
        assert_eq!(w["ok"], true, "{w}");
    }
    let system = [
        "escurel",
        "escurel-agent",
        "escurel-runtime",
        "escurel-chat-channel",
    ];
    for q in [
        "acme widgets",
        "skill agent runtime channel",
        "how do I write a skill page",
    ] {
        let s = rpc(&p, &agent, "search", json!({ "q": q, "k": 5 })).await;
        let hits = s["hits"].as_array().cloned().unwrap_or_default();
        let leaked: Vec<&str> = hits
            .iter()
            .filter_map(|h| h["skill"].as_str())
            .filter(|s| system.contains(s))
            .collect();
        assert!(
            leaked.is_empty(),
            "{q:?} returned system skills {leaked:?}: {s}"
        );
    }
    // The top-k is filled with tenant content now. (Not asserted: that acme-corp
    // ranks FIRST. A page written after boot is not in the BM25 index until the
    // next FTS refresh, so it competes on the vector arm alone; that is its own
    // issue, not #845.)
    let s = rpc(&p, &agent, "search", json!({ "q": "acme widgets", "k": 5 })).await;
    assert!(
        s["hits"].as_array().is_some_and(|h| h
            .iter()
            .any(|h| h["page_id"] == "markdown/instances/customer/acme-corp.md")),
        "the tenant page is in the top 5: {s}"
    );
    // Asked for by name, the meta skill is still searchable.
    let s = rpc(
        &p,
        &agent,
        "search",
        json!({ "q": "skill", "k": 5, "skill": "escurel", "page_kind": "skill" }),
    )
    .await;
    assert!(
        s["hits"]
            .as_array()
            .is_some_and(|h| h.iter().any(|h| h["skill"] == "escurel")),
        "skill: escurel finds the meta skill: {s}"
    );
}
