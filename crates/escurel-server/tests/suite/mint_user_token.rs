//! `mint_user_token` (#830): a chat bridge holds no signing key; the gateway
//! mints each turn's token for it, deciding the subject (principal template),
//! the groups (the union over the configured group directories — Microsoft
//! Graph, Google Cloud Identity, Okta — through local stand-ins) and, in a
//! shared chat, the group principal an admin mapped it to.
//!
//! Real gateway, real verifier, real egress and caches; only the identity
//! providers (`DirectoryStandIn`: token endpoints and group listings) are
//! stood in for. ACLs enforced.

use std::time::Duration;

use base64::Engine as _;
use escurel_test_support::{
    AuthMode, BF_ISSUER, BotFrameworkStandIn, CONNECTOR_TOKEN, ConfigOverrides, DirectoryStandIn,
    EscurelProcess, FixtureBuilder, GRAPH_TENANT, Opts, Role, TEAMS_KID, WEBCHAT_KID, WriteAclMode,
    bridge_mint,
};
use serde_json::{Value, json};

const TENANT: &str = "acme";
const ALICE_OID: &str = "alice-oid";
const BOB_OID: &str = "bob-oid";
const ALICE: &str = "user:alice-oid";
const ALICE_EMAIL: &str = "alice@acme.example";
const BOB_EMAIL: &str = "bob@acme.example";
const GROUP_CHAT: &str = "19:sales-chat@thread.v2";

const DEAL_SKILL: &str = "---\nkind: skill\nid: deal\n\
    description: A sales deal, readable by the sales group.\n\
    acl:\n  read: [sales]\n---\n# deal\n";
const CHAT_SKILL: &str =
    "---\nkind: skill\nid: chat\ndescription: Answer questions.\n---\n# chat\n";
const CONVERSATION_SKILL: &str = "---\nkind: skill\nid: conversation\ndescription: A chat.\n\
    owner_field: owner\nacl:\n  read: [owner]\n  create: [owner]\n  update: [owner]\n\
    chat:\n  page_bound: true\n---\n# conversation\n";

struct World {
    gw: EscurelProcess,
    graph: DirectoryStandIn,
}

/// Reserved and privileged names a directory may well contain, next to real ones.
const ALICE_GROUPS: &[&str] = &[
    "escurel-sales",
    "escurel-admin",
    "escurel-public",
    "escurel-owner",
    "escurel-escurel:admin",
    "Marketing",
];

async fn gateway(mint: escurel_server::group_directory::BridgeMintConfig) -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("deal", DEAL_SKILL)
                .instance(
                    "deal",
                    "q3",
                    "---\nkind: instance\nskill: deal\nid: q3\n---\n# Q3\n",
                )
                .skill("chat", CHAT_SKILL)
                .skill("conversation", CONVERSATION_SKILL)
                .done(),
        ),
        config_overrides: ConfigOverrides {
            write_acl: Some(WriteAclMode::Enforce),
            signing: true,
            bridge_mint: Some(mint),
            ..Default::default()
        },
    })
    .await
}

async fn world_with(cache: Option<(Duration, Duration)>) -> World {
    let graph = DirectoryStandIn::graph().await;
    graph.member_of(ALICE_OID, ALICE_GROUPS);
    graph.member_of(BOB_OID, &["escurel-support"]);
    let gw = gateway(bridge_mint(
        "user:{aadObjectId}",
        vec![graph.config("escurel-")],
        cache,
    ))
    .await;
    World { gw, graph }
}

async fn world() -> World {
    world_with(None).await
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

fn ok(body: &Value, what: &str) -> Value {
    assert!(body.get("error").is_none(), "{what}: {body}");
    body["result"]["structuredContent"].clone()
}

fn refused(body: &Value, code: &str, what: &str) {
    assert_eq!(body["error"]["data"]["code"], code, "{what}: {body}");
}

fn bridge_credential(p: &EscurelProcess) -> String {
    p.mint_token_with_groups(TENANT, "svc:chat-bridge", &["escurel:bridge"], false)
}

fn personal(oid: &str) -> Value {
    json!({ "channel": "msteams", "tenant_id": GRAPH_TENANT, "aad_object_id": oid,
            "user_id": format!("29:{oid}"),
            "conversation": { "kind": "personal", "id": "a:1personal" } })
}

fn in_group_chat(oid: &str) -> Value {
    json!({ "channel": "msteams", "tenant_id": GRAPH_TENANT, "aad_object_id": oid,
            "conversation": { "kind": "group", "id": GROUP_CHAT } })
}

fn claims(token: &str) -> Value {
    let payload = token.split('.').nth(1).unwrap();
    serde_json::from_slice(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(payload)
            .unwrap(),
    )
    .unwrap()
}

async fn mint(p: &EscurelProcess, args: Value) -> Value {
    let (_, body) = call(p, &bridge_credential(p), "mint_user_token", args).await;
    ok(&body, "mint_user_token")
}

async fn map_group_chat(p: &EscurelProcess, group: &str) {
    let admin = p.mint_token(TENANT, Role::Admin);
    let (_, wrote) = call(
        p,
        &admin,
        "update_page",
        json!({ "page_id": "markdown/instances/chat-channel/sales-chat.md",
                "content": format!("---\nkind: instance\nskill: chat-channel\nid: sales-chat\n\
                    channel: msteams\nconversation_id: \"{GROUP_CHAT}\"\ngroup: {group}\n---\n# Sales chat\n") }),
    )
    .await;
    assert_eq!(ok(&wrote, "map")["ok"], true, "{wrote}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_a_bridges_own_credential_mints_a_user_token() {
    let w = world().await;
    let p = &w.gw;
    let member = p.mint_token_with_sub(TENANT, Role::Agent, "user:mallory");
    let admin = p.mint_token(TENANT, Role::Admin);
    let runner = p.mint_token_with_groups(TENANT, "runner:hq", &["escurel:runner"], false);
    for (who, token, code) in [
        ("a member", member, "admin_required"),
        ("an admin", admin, "bridge_required"),
        ("a runner", runner, "admin_required"),
    ] {
        let (_, body) = call(p, &token, "mint_user_token", personal(ALICE_OID)).await;
        refused(&body, code, who);
    }
    // A machine token carrying the bridge role (a run's, acting for someone) is not the bridge.
    let machine = p.resign_token_with_claim(
        &bridge_credential(p),
        "act",
        json!({ "sub": "escurel-runner" }),
    );
    let (_, body) = call(p, &machine, "mint_user_token", personal(ALICE_OID)).await;
    refused(
        &body,
        "admin_required",
        "a machine token holds no service role",
    );
    // A minted turn token cannot mint another.
    let turn = mint(p, personal(ALICE_OID)).await["token"]
        .as_str()
        .unwrap()
        .to_owned();
    let (_, body) = call(p, &turn, "mint_user_token", personal(ALICE_OID)).await;
    assert!(
        body.get("error").is_some(),
        "a turn token mints nothing: {body}"
    );
    // Another Entra tenant is not looked up.
    let mut foreign = personal(ALICE_OID);
    foreign["tenant_id"] = json!("other-tenant");
    let (_, body) = call(p, &bridge_credential(p), "mint_user_token", foreign).await;
    refused(&body, "tenant_not_served", "another tenant");
    // The bridge's own credential sees the tool; a member's does not.
    let listed: Value = reqwest::Client::new()
        .post(p.mcp_url())
        .bearer_auth(bridge_credential(p))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "mint_user_token"),
        "{listed}"
    );
}

/// The 1:1 turn: the person, with the groups Graph names — reserved and
/// privileged ones dropped — and still the bridge's confinement.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_personal_turn_carries_the_askers_graph_groups_and_stays_confined() {
    let w = world().await;
    let p = &w.gw;
    let minted = mint(p, personal(ALICE_OID)).await;
    assert_eq!(minted["subject"], ALICE);
    let token = minted["token"].as_str().unwrap().to_owned();
    let c = claims(&token);
    assert_eq!(
        c["roles"],
        json!(["sales"]),
        "only prefixed, assignable groups; reserved names dropped: {c}"
    );
    assert_eq!(c["bridge_channel"], "msteams");
    assert!(c.get("act").is_none(), "{c}");
    assert!(
        c["exp"].as_u64().unwrap() - c["iat"].as_u64().unwrap() <= 900,
        "{c}"
    );
    assert!(w.graph.lookups() >= 1, "Graph was asked");

    let (_, who) = call(p, &token, "whoami", json!({})).await;
    let who = ok(&who, "whoami");
    assert_eq!(who["subject"], ALICE);
    assert_eq!(who["is_admin"], false);

    // Confined like any bridge token: the six tools only, no non-tool surface.
    let (_, read) = call(
        p,
        &token,
        "expand",
        json!({ "page_id": "markdown/instances/deal/q3.md" }),
    )
    .await;
    assert_eq!(read["error"]["code"], -32003, "{read}");
    let http = reqwest::Client::new();
    for (what, status) in [
        (
            "/ingest",
            http.post(format!("{}/ingest", p.base_url()))
                .bearer_auth(&token)
                .json(&json!({ "blob_id": "x", "content_type": "text/plain" }))
                .send()
                .await
                .unwrap()
                .status(),
        ),
        (
            "/blob",
            http.get(format!(
                "{}/blob/markdown/instances/deal/q3.md",
                p.base_url()
            ))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .status(),
        ),
        (
            "/ws",
            http.get(format!("{}/ws", p.base_url()))
                .bearer_auth(&token)
                .header("connection", "upgrade")
                .header("upgrade", "websocket")
                .header("sec-websocket-version", "13")
                .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
                .send()
                .await
                .unwrap()
                .status(),
        ),
    ] {
        assert_eq!(status, 403, "{what}");
    }

    // Its capture says the bridge relayed it, and carries the groups the gateway resolved — not
    // whatever the caller wrote.
    let (_, captured) = call(
        p,
        &token,
        "capture_event",
        json!({ "event_id": "t-1", "source": "msteams", "label_skill": "chat", "title": "chat turn",
                "provenance": { "captured_groups": ["admin", "finance"], "asked_by": "user:bob" } }),
    )
    .await;
    ok(&captured, "capture");
    let admin = p.mint_token(TENANT, Role::Admin);
    let (_, ev) = call(p, &admin, "list_events", json!({ "event_id": "t-1" })).await;
    let ev = &ok(&ev, "list_events")["events"][0];
    assert_eq!(ev["provenance"]["captured_by"], ALICE, "{ev}");
    assert_eq!(ev["provenance"]["captured_via"], "bridge:msteams");
    assert_eq!(ev["provenance"]["captured_groups"], json!(["sales"]));
    assert!(ev["provenance"].get("asked_by").is_none(), "{ev}");

    // A person's own token cannot forge the stamps either.
    let own = p.mint_token_with_sub(TENANT, Role::Agent, "user:mallory");
    let (_, forged) = call(
        p,
        &own,
        "capture_event",
        json!({ "event_id": "t-2", "source": "x", "label_skill": "chat", "title": "x",
                "provenance": { "captured_groups": ["sales"], "asked_by": ALICE } }),
    )
    .await;
    ok(&forged, "own capture");
    let (_, ev) = call(p, &admin, "list_events", json!({ "event_id": "t-2" })).await;
    let ev = &ok(&ev, "list_events")["events"][0];
    assert!(ev["provenance"].get("captured_groups").is_none(), "{ev}");
    assert!(ev["provenance"].get("asked_by").is_none(), "{ev}");
}

/// A group chat acts as the group an admin mapped it to, for members only.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_group_chat_acts_as_its_mapped_group_for_members_only() {
    let w = world().await;
    let p = &w.gw;
    let bridge = bridge_credential(p);

    let (_, body) = call(p, &bridge, "mint_user_token", in_group_chat(ALICE_OID)).await;
    refused(&body, "chat_channel_unmapped", "an unmapped group chat");
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains(GROUP_CHAT) && m.contains("chat-channel")),
        "the refusal says how an admin maps it: {body}"
    );

    map_group_chat(p, "sales").await;
    let minted = mint(p, in_group_chat(ALICE_OID)).await;
    assert_eq!(minted["subject"], "group:sales");
    assert_eq!(minted["asker"], ALICE);
    let token = minted["token"].as_str().unwrap().to_owned();
    let c = claims(&token);
    assert_eq!(c["sub"], "group:sales");
    assert_eq!(c["roles"], json!(["sales"]), "the group's access only");
    assert_eq!(c["act"]["sub"], ALICE, "the asker is the actor");

    // Bob is in the chat but not in `sales`.
    let (_, body) = call(p, &bridge, "mint_user_token", in_group_chat(BOB_OID)).await;
    refused(&body, "chat_channel_not_member", "a non-member");

    // What the group turn captures names the group as the capturer and the asker beside it.
    let (_, captured) = call(
        p,
        &token,
        "capture_event",
        json!({ "event_id": "g-1", "source": "msteams", "label_skill": "chat", "title": "chat turn" }),
    )
    .await;
    ok(&captured, "capture");
    let admin = p.mint_token(TENANT, Role::Admin);
    let (_, ev) = call(p, &admin, "list_events", json!({ "event_id": "g-1" })).await;
    let ev = &ok(&ev, "list_events")["events"][0];
    assert_eq!(ev["provenance"]["captured_by"], "group:sales");
    assert_eq!(ev["provenance"]["asked_by"], ALICE);
    assert_eq!(ev["provenance"]["captured_via"], "bridge:msteams");

    // Unmapped again (archived): declined from the next turn on.
    let (_, wrote) = call(
        p,
        &admin,
        "update_page",
        json!({ "page_id": "markdown/instances/chat-channel/sales-chat.md",
                "content": format!("---\nkind: instance\nskill: chat-channel\nid: sales-chat\n\
                    channel: msteams\nconversation_id: \"{GROUP_CHAT}\"\ngroup: sales\narchived: true\n---\n") }),
    )
    .await;
    assert_eq!(ok(&wrote, "archive")["ok"], true, "{wrote}");
    let (_, body) = call(p, &bridge, "mint_user_token", in_group_chat(ALICE_OID)).await;
    refused(&body, "chat_channel_unmapped", "an archived mapping");
}

/// A minted group token cannot read what only the asker may read: the
/// asker's own 1:1 conversation stays theirs.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_group_token_cannot_reach_the_askers_private_chat() {
    let w = world().await;
    let p = &w.gw;
    map_group_chat(p, "sales").await;
    let mine = mint(p, personal(ALICE_OID)).await["token"]
        .as_str()
        .unwrap()
        .to_owned();
    let page = "markdown/instances/conversation/teams-alice.md";
    let (_, created) = call(
        p,
        &mine,
        "update_page",
        json!({ "page_id": page, "base_sha256": "",
                "content": format!("---\nkind: instance\nskill: conversation\nid: teams-alice\n\
                    title: chat\nowner: \"{ALICE}\"\n---\n# chat\n") }),
    )
    .await;
    assert_eq!(ok(&created, "create")["ok"], true, "{created}");
    use sha2::Digest as _;
    let group = format!(
        "page-chat:v1:{:x}:{page}",
        sha2::Sha256::digest(ALICE.as_bytes())
    );
    let (_, appended) = call(
        p,
        &mine,
        "append_message",
        json!({ "chat_group_id": group, "role": "user", "content": "my salary is 7", "msg_id": "m-1" }),
    )
    .await;
    ok(&appended, "append");
    // CONTROL: alice's own turn token reads it back.
    let (_, own) = call(p, &mine, "list_messages", json!({ "chat_group_id": group })).await;
    assert_eq!(ok(&own, "own")["messages"].as_array().unwrap().len(), 1);

    let shared = mint(p, in_group_chat(ALICE_OID)).await["token"]
        .as_str()
        .unwrap()
        .to_owned();
    let (_, read) = call(
        p,
        &shared,
        "list_messages",
        json!({ "chat_group_id": group }),
    )
    .await;
    assert!(
        read.get("error").is_some()
            || ok(&read, "list")["messages"]
                .as_array()
                .is_none_or(Vec::is_empty),
        "the group token reads nothing of alice's own chat: {read}"
    );
    let (_, wrote) = call(
        p,
        &shared,
        "append_message",
        json!({ "chat_group_id": group, "role": "user", "content": "x" }),
    )
    .await;
    assert!(wrote.get("error").is_some(), "{wrote}");
}

/// A directory down: a cached answer stands in for a while, then the turn is
/// refused as retryable; back up, it is answered again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn while_a_directory_is_down_a_cached_answer_stands_in_then_the_refusal_is_retryable() {
    let w = world_with(Some((Duration::from_millis(300), Duration::from_secs(2)))).await;
    let p = &w.gw;
    let bridge = bridge_credential(p);
    mint(p, personal(ALICE_OID)).await;
    w.graph.down();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let stale = mint(p, personal(ALICE_OID)).await;
    assert_eq!(
        claims(stale["token"].as_str().unwrap())["roles"],
        json!(["sales"])
    );

    tokio::time::sleep(Duration::from_secs(2)).await;
    let (_, body) = call(p, &bridge, "mint_user_token", personal(ALICE_OID)).await;
    refused(&body, "directory_unavailable", "past the stale window");
    assert_eq!(body["error"]["data"]["retryable"], true, "{body}");
    // Someone never looked up before has nothing cached at all.
    let (_, body) = call(p, &bridge, "mint_user_token", personal(BOB_OID)).await;
    refused(&body, "directory_unavailable", "no cache");

    w.graph.up();
    mint(p, personal(ALICE_OID)).await;
}

/// Mapping pages are the admin's alone, and a malformed one is refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_an_admin_maps_a_chat_and_a_mapping_must_say_one_thing() {
    let w = world().await;
    let p = &w.gw;
    let member = p.mint_token_with_groups(TENANT, "user:mallory", &["sales"], false);
    let page = "markdown/instances/chat-channel/x.md";
    let good = format!(
        "---\nkind: instance\nskill: chat-channel\nid: x\nchannel: msteams\n\
         conversation_id: \"{GROUP_CHAT}\"\ngroup: sales\n---\n"
    );
    let (_, body) = call(
        p,
        &member,
        "update_page",
        json!({ "page_id": page, "content": good }),
    )
    .await;
    assert!(
        body.get("error").is_some() || body["result"]["structuredContent"]["ok"] == false,
        "a member cannot map a chat: {body}"
    );
    let admin = p.mint_token(TENANT, Role::Admin);
    for bad in [
        "group: admin",
        "group: escurel:admin",
        "group: sales\nteam_id: t\nchannel_id: c",
    ] {
        let content = format!(
            "---\nkind: instance\nskill: chat-channel\nid: x\nchannel: msteams\n\
             conversation_id: \"{GROUP_CHAT}\"\n{bad}\n---\n"
        );
        let (_, body) = call(
            p,
            &admin,
            "update_page",
            json!({ "page_id": page, "content": content }),
        )
        .await;
        let out = ok(&body, bad);
        assert_eq!(out["ok"], false, "{bad}: {out}");
        assert!(
            out["issues"]
                .as_array()
                .unwrap()
                .iter()
                .any(|i| i["code"] == "chat_channel_invalid"),
            "{bad}: {out}"
        );
    }
}

fn personal_with_email(oid: &str, email: Option<&str>) -> Value {
    let mut a = personal(oid);
    if let Some(e) = email {
        a["email"] = json!(e);
    }
    a
}

/// The 1:1 asker's `sales` comes from each provider alone, and from a
/// two-provider union; an email-keyed provider adds nothing for a sender
/// with no email, and the others still answer.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn each_directory_alone_and_a_union_of_two_grant_the_askers_groups() {
    let graph = DirectoryStandIn::graph().await;
    graph.member_of(ALICE_OID, &["escurel-sales"]);
    graph.known_as(ALICE_OID, ALICE_EMAIL, &[]);
    let google = DirectoryStandIn::google().await;
    google.member_of(ALICE_EMAIL, &["escurel-sales", "escurel-ops"]);
    let okta = DirectoryStandIn::okta().await;
    okta.member_of(
        ALICE_EMAIL,
        &["escurel-sales", "escurel-support", "escurel-admin"],
    );

    for (what, dirs, want) in [
        ("graph", vec![graph.config("escurel-")], json!(["sales"])),
        (
            "google",
            vec![google.config("escurel-")],
            json!(["ops", "sales"]),
        ),
        (
            "okta",
            vec![okta.config("escurel-")],
            json!(["sales", "support"]),
        ),
        (
            "google + okta",
            vec![google.config("escurel-"), okta.config("escurel-")],
            json!(["ops", "sales", "support"]),
        ),
    ] {
        let gw = gateway(bridge_mint("user:{aadObjectId}", dirs, None)).await;
        let minted = mint(&gw, personal_with_email(ALICE_OID, Some(ALICE_EMAIL))).await;
        let token = minted["token"].as_str().unwrap();
        assert_eq!(claims(token)["roles"], want, "{what}");
        // The token reads through the bridge's own surface only; its groups are what a run gets
        // (`captured_groups`), checked end to end in the chat bridge's roundtrip.
        let (_, who) = call(&gw, token, "whoami", json!({})).await;
        assert_eq!(ok(&who, what)["subject"], ALICE, "{what}");
    }

    // No email: Google and Okta contribute nothing, Graph still answers.
    let gw = gateway(bridge_mint(
        "user:{aadObjectId}",
        vec![
            graph.config("escurel-"),
            google.config("escurel-"),
            okta.config("escurel-"),
        ],
        None,
    ))
    .await;
    let minted = mint(&gw, personal_with_email(ALICE_OID, None)).await;
    assert_eq!(
        claims(minted["token"].as_str().unwrap())["roles"],
        json!(["sales"])
    );
}

/// A group-chat member check passes when the membership comes only from Okta.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_group_chat_membership_from_okta_alone_is_enough() {
    let graph = DirectoryStandIn::graph().await;
    graph.member_of(BOB_OID, &["escurel-support"]);
    graph.known_as(BOB_OID, BOB_EMAIL, &[]);
    let okta = DirectoryStandIn::okta().await;
    okta.member_of(BOB_EMAIL, &["escurel-sales"]);
    let gw = gateway(bridge_mint(
        "user:{aadObjectId}",
        vec![graph.config("escurel-"), okta.config("escurel-")],
        None,
    ))
    .await;
    map_group_chat(&gw, "sales").await;
    let mut args = in_group_chat(BOB_OID);
    args["email"] = json!(BOB_EMAIL);
    let minted = mint(&gw, args).await;
    assert_eq!(minted["subject"], "group:sales");
    assert_eq!(minted["asker"], "user:bob-oid");
    // Without the email Okta cannot be asked, and Graph alone does not put bob in `sales`.
    let (_, body) = call(
        &gw,
        &bridge_credential(&gw),
        "mint_user_token",
        in_group_chat(BOB_OID),
    )
    .await;
    refused(&body, "chat_channel_not_member", "graph alone");
}

/// 429 and 5xx are retryable (the cache stands in, then the turn is refused
/// retryable); 401 / 403 / 404 are permanent — that directory grants nothing
/// and the others still answer.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_directory_status_is_retryable_or_permanent() {
    let graph = DirectoryStandIn::graph().await;
    graph.member_of(ALICE_OID, &["escurel-sales"]);
    graph.known_as(ALICE_OID, ALICE_EMAIL, &[]);
    let google = DirectoryStandIn::google().await;
    google.member_of(ALICE_EMAIL, &["escurel-ops"]);
    let okta = DirectoryStandIn::okta().await;
    okta.member_of(ALICE_EMAIL, &["escurel-support"]);
    let gw = gateway(bridge_mint(
        "user:{aadObjectId}",
        vec![
            graph.config("escurel-"),
            google.config("escurel-"),
            okta.config("escurel-"),
        ],
        Some((Duration::ZERO, Duration::ZERO)),
    ))
    .await;
    let roles = |minted: &Value| claims(minted["token"].as_str().unwrap())["roles"].clone();
    let asker = personal_with_email(ALICE_OID, Some(ALICE_EMAIL));
    assert_eq!(
        roles(&mint(&gw, asker.clone()).await),
        json!(["ops", "sales", "support"])
    );
    for status in [401, 403, 404] {
        okta.answer_with(status);
        assert_eq!(
            roles(&mint(&gw, asker.clone()).await),
            json!(["ops", "sales"]),
            "okta {status} is permanent: it grants nothing, the others answer"
        );
    }
    okta.up();
    for status in [429, 500, 503] {
        google.answer_with(status);
        let (_, body) = call(
            &gw,
            &bridge_credential(&gw),
            "mint_user_token",
            asker.clone(),
        )
        .await;
        refused(&body, "directory_unavailable", &format!("google {status}"));
        assert_eq!(body["error"]["data"]["retryable"], true, "{body}");
    }
    google.up();
    assert_eq!(
        roles(&mint(&gw, asker).await),
        json!(["ops", "sales", "support"])
    );
}

/// Crew S-F3: a template that keys on the bridge-supplied `user_id` passes
/// boot validation, but the subject the ids produce is checked again — a
/// `user_id` of `group:finance` must not mint the finance group principal.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_platform_id_cannot_mint_a_group_or_service_subject() {
    let graph = DirectoryStandIn::graph().await;
    graph.member_of(ALICE_OID, &["escurel-sales"]);
    graph.known_as(ALICE_OID, ALICE_EMAIL, &[]);
    let gw = gateway(bridge_mint(
        "{userId}",
        vec![graph.config("escurel-")],
        None,
    ))
    .await;
    for user_id in ["group:finance", "agent:x", "svc:chat-bridge", "29:a/../b"] {
        let mut args = personal(ALICE_OID);
        args["user_id"] = json!(user_id);
        let (_, body) = call(&gw, &bridge_credential(&gw), "mint_user_token", args).await;
        refused(&body, "no_principal", user_id);
    }
    let minted = mint(&gw, personal(ALICE_OID)).await;
    assert_eq!(minted["subject"], format!("29:{ALICE_OID}"));
}

/// Crew S-F1: a group any employee can create and name — a Microsoft 365
/// group (every Team), a Google group outside the pinned domain, an Okta app
/// group — called `escurel-finance` grants nothing by default; an explicit
/// opt-in (or the id-keyed map file) is the only way it counts.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_group_anyone_can_name_grants_nothing_by_default() {
    use escurel_server::group_directory::DirectoryConfig;
    let graph = DirectoryStandIn::graph().await;
    graph.member_of(ALICE_OID, &["escurel-sales"]);
    graph.known_as(ALICE_OID, ALICE_EMAIL, &[]);
    graph.member_of_shared(ALICE_OID, &["escurel-finance"]);
    let google = DirectoryStandIn::google().await;
    google.member_of_shared(ALICE_EMAIL, &["escurel-finance"]);
    let okta = DirectoryStandIn::okta().await;
    okta.member_of_shared(ALICE_EMAIL, &["escurel-finance"]);
    let roles = |minted: &Value| claims(minted["token"].as_str().unwrap())["roles"].clone();
    let asker = personal_with_email(ALICE_OID, Some(ALICE_EMAIL));

    let gw = gateway(bridge_mint(
        "user:{aadObjectId}",
        vec![
            graph.config("escurel-"),
            google.config("escurel-"),
            okta.config("escurel-"),
        ],
        None,
    ))
    .await;
    assert_eq!(
        roles(&mint(&gw, asker.clone()).await),
        json!(["sales"]),
        "only the security group counts"
    );

    let mut opted_in = graph.config("escurel-");
    if let DirectoryConfig::Graph(g) = &mut opted_in {
        g.include_m365_groups = true;
    }
    let mut app_groups = okta.config("escurel-");
    if let DirectoryConfig::Okta(o) = &mut app_groups {
        o.include_app_groups = true;
    }
    let gw = gateway(bridge_mint(
        "user:{aadObjectId}",
        vec![opted_in, app_groups],
        None,
    ))
    .await;
    assert_eq!(
        roles(&mint(&gw, asker).await),
        json!(["finance", "sales"]),
        "an explicit opt-in counts them"
    );
}

/// Crew S-F2: with Graph configured, the email (what Google and Okta key on)
/// must be one of the addresses Graph knows the object id by, and an object
/// id Graph does not know mints nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bridge_cannot_pair_one_persons_id_with_anothers_email() {
    let graph = DirectoryStandIn::graph().await;
    graph.member_of(ALICE_OID, &["escurel-support"]);
    graph.known_as(ALICE_OID, ALICE_EMAIL, &["a.smith@acme.example"]);
    graph.member_of(BOB_OID, &[]);
    graph.known_as(BOB_OID, BOB_EMAIL, &[]);
    let okta = DirectoryStandIn::okta().await;
    okta.member_of(BOB_EMAIL, &["escurel-finance"]);
    okta.member_of("a.smith@acme.example", &["escurel-sales"]);
    let gw = gateway(bridge_mint(
        "user:{aadObjectId}",
        vec![graph.config("escurel-"), okta.config("escurel-")],
        None,
    ))
    .await;
    // Alice's object id with Bob's address: Bob's finance group must not ride on Alice's subject.
    let (_, body) = call(
        &gw,
        &bridge_credential(&gw),
        "mint_user_token",
        personal_with_email(ALICE_OID, Some(BOB_EMAIL)),
    )
    .await;
    refused(&body, "identity_mismatch", "alice's id, bob's email");
    let mut upn = personal(ALICE_OID);
    upn["upn"] = json!(BOB_EMAIL);
    let (_, body) = call(&gw, &bridge_credential(&gw), "mint_user_token", upn).await;
    refused(&body, "identity_mismatch", "alice's id, bob's upn");
    // An object id Graph does not know is nobody.
    let (_, body) = call(
        &gw,
        &bridge_credential(&gw),
        "mint_user_token",
        personal("nobody-oid"),
    )
    .await;
    refused(&body, "no_principal", "an unknown object id");
    // One of Alice's own addresses (otherMails, any case) is hers.
    let minted = mint(
        &gw,
        personal_with_email(ALICE_OID, Some("A.Smith@acme.example")),
    )
    .await;
    assert_eq!(
        claims(minted["token"].as_str().unwrap())["roles"],
        json!(["sales", "support"])
    );
}

/// Crew U-F7: a gateway with no group directory still mints 1:1 turns
/// (groupless), and refuses every shared chat with its own code.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn without_a_directory_one_to_one_turns_mint_groupless_and_shared_chats_are_refused() {
    let gw = gateway(bridge_mint("user:{aadObjectId}", vec![], None)).await;
    let minted = mint(&gw, personal_with_email(ALICE_OID, Some(ALICE_EMAIL))).await;
    assert_eq!(minted["subject"], ALICE);
    let roles = &claims(minted["token"].as_str().unwrap())["roles"];
    assert!(
        roles.is_null() || roles == &json!([]),
        "no directory, no groups: {roles}"
    );
    map_group_chat(&gw, "sales").await;
    let (_, body) = call(
        &gw,
        &bridge_credential(&gw),
        "mint_user_token",
        in_group_chat(ALICE_OID),
    )
    .await;
    refused(&body, "chat_channel_no_directory", "a mapped group chat");
}

/// Crew U-F11: the refusals a bridge must tell apart, each with its code.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ambiguous_unmappable_unprincipled_and_unsupported_mints_are_refused_by_code() {
    let w = world().await;
    let p = &w.gw;
    // Two pages naming different groups for one chat.
    map_group_chat(p, "sales").await;
    let admin = p.mint_token(TENANT, Role::Admin);
    let (_, wrote) = call(
        p,
        &admin,
        "update_page",
        json!({ "page_id": "markdown/instances/chat-channel/sales-chat-2.md",
                "content": format!("---\nkind: instance\nskill: chat-channel\nid: sales-chat-2\n\
                    channel: msteams\nconversation_id: \"{GROUP_CHAT}\"\ngroup: support\n---\n# again\n") }),
    )
    .await;
    assert_eq!(ok(&wrote, "second map")["ok"], true, "{wrote}");
    let (_, body) = call(
        p,
        &bridge_credential(p),
        "mint_user_token",
        in_group_chat(ALICE_OID),
    )
    .await;
    refused(&body, "chat_channel_ambiguous", "two groups for one chat");
    // A channel turn without its team and channel ids cannot be mapped, and the message says so
    // instead of suggesting a `conversation_id` page that would never match (crew Q-F7).
    let mut channel = personal(ALICE_OID);
    channel["conversation"] = json!({ "kind": "channel", "id": "19:c@thread.tacv2;messageid=1" });
    let (_, body) = call(p, &bridge_credential(p), "mint_user_token", channel).await;
    refused(&body, "chat_channel_unmapped", "an unmappable channel");
    let msg = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        msg.contains("cannot be mapped") && !msg.contains("conversation_id"),
        "{msg}"
    );

    // A template field the sender lacks.
    let user_keyed = gateway(bridge_mint(
        "{userId}",
        vec![w.graph.config("escurel-")],
        None,
    ))
    .await;
    let mut no_user = personal(ALICE_OID);
    no_user.as_object_mut().unwrap().remove("user_id");
    let (_, body) = call(
        &user_keyed,
        &bridge_credential(&user_keyed),
        "mint_user_token",
        no_user,
    )
    .await;
    refused(&body, "no_principal", "no user id for a {userId} template");

    // A gateway that does not mint for a bridge at all.
    let plain = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(FixtureBuilder::new().tenant(TENANT).done()),
        config_overrides: ConfigOverrides {
            signing: true,
            ..Default::default()
        },
    })
    .await;
    let (_, body) = call(
        &plain,
        &bridge_credential(&plain),
        "mint_user_token",
        personal(ALICE_OID),
    )
    .await;
    refused(&body, "unsupported", "no bridge minting configured");
}

// ---- #835: the gateway checks the Teams asker itself -------------------

const BOT_APP: &str = "test-bot-app";
const PERSONAL_CHAT: &str = "a:1personal";

/// A gateway that verifies the forwarded activity token and asks the Bot
/// Framework roster (the stand-in) itself, with Graph as its directory.
struct Verified {
    gw: EscurelProcess,
    bf: BotFrameworkStandIn,
    _graph: DirectoryStandIn,
}

async fn verified_world() -> Verified {
    let graph = DirectoryStandIn::graph().await;
    graph.member_of(ALICE_OID, &["escurel-sales"]);
    graph.known_as(ALICE_OID, ALICE_EMAIL, &[]);
    graph.member_of(BOB_OID, &["escurel-support"]);
    graph.known_as(BOB_OID, BOB_EMAIL, &[]);
    let bf = BotFrameworkStandIn::start().await;
    bf.add_member("29:alice", ALICE_OID, ALICE_EMAIL, GRAPH_TENANT);
    bf.add_member("29:bob", BOB_OID, BOB_EMAIL, GRAPH_TENANT);
    let mut mint = bridge_mint("user:{aadObjectId}", vec![graph.config("escurel-")], None);
    mint.teams = Some(bf.gateway_verification(BOT_APP));
    Verified {
        gw: gateway(mint).await,
        bf,
        _graph: graph,
    }
}

/// What the bridge sends for a 1:1 turn by `user` (as Teams names them), with
/// the activity's bearer forwarded verbatim.
fn verified_turn(bf: &BotFrameworkStandIn, user: &str, oid: &str, token: Option<&str>) -> Value {
    let mut args = json!({ "channel": "msteams", "tenant_id": GRAPH_TENANT, "aad_object_id": oid,
        "user_id": user, "service_url": bf.base,
        "conversation": { "kind": "personal", "id": PERSONAL_CHAT } });
    if let Some(t) = token {
        args["activity_token"] = json!(t);
    }
    args
}

fn member_calls(bf: &BotFrameworkStandIn) -> usize {
    bf.seen.lock().unwrap().member_calls
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_verified_asker_mints_with_microsofts_identity_and_the_roster_answer_is_cached() {
    let w = verified_world().await;
    let token = w.bf.token(BOT_APP);
    let minted = mint(
        &w.gw,
        verified_turn(&w.bf, "29:alice", ALICE_OID, Some(&token)),
    )
    .await;
    assert_eq!(minted["subject"], ALICE);
    assert_eq!(
        claims(minted["token"].as_str().unwrap())["roles"],
        json!(["sales"])
    );
    assert_eq!(member_calls(&w.bf), 1, "the gateway asked the roster");
    assert_eq!(
        w.bf.seen.lock().unwrap().member_authorizations[0],
        format!("Bearer {CONNECTOR_TOKEN}"),
        "with the bot's own connector token"
    );
    // The bridge asserted no email; the gateway used Microsoft's. A busy chat asks once.
    mint(
        &w.gw,
        verified_turn(&w.bf, "29:alice", ALICE_OID, Some(&token)),
    )
    .await;
    assert_eq!(member_calls(&w.bf), 1, "a positive roster answer is cached");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_forged_identity_disagrees_with_the_roster_and_is_refused() {
    let w = verified_world().await;
    let token = w.bf.token(BOT_APP);
    // Alice's Teams id, Bob's object id: the roster says Alice.
    let (_, body) = call(
        &w.gw,
        &bridge_credential(&w.gw),
        "mint_user_token",
        verified_turn(&w.bf, "29:alice", BOB_OID, Some(&token)),
    )
    .await;
    refused(&body, "identity_mismatch", "a forged aad_object_id");
    assert_eq!(body["error"]["data"]["retryable"], false);
    let mut email = verified_turn(&w.bf, "29:alice", ALICE_OID, Some(&token));
    email["email"] = json!(BOB_EMAIL);
    let (_, body) = call(&w.gw, &bridge_credential(&w.gw), "mint_user_token", email).await;
    refused(&body, "identity_mismatch", "a forged email");
    let mut tenant = verified_turn(&w.bf, "29:alice", ALICE_OID, Some(&token));
    tenant["tenant_id"] = json!("another-tenant");
    let (_, body) = call(&w.gw, &bridge_credential(&w.gw), "mint_user_token", tenant).await;
    refused(&body, "identity_mismatch", "a forged tenant");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sender_the_conversation_roster_lacks_is_not_in_conversation() {
    let w = verified_world().await;
    let token = w.bf.token(BOT_APP);
    w.bf.remove_from(PERSONAL_CHAT, "29:bob");
    for (user, oid, why) in [
        ("29:bob", BOB_OID, "taken off this conversation"),
        ("29:mallory", ALICE_OID, "nobody Teams knows"),
    ] {
        let (_, body) = call(
            &w.gw,
            &bridge_credential(&w.gw),
            "mint_user_token",
            verified_turn(&w.bf, user, oid, Some(&token)),
        )
        .await;
        refused(&body, "not_in_conversation", why);
        assert_eq!(body["error"]["data"]["retryable"], false, "{why}");
    }
    // In a group chat too: the roster check holds for every conversation kind.
    map_group_chat(&w.gw, "sales").await;
    w.bf.remove_from(GROUP_CHAT, "29:alice");
    let mut group = verified_turn(&w.bf, "29:alice", ALICE_OID, Some(&token));
    group["conversation"] = json!({ "kind": "group", "id": GROUP_CHAT });
    let (_, body) = call(&w.gw, &bridge_credential(&w.gw), "mint_user_token", group).await;
    refused(&body, "not_in_conversation", "a group chat");
    // 403 is a refusal like 404; a 5xx is an outage, retryable.
    for (status, code, retryable) in [
        (403u16, "not_in_conversation", false),
        (503, "directory_unavailable", true),
    ] {
        w.bf.seen.lock().unwrap().member_status = Some(status);
        let (_, body) = call(
            &w.gw,
            &bridge_credential(&w.gw),
            "mint_user_token",
            verified_turn(&w.bf, "29:bob", BOB_OID, Some(&token)),
        )
        .await;
        refused(&body, code, &status.to_string());
        assert_eq!(body["error"]["data"]["retryable"], retryable, "{status}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_missing_expired_or_foreign_activity_token_is_unverified() {
    let w = verified_world().await;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let claims_with = |patch: Value| {
        let mut c = json!({ "iss": BF_ISSUER, "aud": BOT_APP, "serviceurl": w.bf.base,
                            "nbf": now - 5, "iat": now - 5, "exp": now + 600 });
        for (k, v) in patch.as_object().unwrap() {
            c[k] = v.clone();
        }
        c
    };
    let cases = [
        (None, "no token"),
        (
            Some(w.bf.sign(
                &claims_with(json!({ "exp": now - 3600, "nbf": now - 7200 })),
                TEAMS_KID,
            )),
            "expired",
        ),
        (Some(w.bf.token("another-bot")), "another bot's audience"),
        (
            Some(w.bf.sign(
                &claims_with(json!({ "iss": "https://evil.example" })),
                TEAMS_KID,
            )),
            "another issuer",
        ),
        (
            Some(w.bf.token_with_kid(BOT_APP, WEBCHAT_KID)),
            "a key not endorsed for Teams",
        ),
        (Some("not-a-jwt".to_owned()), "garbage"),
        (
            Some(unsigned(
                json!({ "alg": "HS256", "typ": "JWT", "kid": TEAMS_KID }),
                &claims_with(json!({})),
            )),
            "HS256 alg confusion",
        ),
        (
            Some(unsigned(
                json!({ "alg": "none", "typ": "JWT", "kid": TEAMS_KID }),
                &claims_with(json!({})),
            )),
            "alg none",
        ),
        (
            Some(unsigned(
                json!({ "alg": "RS256", "typ": "JWT" }),
                &claims_with(json!({})),
            )),
            "no kid",
        ),
        (
            Some(w.bf.sign(&claims_with(json!({})), "a-kid-nobody-publishes")),
            "an unknown kid",
        ),
        (
            Some(w.bf.sign(&claims_with(json!({ "nbf": now + 3600 })), TEAMS_KID)),
            "nbf beyond the leeway",
        ),
        (
            Some(w.bf.sign(
                &{
                    let mut c = claims_with(json!({}));
                    c.as_object_mut().unwrap().remove("serviceurl");
                    c
                },
                TEAMS_KID,
            )),
            "no serviceurl claim",
        ),
    ];
    for (token, why) in cases {
        let (_, body) = call(
            &w.gw,
            &bridge_credential(&w.gw),
            "mint_user_token",
            verified_turn(&w.bf, "29:alice", ALICE_OID, token.as_deref()),
        )
        .await;
        refused(&body, "activity_unverified", why);
        assert_eq!(body["error"]["data"]["retryable"], false, "{why}");
    }
    assert_eq!(
        member_calls(&w.bf),
        0,
        "nothing unverified reaches the roster"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_service_url_the_token_does_not_bind_or_the_allow_list_lacks_gets_no_bearer() {
    let w = verified_world().await;
    // The same stand-in under a name the allow-list (127.0.0.1) does not hold.
    let elsewhere = w.bf.base.replace("127.0.0.1", "localhost");
    // The token is bound to the real serviceUrl; the bridge names another.
    let mut moved = verified_turn(&w.bf, "29:alice", ALICE_OID, Some(&w.bf.token(BOT_APP)));
    moved["service_url"] = json!(elsewhere);
    let (_, body) = call(&w.gw, &bridge_credential(&w.gw), "mint_user_token", moved).await;
    refused(
        &body,
        "activity_unverified",
        "a serviceUrl the token does not bind",
    );
    // A genuine token bound to a host the gateway does not send its bearer to.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let foreign = w.bf.sign(
        &json!({ "iss": BF_ISSUER, "aud": BOT_APP, "serviceurl": elsewhere,
                 "nbf": now - 5, "iat": now - 5, "exp": now + 600 }),
        TEAMS_KID,
    );
    let mut unlisted = verified_turn(&w.bf, "29:alice", ALICE_OID, Some(&foreign));
    unlisted["service_url"] = json!(elsewhere);
    let (_, body) = call(
        &w.gw,
        &bridge_credential(&w.gw),
        "mint_user_token",
        unlisted,
    )
    .await;
    refused(&body, "activity_unverified", "a host off the allow-list");
    let seen = w.bf.seen.lock().unwrap();
    assert!(
        seen.member_authorizations.is_empty() && seen.token_requests.is_empty(),
        "the stand-in saw no bearer and no token request: {:?}",
        seen.member_authorizations
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unverifying_gateway_trusts_the_bridge_as_before() {
    let w = world().await;
    // No activity token, no service URL: today's contract.
    let minted = mint(&w.gw, personal(ALICE_OID)).await;
    assert_eq!(minted["subject"], ALICE);
}

// ---- #835: the mint rate limit ----------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mints_are_capped_per_bridge_credential_and_per_asker() {
    let graph = DirectoryStandIn::graph().await;
    for oid in [ALICE_OID, BOB_OID, "carol-oid", "dave-oid"] {
        graph.member_of(oid, &["escurel-sales"]);
    }
    let mut cfg = bridge_mint("user:{aadObjectId}", vec![graph.config("escurel-")], None);
    // A burst of 2 per credential (one more every 30 s) and 1 per asker (one every 60 s): every
    // refusal below follows its bucket's last token by milliseconds, so a refill cannot land in
    // between on a slow runner (crew F14).
    cfg.rate = escurel_server::rate_limit::MintRateLimits {
        per_credential_per_minute: 2,
        per_asker_per_minute: 1,
    };
    let gw = gateway(cfg).await;
    let one = bridge_credential(&gw);
    let other = gw.mint_token_with_groups(TENANT, "svc:chat-bridge-2", &["escurel:bridge"], false);
    let mint_as = |cred: String, oid: &'static str| {
        let gw = &gw;
        async move { call(gw, &cred, "mint_user_token", personal(oid)).await.1 }
    };
    ok(&mint_as(one.clone(), ALICE_OID).await, "alice's first");
    ok(
        &mint_as(one.clone(), BOB_OID).await,
        "bob on the first bridge",
    );
    let body = mint_as(one.clone(), "carol-oid").await;
    refused(&body, "rate_limited", "the per-credential cap");
    assert_eq!(body["error"]["data"]["scope"], "credential", "{body}");
    assert_eq!(body["error"]["data"]["retryable"], true);
    assert!(
        body["error"]["data"]["retry_after_secs"].as_u64().unwrap() >= 1,
        "{body}"
    );
    // Alice again, on another bridge: her own bucket is empty, whichever bridge asks.
    let body = mint_as(other.clone(), ALICE_OID).await;
    refused(&body, "rate_limited", "the per-asker cap");
    assert_eq!(body["error"]["data"]["scope"], "asker", "{body}");
    // The other credential spent one token on that refusal and is not held back by the first.
    ok(&mint_as(other, "dave-oid").await, "dave on another bridge");
}

/// Crew F1: Microsoft's signing keys out of reach is an outage, not a forged
/// token — retryable, and the next mint after recovery succeeds.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_signing_key_outage_is_retryable_and_recovers() {
    let w = verified_world().await;
    w.bf.seen.lock().unwrap().jwks_fail = 0..1;
    let token = w.bf.token(BOT_APP);
    let (_, body) = call(
        &w.gw,
        &bridge_credential(&w.gw),
        "mint_user_token",
        verified_turn(&w.bf, "29:alice", ALICE_OID, Some(&token)),
    )
    .await;
    refused(&body, "directory_unavailable", "the JWKS answered 503");
    assert_eq!(body["error"]["data"]["retryable"], true, "{body}");
    let minted = mint(
        &w.gw,
        verified_turn(&w.bf, "29:alice", ALICE_OID, Some(&token)),
    )
    .await;
    assert_eq!(minted["subject"], ALICE);
}

/// Crew F4: the tenant is Microsoft's, else the one Graph serves, never the
/// bridge's; a roster entry without an object id is nobody.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_roster_answer_without_a_tenant_or_object_id_never_takes_the_bridges() {
    use escurel_test_support::RosterMember;
    let w = verified_world().await;
    let tenantless = |aad: Option<&str>| RosterMember {
        aad_object_id: aad.map(str::to_owned),
        email: ALICE_EMAIL.to_owned(),
        tenant_id: None,
    };
    let token = w.bf.token(BOT_APP);
    // With Graph, its tenant stands in.
    w.bf.add_roster_member("29:alice", tenantless(Some(ALICE_OID)));
    let minted = mint(
        &w.gw,
        verified_turn(&w.bf, "29:alice", ALICE_OID, Some(&token)),
    )
    .await;
    assert_eq!(minted["subject"], ALICE);
    // No object id: no principal.
    w.bf.add_roster_member("29:guest", tenantless(None));
    let (_, body) = call(
        &w.gw,
        &bridge_credential(&w.gw),
        "mint_user_token",
        verified_turn(&w.bf, "29:guest", ALICE_OID, Some(&token)),
    )
    .await;
    refused(&body, "no_principal", "no aadObjectId on the roster");

    // Without Graph there is nothing to stand in: refused, whatever the bridge says.
    let bf = BotFrameworkStandIn::start().await;
    bf.add_roster_member("29:alice", tenantless(Some(ALICE_OID)));
    let mut cfg = bridge_mint("user:{aadObjectId}", vec![], None);
    cfg.teams = Some(bf.gateway_verification(BOT_APP));
    let gw = gateway(cfg).await;
    let token = bf.token(BOT_APP);
    let (_, body) = call(
        &gw,
        &bridge_credential(&gw),
        "mint_user_token",
        verified_turn(&bf, "29:alice", ALICE_OID, Some(&token)),
    )
    .await;
    refused(&body, "identity_mismatch", "no tenant from Teams, no Graph");
}

/// Crew F6: the connector refusing the GATEWAY's own bot credential is a
/// misconfiguration an admin fixes, not "try again later" — while a 403 or
/// 404 about the user stays `not_in_conversation`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refused_bot_credential_is_teams_misconfigured() {
    let w = verified_world().await;
    let token = w.bf.token(BOT_APP);
    let turn = || verified_turn(&w.bf, "29:alice", ALICE_OID, Some(&token));
    // The token endpoint refuses the app secret.
    w.bf.seen.lock().unwrap().token_status = Some(401);
    let (_, body) = call(&w.gw, &bridge_credential(&w.gw), "mint_user_token", turn()).await;
    refused(&body, "teams_misconfigured", "the token endpoint refused");
    assert_eq!(body["error"]["data"]["retryable"], false);
    // A token, but the connector answers 401 even after a renewal.
    w.bf.seen.lock().unwrap().token_status = None;
    w.bf.seen.lock().unwrap().member_status = Some(401);
    let (_, body) = call(&w.gw, &bridge_credential(&w.gw), "mint_user_token", turn()).await;
    refused(
        &body,
        "teams_misconfigured",
        "the connector refused the bearer",
    );
    w.bf.seen.lock().unwrap().member_status = Some(403);
    let (_, body) = call(&w.gw, &bridge_credential(&w.gw), "mint_user_token", turn()).await;
    refused(&body, "not_in_conversation", "a 403 about the user");
}

/// Crew F5, pinned: the Bot Framework signs no activity body, so a token binds
/// the bot and the connector, not the conversation. One accepted token
/// therefore verifies a mint for ANOTHER conversation the sender is in; the
/// roster check is what stops anyone who is in none.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_activity_token_does_not_bind_its_conversation() {
    let w = verified_world().await;
    let token = w.bf.token(BOT_APP);
    mint(
        &w.gw,
        verified_turn(&w.bf, "29:alice", ALICE_OID, Some(&token)),
    )
    .await;
    let mut elsewhere = verified_turn(&w.bf, "29:bob", BOB_OID, Some(&token));
    elsewhere["conversation"]["id"] = json!("a:1another-personal-chat");
    let minted = mint(&w.gw, elsewhere).await;
    assert_eq!(
        minted["subject"], "user:bob-oid",
        "the same token, another chat and person"
    );
}

/// A JWT with `header` and `claims` and a signature that is no signature: for
/// the cases refused before any key is consulted.
fn unsigned(header: Value, claims: &Value) -> String {
    let b64 = |v: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v);
    format!(
        "{}.{}.{}",
        b64(header.to_string().as_bytes()),
        b64(claims.to_string().as_bytes()),
        b64(b"not-a-signature")
    )
}

/// Crew F9: the roster cache is keyed by the conversation too — Alice's 1:1
/// answer must not vouch for a group chat she has left.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cached_roster_answer_never_vouches_for_another_conversation() {
    let w = verified_world().await;
    let token = w.bf.token(BOT_APP);
    mint(
        &w.gw,
        verified_turn(&w.bf, "29:alice", ALICE_OID, Some(&token)),
    )
    .await;
    map_group_chat(&w.gw, "sales").await;
    w.bf.remove_from(GROUP_CHAT, "29:alice");
    let mut group = verified_turn(&w.bf, "29:alice", ALICE_OID, Some(&token));
    group["conversation"] = json!({ "kind": "group", "id": GROUP_CHAT });
    let (_, body) = call(&w.gw, &bridge_credential(&w.gw), "mint_user_token", group).await;
    refused(&body, "not_in_conversation", "a group chat she is not in");
    assert_eq!(
        member_calls(&w.bf),
        2,
        "the group chat was asked about, not cached"
    );
}

/// Crew F12: with verification on, a mapped group chat labelled `personal`
/// (which would skip the mapping and the membership check) is refused, as is
/// a channel turn that names no team.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_conversation_kind_its_teams_id_contradicts_is_refused() {
    let w = verified_world().await;
    let token = w.bf.token(BOT_APP);
    for (conversation, why) in [
        (
            json!({ "kind": "personal", "id": GROUP_CHAT }),
            "a group chat as personal",
        ),
        (
            json!({ "kind": "channel", "id": "19:c@thread.tacv2;messageid=1" }),
            "a channel with no team",
        ),
    ] {
        let mut turn = verified_turn(&w.bf, "29:alice", ALICE_OID, Some(&token));
        turn["conversation"] = conversation;
        let (_, body) = call(&w.gw, &bridge_credential(&w.gw), "mint_user_token", turn).await;
        refused(&body, "identity_mismatch", why);
    }
    assert_eq!(member_calls(&w.bf), 0, "refused before the roster is asked");
}
