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
    AuthMode, ConfigOverrides, DirectoryStandIn, EscurelProcess, FixtureBuilder, GRAPH_TENANT,
    Opts, Role, WriteAclMode, bridge_mint,
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
