//! A NARROWED per-run agent token is confined to its skill (workbench backend
//! P3-6, codex second-opinion review of P3): its groups are tenant-wide, so
//! without the `skill` claim a token minted for `renewal` carrying `ops`
//! could write any other skill that also grants `ops`. The write ACL refuses
//! an instance write under any other skill first, before the groups are
//! consulted. Real gateway under `ESCUREL_WRITE_ACL=enforce`, real DuckDB.

use escurel_test_support::{
    AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role, WriteAclMode,
};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const RENEWAL: &str = "---\ntype: skill\nid: renewal\ndescription: d.\n\
acl:\n  create: [ops]\n  update: [ops]\n---\n# renewal\n";
/// Grants the SAME group — the case the skill claim exists for.
const BILLING: &str = "---\ntype: skill\nid: billing\ndescription: d.\n\
acl:\n  create: [ops]\n  update: [ops]\n---\n# billing\n";

fn instance(skill: &str, note: &str) -> String {
    format!("---\ntype: instance\nid: c1\nskill: {skill}\n---\n# C1\n\n{note}\n")
}

fn page(skill: &str) -> String {
    format!("markdown/instances/{skill}/c1.md")
}

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
                .skill("renewal", RENEWAL)
                .skill("billing", BILLING)
                .instance("renewal", "c1", instance("renewal", "BASELINE").as_str())
                .instance("billing", "c1", instance("billing", "BASELINE").as_str())
                .done(),
        ),
    })
    .await
}

async fn update(p: &EscurelProcess, token: &str, page_id: &str, content: &str) -> Value {
    let body: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": { "name": "update_page",
                                   "arguments": { "page_id": page_id, "content": content } } }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json");
    assert!(body.get("error").is_none(), "update_page: {body}");
    body["result"]["structuredContent"].clone()
}

#[tokio::test]
async fn a_narrowed_token_writes_its_own_skill_and_no_other_even_with_a_shared_group() {
    let p = start().await;
    let renewal_agent = p.mint_token_narrowed(TENANT, "renewal", &["ops"]);

    let r = update(
        &p,
        &renewal_agent,
        &page("renewal"),
        &instance("renewal", "edited"),
    )
    .await;
    assert_eq!(r["ok"], true, "its own skill, granted by `ops`: {r}");

    let r = update(
        &p,
        &renewal_agent,
        &page("billing"),
        &instance("billing", "edited"),
    )
    .await;
    assert_eq!(
        r["ok"], false,
        "billing also grants `ops`, but the token is renewal's: {r}"
    );
    assert_eq!(r["issues"][0]["code"], "forbidden", "{r}");

    // Admin is never confined.
    let admin = p.mint_token(TENANT, Role::Admin);
    let r = update(
        &p,
        &admin,
        &page("billing"),
        &instance("billing", "by admin"),
    )
    .await;
    assert_eq!(r["ok"], true, "{r}");
}
