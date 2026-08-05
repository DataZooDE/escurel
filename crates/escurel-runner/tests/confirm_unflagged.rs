//! Reconciling a run whose trigger had **no pre-flagged instance** — the
//! case every real LLM harness produces, with **no mocks**.
//!
//! ## The gap this pins
//!
//! `HarnessOutcome.produced_instance` is hardcoded `None` in both real
//! adapters (`claude.rs`, `codex.rs`) — their output envelopes do not name
//! the page the model wrote, and the design says the runner reads that back
//! from the gateway instead. Only the deterministic `echo` stub reports a
//! produced instance, which is why every other test passes over this.
//!
//! So for `claude` or `codex` on an event that was NOT captured with an
//! `instance_page_id`, the runner has neither a harness-reported page nor a
//! pre-flagged one. That is the ordinary shape of "fold this into whichever
//! instance is right" — the judgement an LLM is there to make.
//!
//! What the agent *does* leave behind is `assign_event`, which binds the
//! event to the instance it chose. That binding is the gateway's own record
//! of the effect, and it is what reconcile has to be able to read.
//!
//! These tests drive a real `EscurelProcess`: capture an event with no
//! target, assign it the way an agent would, and assert the reconciler can
//! resolve and confirm the produced instance from the event alone.

use escurel_client::{Client, SecretString};
use escurel_runner_core::{Lineage, Trigger, confirm_effect};
use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const SKILL: &str = "customer";
const SKILL_BODY: &str =
    "---\ntype: skill\nid: customer\n---\n# customer\n\nFold the event into a customer instance.\n";
const INSTANCE_ID: &str = "globex";
const INSTANCE_BODY: &str =
    "---\ntype: instance\nid: globex\nskill: customer\n---\n# Globex\n\nAccount state.\n";

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

/// An UNFLAGGED trigger whose event the agent assigned must reconcile to the
/// instance the agent chose.
///
/// Before the fix this returned
/// `Transient("event … bound but instance not resolvable from inbox
/// read-back")` — reconcile could see the event had left the inbox but had
/// no way to ask *where it went*, so a completed run could never be
/// confirmed and never cascaded.
#[tokio::test]
async fn unflagged_trigger_resolves_the_instance_the_agent_assigned() {
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

    // Captured with NO instance_page_id — the agent picks the target.
    let captured = call_mcp(
        &gateway,
        Role::Agent,
        "capture_event",
        json!({
            "source": "manual", "mime": "text/plain", "label_skill": SKILL,
            "title": "renewal", "body": "customer wants to renew",
        }),
    )
    .await;
    let event_id = captured["event_id"].as_str().unwrap().to_owned();

    // What the agent does at the end of a run.
    call_mcp(
        &gateway,
        Role::Agent,
        "assign_event",
        json!({ "event_id": event_id, "instance_page_id": instance_page_id }),
    )
    .await;

    let token = gateway.mint_token(TENANT, Role::Agent);
    let client = Client::connect(gateway.base_url(), SecretString::from(token))
        .await
        .expect("connect client");

    let trigger = Trigger {
        tenant: TENANT.into(),
        event_id: event_id.clone(),
        label_skill: SKILL.into(),
        instance_page_id: None, // <- the whole point
        lineage: Lineage::root(&event_id),
        workflow: None,
    };

    let effect = confirm_effect(&client, &trigger)
        .await
        .expect("an assigned event must reconcile from the event alone");

    assert_eq!(
        effect.instance_page_id, instance_page_id,
        "reconcile must resolve the instance the agent chose"
    );
    assert!(
        !effect.version.is_empty(),
        "a confirmed effect carries the instance's read-back version"
    );
}

/// The genuinely-nothing-to-do case must STILL be distinguishable: an event
/// the agent left unassigned is not a confirmed effect.
///
/// This is the guard that stops the fix from turning every run into a
/// success — "no effect" has to stay a real, reachable outcome, or a broken
/// run would be recorded as converged.
#[tokio::test]
async fn unassigned_event_is_not_a_confirmed_effect() {
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

    let captured = call_mcp(
        &gateway,
        Role::Agent,
        "capture_event",
        json!({
            "source": "manual", "mime": "text/plain", "label_skill": SKILL,
            "title": "untouched", "body": "nobody assigned this",
        }),
    )
    .await;
    let event_id = captured["event_id"].as_str().unwrap().to_owned();

    let token = gateway.mint_token(TENANT, Role::Agent);
    let client = Client::connect(gateway.base_url(), SecretString::from(token))
        .await
        .expect("connect client");

    let trigger = Trigger {
        tenant: TENANT.into(),
        event_id: event_id.clone(),
        label_skill: SKILL.into(),
        instance_page_id: None,
        lineage: Lineage::root(&event_id),
        workflow: None,
    };

    assert!(
        confirm_effect(&client, &trigger).await.is_err(),
        "an event still sitting in the inbox is not a confirmed effect"
    );
}
