//! PR-1 (ADR-0010) — the `project-memory` pack proves the whole
//! provenance model against real code with **zero product changes**.
//!
//! No mocks: a real **hub** `EscurelProcess` seeded with the twelve
//! ontology skills exports a signed pack over `/mcp`; a separate
//! **spoke** imports it; then an agent authors the worked-example
//! instances (a customer-churn project whose expectation drifts) through
//! the real `update_page` write path and reads the provenance edges back
//! through `resolve` / `neighbours` / `list_instances`.
//!
//! The pack markdown is `include_str!`d straight from
//! `examples/project-memory/`, so the shipped artefact and the tested
//! artefact are the same bytes — they cannot drift.

use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const HUB: &str = "hub";
const SPOKE: &str = "acme";
const PACK_SECRET: &str = "shared-pack-signing-secret";
const PACK_ID: &str = "project-memory";
const VERTICAL: &str = "data-science";

// --- the twelve pack skills, straight from examples/ -------------------
macro_rules! skill_md {
    ($f:literal) => {
        include_str!(concat!("../../../../examples/project-memory/skills/", $f))
    };
}
/// (skill id, markdown, is_event_typed) — the pinned contract.
const SKILLS: &[(&str, &str, bool)] = &[
    ("project-memory", skill_md!("project-memory.md"), false),
    ("stakeholder", skill_md!("stakeholder.md"), false),
    ("goal", skill_md!("goal.md"), true),
    ("expectation", skill_md!("expectation.md"), true),
    ("constraint", skill_md!("constraint.md"), true),
    ("priority", skill_md!("priority.md"), false),
    (
        "success_criterion",
        skill_md!("success_criterion.md"),
        false,
    ),
    ("hypothesis", skill_md!("hypothesis.md"), true),
    ("dataset", skill_md!("dataset.md"), false),
    ("analysis", skill_md!("analysis.md"), true),
    ("result", skill_md!("result.md"), true),
    ("decision", skill_md!("decision.md"), true),
    ("project", skill_md!("project.md"), false),
    ("conclusion", skill_md!("conclusion.md"), true),
    (
        "project-memory-assistant",
        skill_md!("project-memory-assistant.md"),
        false,
    ),
];

// --- the worked example, in dependency order ---------------------------
macro_rules! inst_md {
    ($f:literal) => {
        include_str!(concat!(
            "../../../../examples/project-memory/instances/",
            $f
        ))
    };
}
/// (skill, id, markdown) authored on the spoke via `update_page`.
const INSTANCES: &[(&str, &str, &str)] = &[
    (
        "stakeholder",
        "marketing-lead",
        inst_md!("stakeholder__marketing-lead.md"),
    ),
    ("priority", "must-have", inst_md!("priority__must-have.md")),
    (
        "success_criterion",
        "churn-auc-80",
        inst_md!("success_criterion__churn-auc-80.md"),
    ),
    ("goal", "reduce-churn", inst_md!("goal__reduce-churn.md")),
    (
        "dataset",
        "customer-events",
        inst_md!("dataset__customer-events.md"),
    ),
    (
        "expectation",
        "churn-v1",
        inst_md!("expectation__churn-v1.md"),
    ),
    (
        "expectation",
        "churn-v2",
        inst_md!("expectation__churn-v2.md"),
    ),
    (
        "hypothesis",
        "gradient-boost",
        inst_md!("hypothesis__gradient-boost.md"),
    ),
    ("analysis", "gbm-run", inst_md!("analysis__gbm-run.md")),
    ("result", "gbm-auc", inst_md!("result__gbm-auc.md")),
    (
        "decision",
        "ship-full-base-model",
        inst_md!("decision__ship-full-base-model.md"),
    ),
];

async fn spawn(fixtures: FixtureBuilder) -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(fixtures),
        config_overrides: ConfigOverrides {
            pack_secret: Some(PACK_SECRET.to_owned()),
            ..Default::default()
        },
    })
    .await
}

async fn call(p: &EscurelProcess, tenant: &str, role: Role, name: &str, args: Value) -> Value {
    let token = p.mint_token(tenant, role);
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

fn sc(env: &Value) -> Value {
    env["result"]["structuredContent"].clone()
}

/// Resolve a `[[skill::id]]` wikilink on the spoke to its canonical page id.
async fn resolve_pid(spoke: &EscurelProcess, wikilink: &str) -> String {
    let r = sc(&call(
        spoke,
        SPOKE,
        Role::Agent,
        "resolve",
        json!({ "wikilink": wikilink }),
    )
    .await);
    assert_eq!(r["exists"], true, "wikilink {wikilink} must resolve: {r}");
    r["page"]["page_id"].as_str().expect("page_id").to_owned()
}

/// The outbound `(link_skill, dst_slug)` edge set of a page.
async fn out_edges(spoke: &EscurelProcess, page_id: &str) -> Vec<(String, String)> {
    let r = sc(&call(
        spoke,
        SPOKE,
        Role::Agent,
        "neighbours",
        json!({ "page_id": page_id, "direction": "out" }),
    )
    .await);
    r["edges"]
        .as_array()
        .expect("edges")
        .iter()
        .map(|e| {
            (
                e["link_skill"].as_str().unwrap_or_default().to_owned(),
                e["dst_page"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect()
}

/// Full end-to-end: import the pack, author the worked example, read the
/// provenance graph back.
#[tokio::test]
async fn project_memory_pack_imports_and_its_provenance_edges_resolve() {
    // --- hub seeded with the twelve skills; spoke empty ---------------
    let mut hub_fx = FixtureBuilder::new().tenant(HUB);
    for (id, md, _) in SKILLS {
        hub_fx = hub_fx.skill(id, *md);
    }
    let hub = spawn(hub_fx.done()).await;
    let spoke = spawn(FixtureBuilder::new().tenant(SPOKE).done()).await;

    // --- export the whole ontology as project-memory@v1 --------------
    let skill_ids: Vec<&str> = SKILLS.iter().map(|(id, _, _)| *id).collect();
    let ex = sc(&call(
        &hub,
        HUB,
        Role::Admin,
        "export_pack",
        json!({
            "tenant_id": HUB, "id": PACK_ID, "version": 1, "vertical": VERTICAL,
            "publisher": "hub.test", "skills": skill_ids, "include_instances": false,
        }),
    )
    .await);
    let manifest = ex["manifest"].clone();
    let tarball_b64 = ex["tarball_b64"].as_str().expect("tarball").to_owned();

    // --- import onto the spoke ---------------------------------------
    let im = sc(&call(
        &spoke,
        SPOKE,
        Role::Admin,
        "import_pack",
        json!({ "tenant_id": SPOKE, "manifest": manifest, "tarball_b64": tarball_b64 }),
    )
    .await);
    assert_eq!(im["pack"], PACK_ID);
    assert_eq!(im["version"], 1);
    assert_eq!(
        im["pages_imported"], 15,
        "fifteen ontology skills imported: {im}"
    );

    // --- list_skills: all twelve, layer-pinned, event-typing correct --
    let listed = sc(&call(&spoke, SPOKE, Role::Agent, "list_skills", json!({})).await);
    let skills = listed["skills"].as_array().expect("skills").clone();
    for (id, _, want_event_typed) in SKILLS {
        let s = skills
            .iter()
            .find(|s| s["id"] == *id)
            .unwrap_or_else(|| panic!("skill `{id}` listed: {listed}"));
        assert_eq!(
            s["layer"], "base@project-memory@v1",
            "skill `{id}` is pinned base layer: {s}"
        );
        assert_eq!(
            s["is_event_typed"], *want_event_typed,
            "skill `{id}` event-typing: {s}"
        );
    }

    // --- author the worked example in the spoke overlay --------------
    for (skill, id, md) in INSTANCES {
        let w = sc(&call(
            &spoke,
            SPOKE,
            Role::Agent,
            "update_page",
            json!({ "page_id": format!("{skill}::{id}"), "content": md }),
        )
        .await);
        assert_eq!(w["ok"], true, "author {skill}::{id}: {w}");
    }

    // --- the decision's provenance spans BOTH graphs -----------------
    // motivated_by → expectation (why), justified_by → result (evidence),
    // addresses → hypothesis, decided_by → stakeholder. neighbours reports
    // edges at (link_skill, dst_slug) granularity — enough to prove the
    // bridge is wired; src_field disambiguation lands in PR-2.
    let decision = resolve_pid(&spoke, "[[decision::ship-full-base-model]]").await;
    let edges = out_edges(&spoke, &decision).await;
    for want in [
        ("expectation", "churn-v1"),
        ("result", "gbm-auc"),
        ("hypothesis", "gradient-boost"),
        ("stakeholder", "marketing-lead"),
    ] {
        assert!(
            edges.iter().any(|(sk, dst)| sk == want.0 && dst == want.1),
            "decision links {want:?}; got {edges:?}"
        );
    }

    // --- knowledge-graph backlink: the result supports the hypothesis -
    // neighbours' link_skill is the TARGET's skill (here `hypothesis`), so
    // inbound edges can't be filtered by SOURCE type — that's exactly what
    // PR-2's resolved_links.relation adds. Here we read inbound unfiltered
    // and check the source page.
    let hypothesis = resolve_pid(&spoke, "[[hypothesis::gradient-boost]]").await;
    let back = sc(&call(
        &spoke,
        SPOKE,
        Role::Agent,
        "neighbours",
        json!({ "page_id": hypothesis, "direction": "in" }),
    )
    .await);
    let srcs: Vec<String> = back["edges"]
        .as_array()
        .expect("edges")
        .iter()
        .map(|e| e["src_page"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert!(
        srcs.iter().any(|s| s.contains("gbm-auc")),
        "result gbm-auc is an inbound neighbour of the hypothesis: {back}"
    );

    // --- expectation drift: v2 supersedes v1, and is the newer head ---
    let v2 = resolve_pid(&spoke, "[[expectation::churn-v2]]").await;
    assert!(
        out_edges(&spoke, &v2)
            .await
            .iter()
            .any(|(sk, dst)| sk == "expectation" && dst == "churn-v1"),
        "churn-v2 supersedes churn-v1"
    );
    let chain = sc(&call(
        &spoke,
        SPOKE,
        Role::Agent,
        "list_instances",
        json!({ "skill_id": "expectation", "order_by": "at desc" }),
    )
    .await);
    let ids: Vec<String> = chain["instances"]
        .as_array()
        .expect("instances")
        .iter()
        .map(|i| {
            i["frontmatter"]["id"]
                .as_str()
                .unwrap_or_default()
                .to_owned()
        })
        .collect();
    assert_eq!(
        ids,
        vec!["churn-v2".to_owned(), "churn-v1".to_owned()],
        "expectation chain is newest-first: {chain}"
    );

    hub.shutdown().await;
    spoke.shutdown().await;
}
