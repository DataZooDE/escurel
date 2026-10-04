//! A READ tool's refusal must reach the caller as an error, never as a successful result with
//! empty fields.
//!
//! Since the MCP-usability stream, a domain refusal is an MCP result with `isError: true` and
//! `structuredContent: {ok: false, issues: [...]}`. `call_typed` used to unwrap `structuredContent`
//! and deserialise it into the response type regardless of `isError`, so a refused read decoded into
//! a type whose fields are all `#[serde(default)]`: `Ok(response with nothing in it)`, a SILENT
//! PARTIAL READ (found by peacock's `acl_denial_is_a_typed_error_not_a_partial_read`).
//!
//! Real gateway over HTTP via `escurel-test-support`, real client, no mocks (CLAUDE principle 2).

use escurel_client::{
    Client, Error, ListInstancesRequest, QueryInstanceRequest, SecretString, UpdatePageRequest,
};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::json;

const TENANT: &str = "acme";

const CUSTOMER_SKILL: &str = "---\nkind: skill\nid: customer\ndescription: x\nrequired_frontmatter: [id, name]\n---\n# customer\n";
const ACME: &str = "---\nkind: instance\nskill: customer\nid: acme\nname: Acme\n---\n# Acme\n";

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("customer", CUSTOMER_SKILL)
                .instance("customer", "acme", ACME)
                .done(),
        ),
        config_overrides: ConfigOverrides::default(),
    })
    .await
}

async fn client(p: &EscurelProcess, role: Role) -> Client {
    Client::connect(p.base_url(), SecretString::from(p.mint_token(TENANT, role)))
        .await
        .unwrap()
}

fn refused_codes(e: &Error) -> Vec<String> {
    match e {
        Error::Refused(r) => r.issues.iter().map(|i| i.code.clone()).collect(),
        other => panic!("expected Error::Refused, got {other:?}"),
    }
}

#[tokio::test]
async fn a_refused_read_is_an_error_not_an_empty_result() {
    let p = start().await;
    let c = client(&p, Role::Agent).await;
    let err = c
        .list_instances(ListInstancesRequest {
            skill: "customer".to_owned(),
            limit: 10_001,
            ..Default::default()
        })
        .await
        .expect_err("limit 10001 is refused: it must not decode into an empty page");
    assert_eq!(refused_codes(&err), vec!["invalid_limit".to_owned()]);
    // The words an agent or a person acts on survive the client boundary.
    let Error::Refused(refusal) = &err else {
        unreachable!()
    };
    assert!(
        refusal.issues[0].message.contains("10000"),
        "the message carries the range: {:?}",
        refusal.issues[0]
    );
    assert!(err.to_string().contains("invalid_limit"), "{err}");
    p.shutdown().await;
}

#[tokio::test]
async fn an_unknown_query_ref_is_an_error_not_a_null_row_set() {
    let p = start().await;
    let c = client(&p, Role::Agent).await;
    let err = c
        .query_instance(QueryInstanceRequest {
            query_ref: "[[query::nope]]".to_owned(),
            params: json!({}),
        })
        .await
        .expect_err("a query page that does not exist is refused");
    assert_eq!(refused_codes(&err), vec!["query_not_found".to_owned()]);
    p.shutdown().await;
}

#[tokio::test]
async fn a_successful_read_is_unchanged() {
    let p = start().await;
    let c = client(&p, Role::Agent).await;
    let page = c
        .list_instances(ListInstancesRequest {
            skill: "customer".to_owned(),
            limit: 10,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(page.instances.len(), 1);
    p.shutdown().await;
}

/// The WRITE family models `ok`/`issues` in its response, so a refused write still comes back as
/// `Ok(response)` with `ok: false` and the issues: callers already branch on that and must keep working.
#[tokio::test]
async fn a_refused_write_still_carries_its_issues_in_the_typed_response() {
    let p = start().await;
    let c = client(&p, Role::Agent).await;
    let resp = c
        .update_page(UpdatePageRequest {
            page_id: "markdown/instances/customer/legacy.md".to_owned(),
            content: "---\ntype: instance\nskill: customer\nid: legacy\nname: L\n---\n# L\n"
                .to_owned(),
            ..Default::default()
        })
        .await
        .expect("a write refusal is a typed response, not an Err");
    assert!(!resp.ok);
    assert_eq!(resp.issues[0].code, "frontmatter_type_removed");
    p.shutdown().await;
}

/// An admin-only tool called by an agent is a JSON-RPC refusal: already typed, must stay typed.
#[tokio::test]
async fn an_admin_gate_stays_a_typed_jsonrpc_error() {
    let p = start().await;
    let c = client(&p, Role::Agent).await;
    let admin = escurel_client::AdminClient::connect(
        p.base_url(),
        SecretString::from(p.mint_token(TENANT, Role::Agent)),
    )
    .await
    .unwrap();
    let _ = c;
    let err = admin
        .tenant_list(escurel_client::TenantListRequest::default())
        .await
        .expect_err("agent role is not an admin");
    assert!(matches!(err, Error::JsonRpc { .. }), "{err:?}");
    p.shutdown().await;
}
