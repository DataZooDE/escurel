//! The `migrate_kind` admin tool through the real gateway: dry run by default, admin-only, the
//! tenant must match, `apply` rewrites the stored pages, and a second `apply` is a no-op.
//! Real gateway, real DuckDB, real OIDC (TestIssuer), real reqwest. No mocks.

use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const LEGACY_SKILL: &str = "---\ntype: skill\nid: customer\ndescription: x\n---\n# customer\n";
const LEGACY_INSTANCE: &str = "---\ntype: instance\nskill: customer\nid: acme-corp\n---\n# Acme\n";

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("customer", LEGACY_SKILL)
                .instance("customer", "acme-corp", LEGACY_INSTANCE)
                .done(),
        ),
        ..Default::default()
    })
    .await
}

async fn call(p: &EscurelProcess, role: Role, name: &str, args: Value) -> Value {
    let token = p.mint_token(TENANT, role);
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

fn report(body: &Value) -> &Value {
    assert!(body.get("error").is_none(), "migrate_kind error: {body}");
    &body["result"]["structuredContent"]
}

fn pages(v: &Value) -> Vec<String> {
    v.as_array()
        .expect("array")
        .iter()
        .map(|p| p.as_str().expect("str").to_owned())
        .collect()
}

#[tokio::test]
async fn migrate_kind_is_a_dry_run_unless_apply_is_set_and_apply_is_idempotent() {
    let p = start().await;

    // No `apply`: a dry run. It reports the legacy pages and writes nothing.
    let dry = call(
        &p,
        Role::Admin,
        "migrate_kind",
        json!({ "tenant_id": TENANT }),
    )
    .await;
    let r = report(&dry);
    assert_eq!(r["applied"], false);
    let would = pages(&r["pages_to_migrate"]);
    assert!(
        would.iter().any(|p| p.ends_with("customer.md"))
            && would.iter().any(|p| p.ends_with("acme-corp.md")),
        "the seeded legacy pages are reported: {would:?}"
    );
    assert!(
        r["audit_event_id"].is_null(),
        "a dry run records no audit event"
    );

    // A second dry run reports exactly the same: nothing was written.
    let dry2 = call(
        &p,
        Role::Admin,
        "migrate_kind",
        json!({ "tenant_id": TENANT }),
    )
    .await;
    assert_eq!(pages(&report(&dry2)["pages_to_migrate"]), would);

    // apply: rewritten, audited.
    let done = call(
        &p,
        Role::Admin,
        "migrate_kind",
        json!({ "tenant_id": TENANT, "apply": true }),
    )
    .await;
    let r = report(&done);
    assert_eq!(r["applied"], true);
    assert_eq!(pages(&r["pages_to_migrate"]), would);
    assert!(
        r["audit_event_id"].is_string(),
        "an applied migration is audited"
    );

    // The migrated page is still served: reads go through the rewritten lane + index.
    let page = call(
        &p,
        Role::Agent,
        "expand",
        json!({ "page_id": would.iter().find(|p| p.ends_with("acme-corp.md")).unwrap() }),
    )
    .await;
    assert!(
        page.get("error").is_none(),
        "expand after migration: {page}"
    );

    // Idempotent: nothing left to migrate.
    let again = call(
        &p,
        Role::Admin,
        "migrate_kind",
        json!({ "tenant_id": TENANT, "apply": true }),
    )
    .await;
    assert!(pages(&report(&again)["pages_to_migrate"]).is_empty());
    p.shutdown().await;
}

#[tokio::test]
async fn migrate_kind_needs_the_admin_role_and_the_right_tenant() {
    let p = start().await;
    let agent = call(
        &p,
        Role::Agent,
        "migrate_kind",
        json!({ "tenant_id": TENANT }),
    )
    .await;
    assert_eq!(
        agent["error"]["code"], -32001,
        "a non-admin is refused: {agent}"
    );
    let foreign = call(
        &p,
        Role::Admin,
        "migrate_kind",
        json!({ "tenant_id": "globex" }),
    )
    .await;
    assert_eq!(
        foreign["error"]["code"], -32002,
        "a foreign tenant is refused: {foreign}"
    );
    p.shutdown().await;
}
