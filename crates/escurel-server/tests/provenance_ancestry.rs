//! PR-2 (ADR-0010) — `provenance_ancestry`, the bounded multi-hop
//! traversal over `resolved_links`, on the recursive-CTE backend.
//!
//! No mocks: a real gateway + real Indexer + real DuckDB. A three-hop
//! provenance chain is seeded through the real indexer (fixtures →
//! real link extraction), then read back over `/mcp`:
//!
//!   result::r1 --produced_by--> analysis::a1 --uses--> dataset::d1
//!               --derived_from--> dataset::d0
//!
//! Covers: depth labelling, `max_hops` truncation, dangling-link
//! exclusion (the INNER JOIN in `resolved_links` drops a link whose
//! target doesn't exist), the `down` direction, and fail-closed
//! transitive ACL (an unreadable interior node hides its whole subtree).

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "lab";

// Minimal skills. `required_frontmatter: []` — the relation fields are
// undeclared, which is fine: the indexer extracts a link from EVERY
// frontmatter wikilink regardless of declaration.
fn skill(id: &str, extra: &str) -> String {
    format!("---\ntype: skill\nid: {id}\ndescription: {id}.\n{extra}---\n# {id}\n")
}

fn start_fixtures(analysis_owner_private: bool) -> FixtureBuilder {
    let analysis_skill = if analysis_owner_private {
        skill("analysis", "visibility: owner\nowner_field: owner\n")
    } else {
        skill("analysis", "visibility: public\n")
    };
    // r1 also carries a DANGLING derived_from → analysis::ghost (no such
    // page) to prove dangling links never appear in a traversal.
    let r1 = "---\ntype: instance\nskill: result\nid: r1\n\
        produced_by: \"[[analysis::a1]]\"\n\
        derived_from: \"[[analysis::ghost]]\"\n---\n# r1\n";
    let a1 = if analysis_owner_private {
        "---\ntype: instance\nskill: analysis\nid: a1\n\
         owner: \"someone-else\"\nuses: \"[[dataset::d1]]\"\n---\n# a1\n"
    } else {
        "---\ntype: instance\nskill: analysis\nid: a1\n\
         uses: \"[[dataset::d1]]\"\n---\n# a1\n"
    };
    let d1 = "---\ntype: instance\nskill: dataset\nid: d1\n\
        derived_from: \"[[dataset::d0]]\"\n---\n# d1\n";
    let d0 = "---\ntype: instance\nskill: dataset\nid: d0\n---\n# d0\n";

    FixtureBuilder::new()
        .tenant(TENANT)
        .skill("result", skill("result", "visibility: public\n"))
        .skill("analysis", analysis_skill)
        .skill("dataset", skill("dataset", "visibility: public\n"))
        .instance("result", "r1", r1)
        .instance("analysis", "a1", a1)
        .instance("dataset", "d1", d1)
        .instance("dataset", "d0", d0)
        .done()
}

async fn start(analysis_owner_private: bool) -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(start_fixtures(analysis_owner_private)),
        ..Default::default()
    })
    .await
}

async fn ancestry(p: &EscurelProcess, subject: &str, args: Value) -> Vec<Value> {
    let token = p.mint_token_with_sub(TENANT, Role::Agent, subject);
    let resp: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "provenance_ancestry", "arguments": args },
        }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json");
    assert!(resp.get("error").is_none(), "ancestry error: {resp}");
    resp["result"]["structuredContent"]["hops"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// depth of the hop whose page_id contains `id`, or None if absent.
fn depth_of(hops: &[Value], id: &str) -> Option<u64> {
    hops.iter()
        .find(|h| h["page_id"].as_str().unwrap_or_default().contains(id))
        .and_then(|h| h["depth"].as_u64())
}

const ALL: &str = "produced_by,uses,derived_from";

#[tokio::test]
async fn ancestry_walks_the_chain_bounds_hops_and_drops_danglers() {
    let p = start(false).await;
    let relations: Vec<&str> = ALL.split(',').collect();

    // Up from r1 with a generous ceiling: the whole chain, correctly depthed.
    let hops = ancestry(
        &p,
        "reader",
        json!({ "page_id": "markdown/instances/result/r1.md", "direction": "up",
                "relations": relations, "max_hops": 5 }),
    )
    .await;
    assert_eq!(depth_of(&hops, "analysis/a1"), Some(1), "a1 @1: {hops:?}");
    assert_eq!(depth_of(&hops, "dataset/d1"), Some(2), "d1 @2: {hops:?}");
    assert_eq!(depth_of(&hops, "dataset/d0"), Some(3), "d0 @3: {hops:?}");
    // The dangling produced_by → analysis::ghost never resolves, so it is
    // absent (INNER JOIN in resolved_links).
    assert!(
        !hops
            .iter()
            .any(|h| h["page_id"].as_str().unwrap_or_default().contains("ghost")),
        "dangling link excluded: {hops:?}"
    );

    // max_hops=2 truncates the third hop.
    let hops = ancestry(
        &p,
        "reader",
        json!({ "page_id": "markdown/instances/result/r1.md", "direction": "up",
                "relations": relations, "max_hops": 2 }),
    )
    .await;
    assert_eq!(depth_of(&hops, "analysis/a1"), Some(1));
    assert_eq!(depth_of(&hops, "dataset/d1"), Some(2));
    assert_eq!(
        depth_of(&hops, "dataset/d0"),
        None,
        "d0 beyond max_hops: {hops:?}"
    );

    // Down from d0: who derives from it? d1 does (d1 --derived_from--> d0).
    let hops = ancestry(
        &p,
        "reader",
        json!({ "page_id": "markdown/instances/dataset/d0.md", "direction": "down",
                "relations": ["derived_from"], "max_hops": 5 }),
    )
    .await;
    assert_eq!(
        depth_of(&hops, "dataset/d1"),
        Some(1),
        "down reaches d1: {hops:?}"
    );

    p.shutdown().await;
}

#[tokio::test]
async fn ancestry_hides_the_subtree_behind_an_unreadable_node() {
    // analysis is owner-private, a1 owned by "someone-else". A caller who
    // is not the owner can't read a1 — and because the only path to d1/d0
    // runs through a1, the whole subtree is dropped (fail-closed transitive
    // visibility), even though d1/d0 are themselves public.
    let p = start(true).await;
    let relations: Vec<&str> = ALL.split(',').collect();

    let hops = ancestry(
        &p,
        "not-the-owner",
        json!({ "page_id": "markdown/instances/result/r1.md", "direction": "up",
                "relations": relations, "max_hops": 5 }),
    )
    .await;
    for hidden in ["analysis/a1", "dataset/d1", "dataset/d0"] {
        assert_eq!(
            depth_of(&hops, hidden),
            None,
            "{hidden} hidden behind unreadable a1: {hops:?}"
        );
    }

    // The owner still sees the full chain — a1 readable ⇒ subtree visible.
    let hops = ancestry(
        &p,
        "someone-else",
        json!({ "page_id": "markdown/instances/result/r1.md", "direction": "up",
                "relations": relations, "max_hops": 5 }),
    )
    .await;
    assert_eq!(
        depth_of(&hops, "analysis/a1"),
        Some(1),
        "owner sees a1: {hops:?}"
    );
    assert_eq!(
        depth_of(&hops, "dataset/d0"),
        Some(3),
        "owner sees d0: {hops:?}"
    );

    p.shutdown().await;
}
