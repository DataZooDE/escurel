//! Replies in a lineage, under the enforced event ACL (`docs/design/service-role-gate.md`):
//!
//! * the subject that captured a root event reads every reply in that root's lineage, even an
//!   unassigned one somebody else captured — a requester reads the answer to an objectless request;
//! * a role-restricted caller may reply under another label than the root's when that label's skill
//!   grants it `acl.reply_into`, and only into a root it can read.
//!
//! Real gateway (TestIssuer), real DuckDB, write and event ACL enforced, raw JSON-RPC.

use escurel_test_support::{
    AuthMode, ConfigOverrides, EscurelProcess, EventAclMode, FixtureBuilder, Opts, WriteAclMode,
};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const ROLE: &str = "app:worker";
const WORKER: &str = "agent:worker";
const TOOLS: &str = "app:worker=list_events,capture_event,assign_event";

const REQUEST_SKILL: &str = "---\nkind: skill\nid: request\n\
    acl:\n  read: [public]\n  read_events: [app:worker]\n---\n# request\n";
const ANSWER_SKILL: &str = "---\nkind: skill\nid: answer\n\
    acl:\n  read: [public]\n  reply_into: [app:worker]\n---\n# answer\n";
const NOTE_SKILL: &str = "---\nkind: skill\nid: note\nacl:\n  read: [public]\n---\n# note\n";

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            write_acl: Some(WriteAclMode::Enforce),
            event_acl: Some(EventAclMode::Enforce),
            role_tools: Some(TOOLS.to_owned()),
            ..Default::default()
        },
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("request", REQUEST_SKILL)
                .skill("answer", ANSWER_SKILL)
                .skill("note", NOTE_SKILL)
                .done(),
        ),
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

async fn ok(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    let body = rpc(p, token, name, args).await;
    assert!(body.get("error").is_none(), "{name}: {body}");
    body["result"]["structuredContent"].clone()
}

fn event(id: &str, label: &str, root: Option<&str>) -> Value {
    let mut e = json!({
        "event_id": id, "source": "app-ui", "mime": "text/plain",
        "label_skill": label, "title": id, "body": id,
    });
    if let Some(root) = root {
        e["provenance"] = json!({ "runner": { "root_event_id": root, "parent_event_id": root } });
    }
    e
}

fn ids(listing: &Value) -> Vec<String> {
    listing["events"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .filter_map(|e| e["event_id"].as_str().map(str::to_owned))
        .collect()
}

struct Tokens {
    worker: String,
    alice: String,
    bob: String,
}

fn tokens(p: &EscurelProcess) -> Tokens {
    Tokens {
        worker: p.mint_token_with_groups(TENANT, WORKER, &[ROLE], false),
        alice: p.mint_token_with_groups(TENANT, "alice", &["editors"], false),
        bob: p.mint_token_with_groups(TENANT, "bob", &["editors"], false),
    }
}

#[tokio::test]
async fn the_requester_reads_the_unassigned_reply_to_its_objectless_request() {
    let p = start().await;
    let t = tokens(&p);
    ok(&p, &t.alice, "capture_event", event("r-1", "request", None)).await;
    ok(
        &p,
        &t.worker,
        "capture_event",
        event("r-1:reply", "request", Some("r-1")),
    )
    .await;

    let alice = ids(&ok(
        &p,
        &t.alice,
        "list_events",
        json!({ "root_event_id": "r-1" }),
    )
    .await);
    assert!(
        alice.contains(&"r-1:reply".to_owned()),
        "the requester reads the reply: {alice:?}"
    );
    let by_id = ok(
        &p,
        &t.alice,
        "list_events",
        json!({ "event_id": "r-1:reply" }),
    )
    .await;
    assert_eq!(ids(&by_id), ["r-1:reply"], "{by_id}");

    // Nobody else gains anything: another member sees neither the request nor the reply.
    let bob = ids(&ok(&p, &t.bob, "list_events", json!({ "root_event_id": "r-1" })).await);
    assert!(bob.is_empty(), "{bob:?}");

    // A lineage the requester did not root stays closed to it: Bob's request and its reply.
    ok(&p, &t.bob, "capture_event", event("r-2", "request", None)).await;
    ok(
        &p,
        &t.worker,
        "capture_event",
        event("r-2:reply", "request", Some("r-2")),
    )
    .await;
    let alice2 = ids(&ok(
        &p,
        &t.alice,
        "list_events",
        json!({ "root_event_id": "r-2" }),
    )
    .await);
    assert!(alice2.is_empty(), "{alice2:?}");
}

#[tokio::test]
async fn a_restricted_reply_may_change_label_only_into_a_declared_label_and_a_readable_root() {
    let p = start().await;
    let t = tokens(&p);
    ok(&p, &t.alice, "capture_event", event("r-1", "request", None)).await;
    ok(&p, &t.alice, "capture_event", event("n-1", "note", None)).await;

    // Declared label (`answer` grants `reply_into`), readable root: accepted, and the requester
    // reads it through the lineage.
    ok(
        &p,
        &t.worker,
        "capture_event",
        event("r-1:answer", "answer", Some("r-1")),
    )
    .await;
    let alice = ids(&ok(
        &p,
        &t.alice,
        "list_events",
        json!({ "root_event_id": "r-1" }),
    )
    .await);
    assert!(alice.contains(&"r-1:answer".to_owned()), "{alice:?}");

    // Undeclared label: refused.
    let undeclared = rpc(
        &p,
        &t.worker,
        "capture_event",
        event("r-1:note", "note", Some("r-1")),
    )
    .await;
    assert_eq!(
        undeclared["error"]["data"]["code"], "forbidden",
        "{undeclared}"
    );
    // Declared label, but a root the worker cannot read: refused.
    let hidden = rpc(
        &p,
        &t.worker,
        "capture_event",
        event("n-1:answer", "answer", Some("n-1")),
    )
    .await;
    assert_eq!(hidden["error"]["data"]["code"], "forbidden", "{hidden}");

    // Unchanged for a token without the role: any label, any root.
    ok(
        &p,
        &t.bob,
        "capture_event",
        event("b-1", "note", Some("r-1")),
    )
    .await;
}
