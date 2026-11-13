//! A chat bridge's token at the gateway (#824, crew F1): the bridge signs for
//! the people it answers with a key of its OWN, under an issuer the gateway
//! names as the bridge's (`ConfigOverrides::bridge_issuer`, production
//! `ESCUREL_AUTH_BRIDGE_ISSUER`, pinned to `ESCUREL_AUTH_BRIDGE_KID`). Whatever the bridge signs under it, the gateway
//! bounds it: never admin, no groups, short-lived, no agent/run claims — and
//! what it captures says the bridge relayed it. Real gateway, real JWKS for
//! both issuers; no mocks.

use escurel_test_support::{
    AuthMode, ConfigOverrides, EscurelProcess, ExtraIssuer, FixtureBuilder, Opts, Role,
    WriteAclMode,
};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const BRIDGE_KID: &str = "escurel-chat-bridge";

const DEAL_SKILL: &str = "---\nkind: skill\nid: deal\n\
    description: A sales deal, readable by the sales group.\n\
    acl:\n  read: [sales]\n---\n# deal\n";
const DEAL: &str = "---\nkind: instance\nskill: deal\nid: q3\n---\n# Q3\nRenewal.\n";
const DEAL_PAGE: &str = "markdown/instances/deal/q3.md";

async fn gateway(bridge: &ExtraIssuer) -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("deal", DEAL_SKILL)
                .instance("deal", "q3", DEAL)
                .skill("chat", CHAT_SKILL)
                .done(),
        ),
        config_overrides: ConfigOverrides {
            extra_issuers: vec![(bridge.issuer_url().to_owned(), bridge.jwks_url().to_owned())],
            bridge_issuer: Some(bridge.issuer_url().to_owned()),
            bridge_kids: vec![BRIDGE_KID.to_owned()],
            ..Default::default()
        },
    })
    .await
}

/// `(http status, body)` of one tool call.
async fn call(p: &EscurelProcess, token: &str, name: &str, args: Value) -> (u16, Value) {
    let resp = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": { "name": name, "arguments": args } }))
        .send()
        .await
        .expect("post");
    let status = resp.status().as_u16();
    (status, resp.json().await.unwrap_or(Value::Null))
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// What a compromised bridge would sign: admin, operator, a group, and the
/// channel claim.
fn greedy(bridge: &ExtraIssuer, ttl: u64) -> Value {
    let now = now();
    json!({
        "iss": bridge.issuer_url(), "aud": "escurel", "sub": "user:alice", "tenant": TENANT,
        "roles": ["escurel:admin", "escurel:operator", "sales"],
        "bridge_channel": "msteams", "iat": now, "exp": now + ttl,
    })
}

#[tokio::test]
async fn a_bridge_token_acts_as_the_person_without_admin_or_groups_and_its_captures_say_so() {
    let bridge = ExtraIssuer::start_with_own_key(BRIDGE_KID).await;
    let p = gateway(&bridge).await;
    let token = bridge.sign(&greedy(&bridge, 900));

    let (_, who) = call(&p, &token, "whoami", json!({})).await;
    let who = &who["result"]["structuredContent"];
    assert_eq!(who["subject"], "user:alice", "{who}");
    assert_eq!(
        who["is_admin"], false,
        "the admin role is not honoured: {who}"
    );

    // `sales` would grant the read, but a bridge token reads nothing at all
    // (and carries no groups: crates/escurel-auth/tests/oidc.rs).
    let (_, page) = call(&p, &token, "expand", json!({ "page_id": DEAL_PAGE })).await;
    forbidden(&page, "expand");

    // The capture names the bridge as the relay, whatever the caller wrote.
    let (_, captured) = call(
        &p,
        &token,
        "capture_event",
        json!({ "event_id": "bridge-ev-1", "source": "msteams", "label_skill": "chat",
                "title": "chat turn",
                "provenance": { "captured_via": "escurel-runner", "captured_by": "user:bob" } }),
    )
    .await;
    assert!(captured.get("error").is_none(), "{captured}");
    let admin = p.mint_token(TENANT, Role::Admin);
    let (_, listed) = call(&p, &admin, "list_events", json!({ "label_skill": "chat" })).await;
    let events = listed["result"]["structuredContent"]["events"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let ev = events
        .iter()
        .find(|e| e["event_id"] == "bridge-ev-1")
        .unwrap_or_else(|| panic!("captured: {listed}"));
    assert_eq!(ev["provenance"]["captured_by"], "user:alice");
    assert_eq!(ev["provenance"]["captured_via"], "bridge:msteams");
}

#[tokio::test]
async fn a_long_lived_or_agent_shaped_bridge_token_is_refused() {
    let bridge = ExtraIssuer::start_with_own_key(BRIDGE_KID).await;
    let p = gateway(&bridge).await;
    let mut refused = vec![greedy(&bridge, 3600)];
    for (claim, value) in [
        ("agent", json!("deploy-bot")),
        ("run_id", json!("r-1")),
        ("act", json!({ "sub": "escurel-runner" })),
    ] {
        let mut c = greedy(&bridge, 900);
        c[claim] = value;
        refused.push(c);
    }
    let mut unnamed = greedy(&bridge, 900);
    unnamed.as_object_mut().unwrap().remove("bridge_channel");
    refused.push(unnamed);
    for claims in refused {
        let (status, body) = call(&p, &bridge.sign(&claims), "whoami", json!({})).await;
        assert_eq!(status, 401, "{claims} → {body}");
    }
}

// --- Confinement (crew security F2) -------------------------------------
//
// The bridge's key signs for anyone, so a bridge token must be a chat relay,
// not a stand-in for the person on the whole agent surface: holding the key
// must not let it rewrite a victim's records or read their private pages.

const VICTIM: &str = "user:victim";
const RENEWAL_SKILL: &str = "---\nkind: skill\nid: renewal\ndescription: A renewal.\n\
    autonomy: auto\nowner_field: owner\nacl:\n  read: [owner]\n  create: [owner]\n  update: [owner]\n---\n# renewal\n";
const RENEWAL: &str =
    "---\nkind: instance\nskill: renewal\nid: r1\nowner: \"user:victim\"\n---\n# R1\n";
const RENEWAL_PAGE: &str = "markdown/instances/renewal/r1.md";
const NOTE_SKILL: &str = "---\nkind: skill\nid: note\ndescription: A note.\nvisibility: owner\n\
    owner_field: owner\n---\n# note\n";
const NOTE: &str =
    "---\nkind: instance\nskill: note\nid: mine\nowner: \"user:victim\"\n---\n# Mine\n\nSECRET.\n";
const NOTE_PAGE: &str = "markdown/instances/note/mine.md";
const CHAT_SKILL: &str =
    "---\nkind: skill\nid: chat\ndescription: Answer questions.\n---\n# chat\n";
const CONVERSATION_SKILL: &str = "---\nkind: skill\nid: conversation\ndescription: A chat.\n\
    owner_field: owner\nacl:\n  read: [owner]\n  create: [owner]\n  update: [owner]\n\
    chat:\n  page_bound: true\n---\n# conversation\n";

async fn victim_gateway(bridge: &ExtraIssuer) -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("renewal", RENEWAL_SKILL)
                .instance("renewal", "r1", RENEWAL)
                .skill("note", NOTE_SKILL)
                .instance("note", "mine", NOTE)
                .skill("chat", CHAT_SKILL)
                .skill("conversation", CONVERSATION_SKILL)
                .done(),
        ),
        config_overrides: ConfigOverrides {
            write_acl: Some(WriteAclMode::Enforce),
            extra_issuers: vec![(bridge.issuer_url().to_owned(), bridge.jwks_url().to_owned())],
            bridge_issuer: Some(bridge.issuer_url().to_owned()),
            bridge_kids: vec![BRIDGE_KID.to_owned()],
            ..Default::default()
        },
    })
    .await
}

fn bridge_token_for(bridge: &ExtraIssuer, subject: &str, tenant: &str) -> String {
    let now = now();
    bridge.sign(&json!({
        "iss": bridge.issuer_url(), "aud": "escurel", "sub": subject, "tenant": tenant,
        "bridge_channel": "msteams", "iat": now, "exp": now + 600,
    }))
}

fn forbidden(body: &Value, what: &str) {
    assert_eq!(body["error"]["code"], -32003, "{what}: {body}");
    assert_eq!(body["error"]["data"]["code"], "forbidden", "{what}: {body}");
}

#[tokio::test]
async fn a_bridge_token_for_a_victim_cannot_rewrite_their_records_or_read_their_pages() {
    let bridge = ExtraIssuer::start_with_own_key(BRIDGE_KID).await;
    let p = victim_gateway(&bridge).await;
    let token = bridge_token_for(&bridge, VICTIM, TENANT);
    let rewrite = json!({ "page_id": RENEWAL_PAGE,
        "content": "---\nkind: instance\nskill: renewal\nid: r1\nowner: \"user:victim\"\n---\n# R1\n\nCancelled.\n" });

    // CONTROL: the victim's own token may do both, so what refuses the bridge
    // below is the bridge confinement, not the ACL.
    let own = p.mint_token_with_sub(TENANT, Role::Agent, VICTIM);
    let (_, page) = call(&p, &own, "expand", json!({ "page_id": NOTE_PAGE })).await;
    assert!(
        page["result"]["structuredContent"]["page"].is_object(),
        "the owner reads their note: {page}"
    );

    let (_, wrote) = call(&p, &token, "update_page", rewrite.clone()).await;
    forbidden(&wrote, "update_page on a non-chat instance");
    let (_, read) = call(&p, &token, "expand", json!({ "page_id": NOTE_PAGE })).await;
    forbidden(&read, "expand of a private page");
    let (_, created) = call(
        &p,
        &token,
        "update_page",
        json!({ "page_id": "markdown/instances/renewal/r2.md", "base_sha256": "",
                "content": "---\nkind: instance\nskill: renewal\nid: r2\nowner: \"user:victim\"\n---\n# R2\n" }),
    )
    .await;
    forbidden(
        &created,
        "creating an instance of a skill that is not page-bound chat",
    );
    let (_, labelled) = call(
        &p,
        &token,
        "capture_event",
        json!({ "event_id": "b-1", "source": "msteams", "label_skill": "renewal", "title": "x" }),
    )
    .await;
    forbidden(&labelled, "capture_event under a non-chat label");
    let (_, faked) = call(
        &p,
        &token,
        "append_message",
        json!({ "chat_group_id": "general", "role": "user", "content": "hi" }),
    )
    .await;
    forbidden(&faked, "append_message outside a page-bound chat");

    // The record is untouched.
    let admin = p.mint_token(TENANT, Role::Admin);
    let (_, after) = call(&p, &admin, "expand", json!({ "page_id": RENEWAL_PAGE })).await;
    let body = after["result"]["structuredContent"]["page"].to_string();
    assert!(!body.contains("Cancelled"), "{after}");

    // tools/list offers a bridge token only what it may call.
    let listed: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let mut names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "append_message",
            "capture_event",
            "ingest_upload",
            "list_messages",
            "update_page",
            "whoami"
        ],
        "{listed}"
    );
}

#[tokio::test]
async fn a_bridge_token_reaches_none_of_the_non_tool_surfaces() {
    let bridge = ExtraIssuer::start_with_own_key(BRIDGE_KID).await;
    let p = victim_gateway(&bridge).await;
    let token = bridge_token_for(&bridge, VICTIM, TENANT);
    let http = reqwest::Client::new();
    let ingest = http
        .post(format!("{}/ingest", p.base_url()))
        .bearer_auth(&token)
        .json(&json!({ "blob_id": "x", "content_type": "text/plain" }))
        .send()
        .await
        .unwrap();
    assert_eq!(ingest.status(), 403, "/ingest");
    let blob = http
        .get(format!("{}/blob/{NOTE_PAGE}", p.base_url()))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(blob.status(), 403, "/blob");
    let ws = http
        .get(format!("{}/ws", p.base_url()))
        .bearer_auth(&token)
        .header("connection", "upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-version", "13")
        .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
        .send()
        .await
        .unwrap();
    assert_eq!(ws.status(), 403, "/ws");
}

/// What the bridge does on a turn still works: create the conversation page
/// (create-if-absent), append the user turn, capture the `chat` event, read
/// the group back.
#[tokio::test]
async fn a_bridge_token_still_relays_a_chat_turn() {
    let bridge = ExtraIssuer::start_with_own_key(BRIDGE_KID).await;
    let p = victim_gateway(&bridge).await;
    let token = bridge_token_for(&bridge, VICTIM, TENANT);
    let page = "markdown/instances/conversation/teams-1.md";
    let (_, created) = call(
        &p,
        &token,
        "update_page",
        json!({ "page_id": page, "base_sha256": "",
                "content": "---\nkind: instance\nskill: conversation\nid: teams-1\ntitle: chat\nowner: \"user:victim\"\n---\n# chat\n" }),
    )
    .await;
    assert_eq!(
        created["result"]["structuredContent"]["ok"], true,
        "{created}"
    );
    use sha2::Digest as _;
    let group = format!(
        "page-chat:v1:{:x}:{page}",
        sha2::Sha256::digest(VICTIM.as_bytes())
    );
    let (_, appended) = call(
        &p,
        &token,
        "append_message",
        json!({ "chat_group_id": group, "role": "user", "content": "hello", "msg_id": "m-1" }),
    )
    .await;
    assert!(appended.get("error").is_none(), "{appended}");
    let (_, assistant) = call(
        &p,
        &token,
        "append_message",
        json!({ "chat_group_id": group, "role": "assistant", "content": "fake answer" }),
    )
    .await;
    forbidden(&assistant, "a bridge never writes the answer");
    let (_, captured) = call(
        &p,
        &token,
        "capture_event",
        json!({ "event_id": "msteams-1", "source": "msteams", "label_skill": "chat",
                "title": "chat turn", "instance_page_id": page,
                "provenance": { "chat_group_id": group, "user_msg_id": "m-1" } }),
    )
    .await;
    assert!(captured.get("error").is_none(), "{captured}");
    let (_, listed) = call(
        &p,
        &token,
        "list_messages",
        json!({ "chat_group_id": group }),
    )
    .await;
    assert!(listed.get("error").is_none(), "{listed}");
}

/// The bridge entry is pinned to one tenant: a token from it naming another
/// tenant is refused, even before the served-tenant check would.
#[tokio::test]
async fn a_bridge_token_naming_another_tenant_is_refused() {
    let bridge = ExtraIssuer::start_with_own_key(BRIDGE_KID).await;
    let p = victim_gateway(&bridge).await;
    let token = bridge_token_for(&bridge, VICTIM, "globex");
    let (status, body) = call(&p, &token, "whoami", json!({})).await;
    assert_eq!(status, 401, "{body}");
}
