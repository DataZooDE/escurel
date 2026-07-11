//! DoD test for issue #150: the context packager turns a `Trigger` into a
//! `TaskContext` ("skill body = instructions, `/mcp` = tools"), end to end
//! with **no mocks** — a real `escurel` gateway process, a real
//! `escurel-client::Client`, real `resolve`/`expand`/`list_events` over
//! `/mcp`, real `FixtureBuilder` + `capture_event` data.
//!
//! Flow:
//! 1. Spawn a real gateway (`EscurelProcess`, TestIssuer auth) seeded via
//!    `FixtureBuilder` with a recognizable **skill** page body and an
//!    **instance** page body.
//! 2. `capture_event` a real event labelled with that skill against the
//!    gateway over `/mcp`, then `assign_event` it to the seeded instance.
//! 3. Build a real `escurel-client::Client` with a minted agent bearer and
//!    a `RunnerConfig` pointed at the gateway.
//! 4. Build the `Trigger` (label_skill = seeded skill, instance_page_id =
//!    seeded instance, the real event_id/title/body) and call
//!    `package(&trigger, &client, &cfg)`.
//! 5. Assert the packaged `TaskContext`:
//!    - `instructions` contains the seeded **skill body text** (fetched via
//!      real resolve→expand), the task framing, and the event title/body;
//!    - `input` contains the seeded **instance content** (real expand);
//!    - `mcp_endpoint` ends with `/mcp`;
//!    - `allowed_tools` includes `update_page` + `assign_event`.

use escurel_client::{Client, SecretString};
use escurel_runner_core::{Lineage, RunnerConfig, Trigger, package};
use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const SKILL: &str = "customer";
const SKILL_BODY: &str = "---\ntype: skill\nid: customer\n---\n# customer\n\nUNIQUE_SKILL_MARKER fold the event into a customer instance.\n";
const INSTANCE_ID: &str = "globex";
const INSTANCE_BODY: &str = "---\ntype: instance\nid: globex\nskill: customer\n---\n# Globex\n\nUNIQUE_INSTANCE_MARKER current account state.\n";

/// Call an MCP tool over `/mcp` with a freshly minted bearer; return the
/// JSON-RPC `result`.
async fn call_mcp(p: &EscurelProcess, role: Role, name: &str, args: Value) -> Value {
    let token = p.mint_token(TENANT, role);
    let resp = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": name, "arguments": args },
        }))
        .send()
        .await
        .expect("post /mcp");
    assert_eq!(resp.status(), 200, "http status");
    let body: Value = resp.json().await.unwrap();
    assert!(body.get("error").is_none(), "tool {name} error: {body}");
    let result = body["result"].clone();
    result.get("structuredContent").cloned().unwrap_or(result)
}

#[tokio::test]
async fn packages_skill_body_as_instructions_with_event_and_instance() {
    // 1. Real gateway with a recognizable skill + instance fixture.
    let gateway = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill(SKILL, SKILL_BODY)
                .instance(SKILL, INSTANCE_ID, INSTANCE_BODY)
                .done(),
        ),
        ..Default::default()
    })
    .await;

    let instance_page_id = format!("markdown/instances/{SKILL}/{INSTANCE_ID}.md");

    // 2. Capture a real event labelled with the skill, then assign it to the
    //    seeded instance so its history is non-empty.
    let captured = call_mcp(
        &gateway,
        Role::Agent,
        "capture_event",
        json!({
            "source": "manual",
            "mime": "text/plain",
            "label_skill": SKILL,
            "title": "EVENT_TITLE_MARKER renewal",
            "body": "EVENT_BODY_MARKER customer wants to renew"
        }),
    )
    .await;
    let event_id = captured["event_id"]
        .as_str()
        .expect("capture_event returns an event_id")
        .to_owned();

    call_mcp(
        &gateway,
        Role::Agent,
        "assign_event",
        json!({ "event_id": event_id, "instance_page_id": instance_page_id }),
    )
    .await;

    // 3. Real client + config pointed at the gateway.
    let token = gateway.mint_token(TENANT, Role::Agent);
    let client = Client::connect(gateway.base_url(), SecretString::from(token.clone()))
        .await
        .expect("connect client");

    let cfg = RunnerConfig::from_env_with(|key| match key {
        "ESCUREL_RUNNER_GATEWAY_URL" => Some(gateway.base_url().to_owned()),
        "ESCUREL_RUNNER_TENANT" => Some(TENANT.to_owned()),
        "ESCUREL_RUNNER_TOKEN" => Some(token.clone()),
        _ => None,
    })
    .expect("config");

    // 4. Build the trigger and package it.
    let trigger = Trigger {
        tenant: TENANT.to_owned(),
        event_id: event_id.clone(),
        label_skill: SKILL.to_owned(),
        instance_page_id: Some(instance_page_id.clone()),
        lineage: Lineage::root(event_id.clone()),
        workflow: None,
    };

    let ctx = package(&trigger, &client, &cfg)
        .await
        .expect("package the trigger");

    // 5a. Instructions carry the real skill body + task framing + event.
    assert!(
        ctx.instructions.contains("UNIQUE_SKILL_MARKER"),
        "instructions must contain the resolved+expanded skill body: {}",
        ctx.instructions
    );
    assert!(
        ctx.instructions.contains(SKILL),
        "instructions must name the skill in the task framing: {}",
        ctx.instructions
    );
    assert!(
        ctx.instructions.contains("EVENT_TITLE_MARKER")
            && ctx.instructions.contains("EVENT_BODY_MARKER"),
        "instructions must include the event title + body: {}",
        ctx.instructions
    );

    // 5b. Input carries the real instance content.
    assert!(
        ctx.input.contains("UNIQUE_INSTANCE_MARKER"),
        "input must contain the expanded instance state: {}",
        ctx.input
    );

    // 5c. Toolset pointer: /mcp endpoint + the write-capable tools.
    assert!(
        ctx.mcp_endpoint.ends_with("/mcp"),
        "mcp_endpoint must end with /mcp: {}",
        ctx.mcp_endpoint
    );
    assert!(
        ctx.allowed_tools.iter().any(|t| t == "update_page"),
        "allowed_tools must include update_page: {:?}",
        ctx.allowed_tools
    );
    assert!(
        ctx.allowed_tools.iter().any(|t| t == "assign_event"),
        "allowed_tools must include assign_event: {:?}",
        ctx.allowed_tools
    );

    // The minted scoped token must be usable: a fresh client built from it
    // can read the gateway over the same /mcp.
    let scoped = Client::connect(
        gateway.base_url(),
        SecretString::from(ctx.token_str().to_owned()),
    )
    .await
    .expect("connect with the packaged scoped token");
    let resolved = scoped
        .resolve(escurel_client::ResolveRequest {
            wikilink: format!("[[{SKILL}]]"),
            ..Default::default()
        })
        .await
        .expect("resolve with scoped token");
    assert!(
        resolved.exists,
        "packaged token must be a usable agent token"
    );
}
