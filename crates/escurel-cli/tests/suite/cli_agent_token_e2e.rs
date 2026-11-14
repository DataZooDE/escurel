//! `escurel agent token issue|revoke` (#829): an admin issues an external
//! agent's token from the CLI, the token works as the agent, and `revoke`
//! (the page's `token_epoch`, through `update_page`) refuses it from the next
//! call on while keeping the page's other keys.
//!
//! Real gateway (test issuer, gateway signing on), the compiled binary.

use assert_cmd::Command;
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const NOTE: &str = "---\nkind: skill\nid: note\ndescription: n\n\
    acl:\n  create: [ops]\n  update: [ops]\n---\n# note\n";
const SCRIBE: &str = "---\nkind: instance\nskill: agent\nid: scribe\n\
    skills: [\"[[skill::note]]\"]\n\
    scopes:\n  tools: [whoami, expand]\n  write: [\"markdown/instances/note/*\"]\n\
    ---\n# scribe\n\nTakes notes.\n";

async fn gateway() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            signing: true,
            ..Default::default()
        },
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("note", NOTE)
                .instance("agent", "scribe", SCRIBE)
                .done(),
        ),
    })
    .await
}

/// Run the CLI as `token`; `(success, stdout JSON or stderr JSON)`.
async fn cli(p: &EscurelProcess, token: &str, args: &[&str]) -> (bool, Value) {
    let server = p.base_url().to_owned();
    let token = token.to_owned();
    let args: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
    tokio::task::spawn_blocking(move || {
        let out = Command::cargo_bin("escurel")
            .unwrap()
            .env("ESCUREL_SERVER", server)
            .env("ESCUREL_TOKEN", token)
            .args(&args)
            .output()
            .unwrap();
        let text = if out.status.success() {
            out.stdout
        } else {
            out.stderr
        };
        (
            out.status.success(),
            serde_json::from_slice(&text).unwrap_or(Value::Null),
        )
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn an_admin_issues_and_revokes_an_external_agents_token() {
    let p = gateway().await;
    let admin = p.mint_token_with_sub(TENANT, Role::Admin, "ops:jo");

    let (ok, issued) = cli(
        &p,
        &admin,
        &["agent", "token", "issue", "scribe", "--ttl", "2d"],
    )
    .await;
    assert!(ok, "{issued}");
    assert_eq!(issued["subject"], "agent:scribe", "{issued}");
    let token = issued["token"].as_str().unwrap().to_owned();
    let (ok, me) = cli(&p, &token, &["whoami"]).await;
    assert!(ok, "{me}");
    assert_eq!(me["subject"], "agent:scribe");

    let (ok, revoked) = cli(&p, &admin, &["agent", "token", "revoke", "scribe"]).await;
    assert!(ok, "{revoked}");
    let epoch = revoked["token_epoch"].as_u64().unwrap();
    assert_eq!(revoked["agent"], "scribe", "{revoked}");

    let (ok, err) = cli(&p, &token, &["whoami"]).await;
    assert!(!ok, "a revoked token is refused: {err}");
    assert!(err["error"].as_str().unwrap().contains("revoked"), "{err}");

    // The page kept its keys and body, and carries the epoch.
    let (ok, page) = cli(
        &p,
        &admin,
        &["page", "expand", "markdown/instances/agent/scribe.md"],
    )
    .await;
    assert!(ok, "{page}");
    assert_eq!(page["frontmatter"]["token_epoch"], json!(epoch), "{page}");
    assert_eq!(
        page["frontmatter"]["scopes"]["write"],
        json!(["markdown/instances/note/*"]),
        "{page}"
    );
    assert!(
        page["body"].as_str().unwrap().contains("Takes notes."),
        "{page}"
    );

    // The epoch is the gateway's, never the CLI's clock: past an epoch already ahead of every
    // clock, the next revoke moves it one second further rather than back to "now + 1".
    let ahead = epoch + 600;
    let content = SCRIBE.replacen(
        "---\n# scribe",
        &format!("token_epoch: {ahead}\n---\n# scribe"),
        1,
    );
    let wrote: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {admin}"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
            "name": "update_page",
            "arguments": { "page_id": "markdown/instances/agent/scribe.md", "content": content } } }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(wrote["result"]["structuredContent"]["ok"], true, "{wrote}");
    let (ok, again) = cli(&p, &admin, &["agent", "token", "revoke", "scribe"]).await;
    assert!(ok, "{again}");
    assert_eq!(again["token_epoch"], json!(ahead + 1), "{again}");
    let (ok, page) = cli(
        &p,
        &admin,
        &["page", "expand", "markdown/instances/agent/scribe.md"],
    )
    .await;
    assert!(ok, "{page}");
    assert_eq!(
        page["frontmatter"]["token_epoch"],
        json!(ahead + 1),
        "{page}"
    );
    assert_eq!(
        page["frontmatter"]["skills"],
        json!(["[[skill::note]]"]),
        "{page}"
    );

    // A member can do neither.
    let member = p.mint_token_with_sub(TENANT, Role::Agent, "member:alice");
    let (ok, err) = cli(&p, &member, &["agent", "token", "issue", "scribe"]).await;
    assert!(!ok, "{err}");
    let (ok, err) = cli(&p, &member, &["agent", "token", "revoke", "scribe"]).await;
    assert!(!ok, "{err}");
}
