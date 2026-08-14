//! Project / sub-project / conclusion lifecycle over the shipped tools —
//! no new engine code, just the `project` + `conclusion` skills and the
//! existing `neighbours` / `provenance_ancestry` / `expand` /
//! `provenance_report(kind: "abandoned")` surface.
//!
//! Shape:
//!   project::churn  (active)
//!     ├─ project::p1 (closed, concluded_by conclusion::c1)
//!     └─ project::p2 (closed, concluded_by conclusion::c2)
//!   conclusion::c2 supersedes conclusion::c1
//!   hypothesis::downstream  builds_on conclusion::c2  (+ scope project::p2)
//!
//! Proves: containment (part_of), closing (status + concluded_by), reuse
//! (provenance_ancestry down via builds_on), scope, and retirement of an
//! overturned conclusion (provenance_report kind=abandoned).

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "proj";

fn skill(id: &str) -> String {
    // required_frontmatter:[] — author instances freely with just the
    // relation fields the lifecycle needs.
    format!("---\ntype: skill\nid: {id}\ndescription: {id}.\nvisibility: public\n---\n# {id}\n")
}

fn start() -> FixtureBuilder {
    FixtureBuilder::new()
        .tenant(TENANT)
        .skill("project", skill("project"))
        .skill("conclusion", skill("conclusion"))
        .skill("hypothesis", skill("hypothesis"))
        .instance(
            "project",
            "churn",
            "---\ntype: instance\nskill: project\nid: churn\ntitle: Churn\nstatus: active\n---\n# churn\n",
        )
        .instance(
            "project",
            "p1",
            "---\ntype: instance\nskill: project\nid: p1\ntitle: Phase 1\n\
             part_of: \"[[project::churn]]\"\nstatus: closed\n\
             concluded_by: \"[[conclusion::c1]]\"\n---\n# p1\n",
        )
        .instance(
            "project",
            "p2",
            "---\ntype: instance\nskill: project\nid: p2\ntitle: Phase 2\n\
             part_of: \"[[project::churn]]\"\nstatus: closed\n\
             concluded_by: \"[[conclusion::c2]]\"\n---\n# p2\n",
        )
        .instance(
            "conclusion",
            "c1",
            "---\ntype: instance\nskill: conclusion\nid: c1\nat: 2026-02-10T00:00:00Z\n\
             concludes: \"[[project::p1]]\"\n---\n# c1\n",
        )
        .instance(
            "conclusion",
            "c2",
            "---\ntype: instance\nskill: conclusion\nid: c2\nat: 2026-04-15T00:00:00Z\n\
             concludes: \"[[project::p2]]\"\nsupersedes: \"[[conclusion::c1]]\"\n---\n# c2\n",
        )
        .instance(
            "hypothesis",
            "downstream",
            "---\ntype: instance\nskill: hypothesis\nid: downstream\n\
             builds_on: \"[[conclusion::c2]]\"\nscope: \"[[project::p2]]\"\n---\n# downstream\n",
        )
        .done()
}

async fn spawn() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(start()),
        ..Default::default()
    })
    .await
}

async fn call(p: &EscurelProcess, name: &str, args: Value) -> Value {
    let token = p.mint_token(TENANT, Role::Agent);
    let resp: Value = reqwest::Client::new()
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
        .expect("json");
    assert!(resp.get("error").is_none(), "{name} error: {resp}");
    resp["result"]["structuredContent"].clone()
}

const CHURN: &str = "markdown/instances/project/churn.md";
const P1: &str = "markdown/instances/project/p1.md";
const P2: &str = "markdown/instances/project/p2.md";
const C2: &str = "markdown/instances/conclusion/c2.md";

fn inbound_srcs(sc: &Value) -> Vec<String> {
    sc["edges"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .map(|e| e["src_page"].as_str().unwrap_or_default().to_owned())
        .collect()
}

#[tokio::test]
async fn subprojects_close_with_conclusions_that_carry_forward() {
    let p = spawn().await;

    // 1. Containment: the parent rolls up its sub-projects via part_of.
    let subs = inbound_srcs(
        &call(
            &p,
            "neighbours",
            json!({ "page_id": CHURN, "direction": "in", "link_skill": "project" }),
        )
        .await,
    );
    assert!(
        subs.iter().any(|s| s == P1),
        "p1 is part_of churn: {subs:?}"
    );
    assert!(
        subs.iter().any(|s| s == P2),
        "p2 is part_of churn: {subs:?}"
    );

    // 2. Closing: the sub-project is status=closed and names its conclusion.
    let ex = call(&p, "expand", json!({ "page_id": P1 })).await;
    assert_eq!(ex["frontmatter"]["status"], "closed", "p1 closed: {ex}");
    let out = call(
        &p,
        "neighbours",
        json!({ "page_id": P1, "direction": "out" }),
    )
    .await;
    assert!(
        out["edges"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["link_skill"] == "conclusion" && e["dst_page"] == "c1"),
        "p1 concluded_by c1: {out}"
    );

    // 3. Reuse: everything downstream that builds_on the phase-2 conclusion.
    let down = call(
        &p,
        "provenance_ancestry",
        json!({ "page_id": C2, "direction": "down", "relations": ["builds_on"], "max_hops": 5 }),
    )
    .await;
    assert!(
        down["hops"].as_array().unwrap().iter().any(|h| h["page_id"]
            .as_str()
            .unwrap_or_default()
            .contains("hypothesis/downstream")),
        "the downstream hypothesis builds_on c2: {down}"
    );

    // 4. Scope: a work item sits inside its project.
    let in_p2 = inbound_srcs(
        &call(
            &p,
            "neighbours",
            json!({ "page_id": P2, "direction": "in", "link_skill": "project" }),
        )
        .await,
    );
    assert!(
        in_p2.iter().any(|s| s.contains("hypothesis/downstream")),
        "downstream is scoped to p2: {in_p2:?}"
    );

    // 5. Retirement: c1 was superseded by c2, so it surfaces as abandoned;
    //    c2 (the current head) does not.
    let ab = call(
        &p,
        "provenance_report",
        json!({ "kind": "abandoned", "skill": "conclusion" }),
    )
    .await;
    let nodes: Vec<String> = ab["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["page_id"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert!(
        nodes.iter().any(|n| n.contains("conclusion/c1")),
        "c1 retired: {nodes:?}"
    );
    assert!(
        !nodes.iter().any(|n| n.contains("conclusion/c2")),
        "c2 is current: {nodes:?}"
    );

    p.shutdown().await;
}
