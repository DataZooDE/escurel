//! MySQL is off the supported surface (owner decision 2026-10-09): it has no test, no connect/statement
//! timeouts and was never DNS-pinned like Postgres. A gateway that advertises nothing must also REFUSE it,
//! so nobody builds on a path nobody exercises. Real gateway, real HTTP.

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
    reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": { "name": name, "arguments": args } }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json")
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

fn skill_with(connector: &str) -> String {
    format!(
        "---\nkind: skill\nid: orders\ndescription: orders.\nbackend:\n  kind: sql_view\n  source:\n    \
         connector: {connector}\n    attach: erp\n    relation: sales.orders\n---\n# orders\n"
    )
}

#[tokio::test]
async fn a_mysql_credential_is_refused_and_not_stored() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);
    for connector in ["mysql", "mariadb", "MySQL"] {
        let out = rpc(
            &p,
            &admin,
            "register_credential",
            json!({ "name": "erp", "connector": connector, "secret_ref": "gsm:erp" }),
        )
        .await;
        let r = &out["result"]["structuredContent"];
        assert_eq!(r["ok"], false, "{connector}: {out}");
        assert!(
            codes(r).contains(&"connector_not_supported".to_owned()),
            "{connector}: {out}"
        );
    }
    let listed = rpc(&p, &admin, "list_credentials", json!({})).await;
    assert_eq!(
        listed["result"]["structuredContent"]["credentials"],
        json!([]),
        "a refused credential must not be stored: {listed}"
    );
    // Postgres is untouched.
    let pg = rpc(
        &p,
        &admin,
        "register_credential",
        json!({ "name": "erp", "connector": "postgres", "secret_ref": "gsm:erp" }),
    )
    .await;
    assert_eq!(pg["result"]["structuredContent"]["ok"], true, "{pg}");
}

#[tokio::test]
async fn a_skill_page_with_a_mysql_source_cannot_be_written() {
    let p = start().await;
    let admin = p.mint_token(TENANT, Role::Admin);
    let v = rpc(
        &p,
        &admin,
        "validate",
        json!({ "content": skill_with("mysql"), "as_page_id": "markdown/skills/orders.md" }),
    )
    .await;
    assert!(
        codes(&v["result"]["structuredContent"]).contains(&"connector_not_supported".to_owned()),
        "validate must name the unsupported connector: {v}"
    );
    let w = rpc(
        &p,
        &admin,
        "update_page",
        json!({ "page_id": "markdown/skills/orders.md", "content": skill_with("mysql") }),
    )
    .await;
    let r = &w["result"]["structuredContent"];
    assert_eq!(r["ok"], false, "the skill page must not be stored: {w}");
    assert!(
        codes(r).contains(&"connector_not_supported".to_owned()),
        "{w}"
    );
    // A postgres source on the same page is fine.
    let ok = rpc(
        &p,
        &admin,
        "update_page",
        json!({ "page_id": "markdown/skills/orders.md", "content": skill_with("postgres") }),
    )
    .await;
    assert_ne!(ok["result"]["structuredContent"]["ok"], false, "{ok}");
}
