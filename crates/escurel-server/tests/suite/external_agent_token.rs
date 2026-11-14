//! External agent tokens (#829): an admin issues a revocable bearer for an
//! agent that runs OUTSIDE escurel (Claude Code, Codex, Multica) as an agent
//! profile. The agent page's `scopes` confine it exactly as they confine a
//! run; its `token_epoch` revokes every token issued before it from the next
//! call on.
//!
//! Real gateway (test issuer, gateway signing on the issuer's key,
//! `ESCUREL_WRITE_ACL=enforce`), a real `escurel-client` MCP session with the
//! issued token, raw JSON-RPC for the refusals.

use escurel_client::{Client, SecretString, UpdatePageRequest};
use escurel_test_support::{
    AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role, WriteAclMode,
};
use serde_json::{Value, json};

const TENANT: &str = "acme";

fn skill(id: &str) -> String {
    format!(
        "---\nkind: skill\nid: {id}\ndescription: {id}\n\
         acl:\n  create: [ops]\n  update: [ops]\n---\n# {id}\n"
    )
}

fn instance(skill: &str) -> String {
    format!("---\nkind: instance\nid: c1\nskill: {skill}\n---\n# C1\n\nBASELINE.\n")
}

const SCRIBE: &str = "---\nkind: instance\nskill: agent\nid: scribe\n\
    skills: [\"[[skill::note]]\"]\n\
    scopes:\n  tools: [whoami, expand, update_page]\n  write: [\"markdown/instances/note/*\"]\n\
    ---\n# scribe\n";
const RETIRED: &str =
    "---\nkind: instance\nskill: agent\nid: retired\narchived: true\n---\n# retired\n";
const SCRIBE_PAGE: &str = "markdown/instances/agent/scribe.md";

async fn gateway() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            signing: true,
            write_acl: Some(WriteAclMode::Enforce),
            ..Default::default()
        },
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("note", skill("note").as_str())
                .skill("customer", skill("customer").as_str())
                .instance("note", "c1", instance("note").as_str())
                .instance("customer", "c1", instance("customer").as_str())
                .instance("agent", "scribe", SCRIBE)
                .instance("agent", "retired", RETIRED)
                .done(),
        ),
    })
    .await
}

async fn rpc(p: &EscurelProcess, token: &str, method: &str, params: Value) -> Value {
    reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params }))
        .send()
        .await
        .expect("post")
        .json()
        .await
        .expect("json")
}

async fn tool(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    rpc(
        p,
        token,
        "tools/call",
        json!({ "name": name, "arguments": args }),
    )
    .await
}

fn admin(p: &EscurelProcess) -> String {
    p.mint_token_with_sub(TENANT, Role::Admin, "ops:jo")
}

async fn issue(p: &EscurelProcess, agent: &str) -> Value {
    let body = tool(p, &admin(p), "issue_agent_token", json!({ "agent": agent })).await;
    assert!(body.get("error").is_none(), "{body}");
    body["result"]["structuredContent"].clone()
}

fn claims_of(token: &str) -> Value {
    use base64::Engine as _;
    let payload = token.split('.').nth(1).expect("jwt");
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .expect("b64");
    serde_json::from_slice(&bytes).expect("json")
}

fn code(body: &Value) -> &str {
    body["error"]["data"]["code"].as_str().unwrap_or("")
}

#[tokio::test]
async fn an_issued_token_runs_as_the_agent_inside_its_scopes() {
    let p = gateway().await;
    let issued = issue(&p, "scribe").await;
    let token = issued["token"].as_str().unwrap().to_owned();
    let c = claims_of(&token);
    assert_eq!(c["sub"], "agent:scribe", "{c}");
    assert_eq!(c["act"]["sub"], "ops:jo", "{c}");
    assert_eq!(c["agent"], "scribe", "{c}");
    assert_eq!(c["roles"], json!(["escurel:agent", "ops"]), "{c}");
    assert!(c.get("run_id").is_none() && c.get("skill").is_none(), "{c}");
    let life = c["exp"].as_u64().unwrap() - c["iat"].as_u64().unwrap();
    assert_eq!(life, 30 * 24 * 3600, "the default life is 30 days: {c}");
    assert_eq!(issued["subject"], "agent:scribe");

    // `tools/list` offers exactly the agent's `scopes.tools`.
    let listed = rpc(&p, &token, "tools/list", json!({})).await;
    let mut names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    names.sort_unstable();
    assert_eq!(names, vec!["expand", "update_page", "whoami"], "{listed}");

    // A real MCP client session: in scope works, as the agent.
    let client = Client::connect(p.base_url(), SecretString::from(token.clone()))
        .await
        .expect("connect");
    assert_eq!(client.whoami().await.unwrap().subject, "agent:scribe");
    let wrote = client
        .update_page(UpdatePageRequest {
            page_id: "markdown/instances/note/c1.md".into(),
            content: "---\nkind: instance\nid: c1\nskill: note\n---\n# C1\n\nBY SCRIBE.\n".into(),
            ..Default::default()
        })
        .await
        .expect("update_page");
    assert!(wrote.ok, "{wrote:?}");

    // Out of scope: a write outside `scopes.write`, a tool outside `scopes.tools`.
    let body = tool(
        &p,
        &token,
        "update_page",
        json!({ "page_id": "markdown/instances/customer/c1.md",
                "content": "---\nkind: instance\nid: c1\nskill: customer\n---\n# C1\n\nNO.\n" }),
    )
    .await;
    assert!(
        body.to_string().contains("agent_scope_denied"),
        "a write outside scopes.write: {body}"
    );
    let body = tool(&p, &token, "search", json!({ "q": "baseline" })).await;
    assert_eq!(code(&body), "agent_scope_denied", "{body}");

    // Not a minter: the token cannot issue or mint another.
    let body = tool(
        &p,
        &token,
        "issue_agent_token",
        json!({ "agent": "scribe" }),
    )
    .await;
    assert!(body.get("error").is_some(), "{body}");
}

#[tokio::test]
async fn the_token_epoch_revokes_from_the_next_call_and_a_later_token_stands() {
    let p = gateway().await;
    let before = issue(&p, "scribe").await["token"]
        .as_str()
        .unwrap()
        .to_owned();
    let ok = tool(&p, &before, "whoami", json!({})).await;
    assert!(ok.get("error").is_none(), "{ok}");

    // Revoke as `escurel agent token revoke` does: `token_epoch` = the next second.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let revoked = SCRIBE.replacen(
        "---\n# scribe",
        &format!("token_epoch: {}\n---\n# scribe", now + 1),
        1,
    );
    let body = tool(
        &p,
        &admin(&p),
        "update_page",
        json!({ "page_id": SCRIBE_PAGE, "content": revoked }),
    )
    .await;
    assert_eq!(body["result"]["structuredContent"]["ok"], true, "{body}");

    let body = tool(&p, &before, "whoami", json!({})).await;
    assert_eq!(code(&body), "agent_token_revoked", "{body}");
    let body = rpc(&p, &before, "tools/list", json!({})).await;
    assert_eq!(code(&body), "agent_token_revoked", "{body}");

    // Issued after the revoke, within the same second: it stands.
    let after = issue(&p, "scribe").await["token"]
        .as_str()
        .unwrap()
        .to_owned();
    let body = tool(&p, &after, "whoami", json!({})).await;
    assert!(body.get("error").is_none(), "{body}");

    // A token with no `iat` cannot be placed against the epoch.
    let undated = p.resign_token_with_claim(&after, "iat", Value::Null);
    let body = tool(&p, &undated, "whoami", json!({})).await;
    assert_eq!(code(&body), "agent_token_revoked", "{body}");
}

#[tokio::test]
async fn only_an_admin_issues_and_only_for_a_live_agent() {
    let p = gateway().await;
    let member = p.mint_token_with_sub(TENANT, Role::Agent, "member:alice");
    let body = tool(
        &p,
        &member,
        "issue_agent_token",
        json!({ "agent": "scribe" }),
    )
    .await;
    assert_eq!(code(&body), "admin_required", "{body}");
    let run = p.mint_token_for_run(TENANT, Role::Admin, "agent:note", "run-1", "root-1");
    let body = tool(&p, &run, "issue_agent_token", json!({ "agent": "scribe" })).await;
    assert_eq!(code(&body), "agent_scope_denied", "{body}");
    let runner = p.mint_token_with_groups(TENANT, "runner:hq", &["escurel:runner"], false);
    let body = tool(
        &p,
        &runner,
        "issue_agent_token",
        json!({ "agent": "scribe" }),
    )
    .await;
    assert_eq!(
        code(&body),
        "admin_required",
        "a runner is not an admin: {body}"
    );
    for agent in ["ghost", "retired", "../scribe"] {
        let body = tool(
            &p,
            &admin(&p),
            "issue_agent_token",
            json!({ "agent": agent }),
        )
        .await;
        assert_eq!(code(&body), "agent_unknown", "{agent}: {body}");
    }
    for ttl in [60, 91 * 24 * 3600] {
        let body = tool(
            &p,
            &admin(&p),
            "issue_agent_token",
            json!({ "agent": "scribe", "ttl_secs": ttl }),
        )
        .await;
        assert_eq!(body["error"]["code"], -32602, "ttl {ttl}: {body}");
    }
}
