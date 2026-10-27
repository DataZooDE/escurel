//! The service-role gate (`docs/design/service-role-gate.md`) over the real HTTP surface.
//!
//! A worker runs under a service identity: `sub: agent:worker`, group `app:worker`, which the
//! gateway config (`ESCUREL_AUTH_ROLE_TOOLS`) restricts to a short tool list. The skills declare
//! what the role may read and propose:
//!
//! * `request` (the event label) — `acl.read_events: [app:worker]`: every request event in the
//!   tenant, objectless ones included;
//! * `attachment` — `acl.read_referenced: [app:worker]`, `read: [owner]`: only attachments a
//!   visible request references, and only ones its capturer could read;
//! * `capability` — `acl.propose: [app:worker]`: drafts without write rights; `secret` declares no
//!   grant.
//!
//! Real gateway (TestIssuer), real DuckDB, write and event ACL enforced, raw JSON-RPC.

use escurel_test_support::{
    AuthMode, ConfigOverrides, EscurelProcess, EventAclMode, FixtureBuilder, Opts, WriteAclMode,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const TENANT: &str = "acme";
const ROLE: &str = "app:worker";
const WORKER: &str = "agent:worker";
const TOOLS: &str = "app:worker=list_inbox,list_events,expand,list_instances,list_drafts,\
                     create_draft,capture_event,assign_event,mint_agent_token";

const REQUEST_SKILL: &str = "---\nkind: skill\nid: request\n\
    acl:\n  read: [public]\n  create: [editors]\n  update: [editors]\n  read_events: [app:worker]\n---\n# request\n";
const OTHER_SKILL: &str = "---\nkind: skill\nid: other\n\
    acl:\n  read: [public]\n  create: [editors]\n  update: [editors]\n---\n# other\n";
const ATTACHMENT_SKILL: &str = "---\nkind: skill\nid: attachment\nowner_field: author\n\
    acl:\n  read: [owner]\n  create: [owner]\n  read_referenced: [app:worker]\n---\n# attachment\n";
const CAPABILITY_SKILL: &str = "---\nkind: skill\nid: capability\n\
    acl:\n  read: [public]\n  create: [editors]\n  update: [editors]\n  propose: [app:worker]\n---\n# capability\n";
const SECRET_SKILL: &str = "---\nkind: skill\nid: secret\n\
    acl:\n  read: [public]\n  create: [editors]\n  update: [editors]\n---\n# secret\n";

const CAP1: &str = "---\nkind: instance\nid: cap1\nskill: capability\n---\n# Cap1\n\nBASE.\n";
const S1: &str = "---\nkind: instance\nid: s1\nskill: secret\n---\n# S1\n\nBASE.\n";
const A1: &str =
    "---\nkind: instance\nid: a1\nskill: attachment\nauthor: alice\n---\n# a1\n\nALICE-FILE-1\n";
const A2: &str =
    "---\nkind: instance\nid: a2\nskill: attachment\nauthor: alice\n---\n# a2\n\nALICE-FILE-2\n";
const B1: &str =
    "---\nkind: instance\nid: b1\nskill: attachment\nauthor: bob\n---\n# b1\n\nBOB-FILE-1\n";

const CAP1_PAGE: &str = "markdown/instances/capability/cap1.md";
const S1_PAGE: &str = "markdown/instances/secret/s1.md";

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            write_acl: Some(WriteAclMode::Enforce),
            event_acl: Some(EventAclMode::Enforce),
            role_tools: Some(TOOLS.to_owned()),
            signing: true,
            ..Default::default()
        },
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("request", REQUEST_SKILL)
                .skill("other", OTHER_SKILL)
                .skill("attachment", ATTACHMENT_SKILL)
                .skill("capability", CAPABILITY_SKILL)
                .skill("secret", SECRET_SKILL)
                .instance("capability", "cap1", CAP1)
                .instance("secret", "s1", S1)
                .instance("attachment", "a1", A1)
                .instance("attachment", "a2", A2)
                .instance("attachment", "b1", B1)
                .done(),
        ),
    })
    .await
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

/// The whole JSON-RPC response body.
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

fn assert_role_refused(body: &Value, tool: &str) {
    assert_eq!(
        body["error"]["data"]["code"], "role_restricted",
        "{tool} must be refused by the role gate: {body}"
    );
}

fn sha(s: &str) -> String {
    format!("{:x}", Sha256::digest(s.as_bytes()))
}

fn ids(events: &Value) -> Vec<String> {
    events["events"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .filter_map(|e| e["event_id"].as_str().map(str::to_owned))
        .collect()
}

fn event(id: &str, label: &str, body: &str, page: Option<&str>) -> Value {
    let mut e = json!({
        "event_id": id, "source": "app-ui", "mime": "text/plain",
        "label_skill": label, "title": id, "body": body,
    });
    if let Some(page) = page {
        e["instance_page_id"] = json!(page);
    }
    e
}

/// Alice files an object request (on cap1) and an objectless one with two attachment links; Bob
/// files an objectless request; Alice files an objectless event under another label.
async fn seed_events(p: &EscurelProcess, t: &Tokens) {
    ok(
        p,
        &t.alice,
        "capture_event",
        event("r-obj", "request", "about cap1", Some(CAP1_PAGE)),
    )
    .await;
    let body = "Attachment: [[attachment::a1]]\nAttachment: [[attachment::b1]]\n\"please look\"";
    ok(
        p,
        &t.alice,
        "capture_event",
        event("r-free", "request", body, None),
    )
    .await;
    ok(
        p,
        &t.bob,
        "capture_event",
        event("r-bob", "request", "bob asks", None),
    )
    .await;
    ok(
        p,
        &t.alice,
        "capture_event",
        event("o-1", "other", "alice private", None),
    )
    .await;
}

#[tokio::test]
async fn the_worker_reads_every_declared_request_event_and_no_other_private_event() {
    let p = start().await;
    let t = tokens(&p);
    seed_events(&p, &t).await;

    let requests = ok(
        &p,
        &t.worker,
        "list_events",
        json!({ "label_skill": "request" }),
    )
    .await;
    let mut seen = ids(&requests);
    seen.sort();
    assert_eq!(
        seen,
        ["r-bob", "r-free", "r-obj"],
        "objectless included: {requests}"
    );

    let inbox = ids(&ok(&p, &t.worker, "list_inbox", json!({})).await);
    assert!(inbox.contains(&"r-free".to_owned()), "{inbox:?}");
    assert!(
        !inbox.contains(&"o-1".to_owned()),
        "another label stays private: {inbox:?}"
    );
    let other = ok(
        &p,
        &t.worker,
        "list_events",
        json!({ "label_skill": "other" }),
    )
    .await;
    assert!(ids(&other).is_empty(), "{other}");

    // Unchanged for a token without the role: Bob still cannot see Alice's objectless request.
    let bob_view = ids(&ok(
        &p,
        &t.bob,
        "list_events",
        json!({ "label_skill": "request" }),
    )
    .await);
    assert!(!bob_view.contains(&"r-free".to_owned()), "{bob_view:?}");
    assert!(bob_view.contains(&"r-bob".to_owned()), "{bob_view:?}");
}

#[tokio::test]
async fn the_worker_reads_only_attachments_a_visible_request_references() {
    let p = start().await;
    let t = tokens(&p);
    seed_events(&p, &t).await;

    let a1 = ok(
        &p,
        &t.worker,
        "expand",
        json!({ "page_id": "markdown/instances/attachment/a1.md" }),
    )
    .await;
    assert!(a1["page"].is_object(), "referenced by r-free: {a1}");
    let a2 = ok(
        &p,
        &t.worker,
        "expand",
        json!({ "page_id": "markdown/instances/attachment/a2.md" }),
    )
    .await;
    assert!(a2["page"].is_null(), "not referenced: {a2}");
    // r-free names Bob's file, but its capturer (Alice) may not read it: no confused deputy.
    let b1 = ok(
        &p,
        &t.worker,
        "expand",
        json!({ "page_id": "markdown/instances/attachment/b1.md" }),
    )
    .await;
    assert!(b1["page"].is_null(), "requester cannot read it: {b1}");
    let listed = ok(
        &p,
        &t.worker,
        "list_instances",
        json!({ "skill": "attachment" }),
    )
    .await;
    let listed = listed.to_string();
    assert!(
        listed.contains("a1") && !listed.contains("\"a2\"") && !listed.contains("b1"),
        "{listed}"
    );

    // Unchanged for a token without the role.
    let bob = ok(
        &p,
        &t.bob,
        "expand",
        json!({ "page_id": "markdown/instances/attachment/a1.md" }),
    )
    .await;
    assert!(bob["page"].is_null(), "{bob}");
    let alice = ok(
        &p,
        &t.alice,
        "expand",
        json!({ "page_id": "markdown/instances/attachment/a2.md" }),
    )
    .await;
    assert!(alice["page"].is_object(), "{alice}");
}

#[tokio::test]
async fn the_worker_proposes_only_for_declared_skills_and_never_decides() {
    let p = start().await;
    let t = tokens(&p);
    seed_events(&p, &t).await;
    let proposed = CAP1.replace("BASE.", "PROPOSED.");
    let created = ok(
        &p,
        &t.worker,
        "create_draft",
        json!({ "target_page_id": CAP1_PAGE, "content": proposed, "base_sha256": sha(CAP1) }),
    )
    .await;
    let draft_id = created["draft"]["draft_id"]
        .as_str()
        .unwrap_or_else(|| panic!("{created}"))
        .to_owned();

    let refused = ok(
        &p,
        &t.worker,
        "create_draft",
        json!({ "target_page_id": S1_PAGE, "content": S1.replace("BASE.", "X."), "base_sha256": sha(S1) }),
    )
    .await;
    assert_eq!(
        refused["ok"],
        json!(false),
        "no propose grant on `secret`: {refused}"
    );

    for (tool, args) in [
        ("promote_draft", json!({ "draft_id": draft_id })),
        ("discard_draft", json!({ "draft_id": draft_id })),
        ("promote_changeset", json!({ "changeset_id": "cs-1" })),
        ("discard_changeset", json!({ "changeset_id": "cs-1" })),
        (
            "update_page",
            json!({ "page_id": CAP1_PAGE, "content": proposed }),
        ),
        (
            "write_instance",
            json!({ "page_id": CAP1_PAGE, "patch": {} }),
        ),
        (
            "ingest_upload",
            json!({ "skill": "attachment", "content_type": "text/plain", "bytes_b64": "eA==" }),
        ),
        ("search", json!({ "q": "cap" })),
        ("admin_audit", json!({})),
        ("tenant_list", json!({})),
    ] {
        assert_role_refused(&rpc(&p, &t.worker, tool, args).await, tool);
    }
    let ingest = reqwest::Client::new()
        .post(format!("{}/ingest", p.base_url()))
        .header("authorization", format!("Bearer {}", t.worker))
        .json(&json!({ "blob_id": "x", "content_type": "text/plain" }))
        .send()
        .await
        .expect("post");
    assert_eq!(ingest.status(), 403, "the REST intake door is closed too");

    // The draft is still there for a human: Alice (an editor, no role) promotes it.
    let promoted = ok(
        &p,
        &t.alice,
        "promote_draft",
        json!({ "draft_id": draft_id }),
    )
    .await;
    assert_ne!(promoted["ok"], json!(false), "{promoted}");
}

#[tokio::test]
async fn the_worker_captures_replies_and_assigns_visible_requests() {
    let p = start().await;
    let t = tokens(&p);
    seed_events(&p, &t).await;

    let mut reply = event("r-free:reply", "request", "answered", None);
    reply["provenance"] =
        json!({ "runner": { "root_event_id": "r-free", "parent_event_id": "r-free" } });
    let stored = ok(&p, &t.worker, "capture_event", reply).await;
    assert_eq!(stored["provenance"]["captured_by"], WORKER, "{stored}");

    // New work under another label, and a reply to an event it cannot read, are refused.
    let fresh = rpc(
        &p,
        &t.worker,
        "capture_event",
        event("w-1", "other", "new work", None),
    )
    .await;
    assert!(fresh.get("error").is_some(), "{fresh}");
    let mut hidden = event("o-1:reply", "other", "x", None);
    hidden["provenance"] = json!({ "runner": { "root_event_id": "o-1" } });
    assert!(
        rpc(&p, &t.worker, "capture_event", hidden)
            .await
            .get("error")
            .is_some()
    );

    // An object request: filed already, the worker marks it processed.
    let a = ok(
        &p,
        &t.worker,
        "assign_event",
        json!({ "event_id": "r-obj", "instance_page_id": CAP1_PAGE }),
    )
    .await;
    assert_eq!(a["status"], "processed", "{a}");
    // An objectless request: filed into a page the worker can read, without write on it.
    let b = ok(
        &p,
        &t.worker,
        "assign_event",
        json!({ "event_id": "r-bob", "instance_page_id": CAP1_PAGE }),
    )
    .await;
    assert_eq!(b["status"], "processed", "{b}");
    // Another label's private event stays invisible.
    let c = rpc(
        &p,
        &t.worker,
        "assign_event",
        json!({ "event_id": "o-1", "instance_page_id": CAP1_PAGE }),
    )
    .await;
    assert_eq!(c["error"]["data"]["code"], "event_not_found", "{c}");

    // Unchanged for a token without the role: a member without write on the target cannot file
    // its own objectless event there.
    let carol = p.mint_token_with_groups(TENANT, "carol", &[], false);
    ok(
        &p,
        &carol,
        "capture_event",
        event("c-1", "request", "carol", None),
    )
    .await;
    let d = rpc(
        &p,
        &carol,
        "assign_event",
        json!({ "event_id": "c-1", "instance_page_id": CAP1_PAGE }),
    )
    .await;
    assert_eq!(d["error"]["data"]["code"], "event_not_found", "{d}");
}

#[tokio::test]
async fn a_minted_child_token_inherits_the_restriction() {
    let p = start().await;
    let t = tokens(&p);
    seed_events(&p, &t).await;
    let minted = ok(
        &p,
        &t.worker,
        "mint_agent_token",
        json!({ "skill": "request", "root_event_id": "r-free", "ttl_secs": 300 }),
    )
    .await;
    let child = minted["token"].as_str().expect("token").to_owned();

    let requests = ids(&ok(
        &p,
        &child,
        "list_events",
        json!({ "label_skill": "request" }),
    )
    .await);
    assert!(requests.contains(&"r-free".to_owned()), "{requests:?}");
    let proposed = CAP1.replace("BASE.", "CHILD.");
    let created = ok(
        &p,
        &child,
        "create_draft",
        json!({ "target_page_id": CAP1_PAGE, "content": proposed, "base_sha256": sha(CAP1) }),
    )
    .await;
    let draft_id = created["draft"]["draft_id"]
        .as_str()
        .unwrap_or_else(|| panic!("{created}"))
        .to_owned();
    let refused = ok(
        &p,
        &child,
        "create_draft",
        json!({ "target_page_id": S1_PAGE, "content": S1.replace("BASE.", "X."), "base_sha256": sha(S1) }),
    )
    .await;
    assert_eq!(refused["ok"], json!(false), "{refused}");
    assert_role_refused(
        &rpc(&p, &child, "promote_draft", json!({ "draft_id": draft_id })).await,
        "promote_draft",
    );
    assert_role_refused(
        &rpc(
            &p,
            &child,
            "update_page",
            json!({ "page_id": CAP1_PAGE, "content": proposed }),
        )
        .await,
        "update_page",
    );
}

#[tokio::test]
async fn tools_list_shows_a_restricted_token_only_its_tools() {
    let p = start().await;
    let t = tokens(&p);
    let list = |token: String| {
        let url = p.mcp_url();
        async move {
            let body: Value = reqwest::Client::new()
                .post(url)
                .header("authorization", format!("Bearer {token}"))
                .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
                .send()
                .await
                .expect("post")
                .json()
                .await
                .expect("json");
            let mut names: Vec<String> = body["result"]["tools"]
                .as_array()
                .expect("tools")
                .iter()
                .filter_map(|t| t["name"].as_str().map(str::to_owned))
                .collect();
            names.sort();
            names
        }
    };
    let worker = list(t.worker.clone()).await;
    assert_eq!(
        worker,
        [
            "assign_event",
            "capture_event",
            "create_draft",
            "expand",
            "list_drafts",
            "list_events",
            "list_inbox",
            "list_instances",
            "mint_agent_token"
        ]
    );
    let alice = list(t.alice.clone()).await;
    assert!(alice.contains(&"promote_draft".to_owned()) && alice.contains(&"search".to_owned()));
}

/// The minter writes a child's groups to `roles`. Where the gateway reads groups from another
/// claim, a restricted caller's child would arrive without its role, i.e. unrestricted: refused.
#[tokio::test]
async fn a_restricted_role_cannot_mint_when_groups_come_from_another_claim() {
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            event_acl: Some(EventAclMode::Enforce),
            role_tools: Some(TOOLS.to_owned()),
            groups_claim: Some("member_groups".to_owned()),
            signing: true,
            ..Default::default()
        },
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("request", REQUEST_SKILL)
                .done(),
        ),
    })
    .await;
    let worker = p.mint_token_with_groups(TENANT, WORKER, &[ROLE], false);
    let refused = rpc(
        &p,
        &worker,
        "mint_agent_token",
        json!({ "skill": "request" }),
    )
    .await;
    assert_eq!(refused["error"]["data"]["code"], "unsupported", "{refused}");
    // A token without the role mints as before.
    let alice = p.mint_token_with_groups(TENANT, "alice", &["editors"], false);
    ok(
        &p,
        &alice,
        "mint_agent_token",
        json!({ "skill": "request" }),
    )
    .await;
}
