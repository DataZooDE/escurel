//! Chat-surface ACL over real HTTP: a chat group (ADR-13: `chat_group_id`
//! := `community_member_id`) is private to its owning member. Only the owner
//! (or admin) may append to or read a chat's history; a non-owner read
//! returns an EMPTY page (non-leaking) and a non-owner append is forbidden.
//! Gated by `ESCUREL_WRITE_ACL` (off = legacy open chat). No LLM in the path.

use escurel_server::WriteAclMode;
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const TENANT: &str = "stuttgart-ai";
const ALICE: &str = "whatsapp:111";
const BOB: &str = "whatsapp:222";

const MEMBER_SKILL: &str = "---\nkind: skill\nid: community_member\n\
    description: A member.\nvisibility: owner\nowner_field: credential\n---\n# community_member\n";
const ALICE_MEMBER: &str = "---\nkind: instance\nskill: community_member\nid: alice\n\
    credential: \"whatsapp:111\"\n---\n# Alice\n";
const BOB_MEMBER: &str = "---\nkind: instance\nskill: community_member\nid: bob\n\
    credential: \"whatsapp:222\"\n---\n# Bob\n";

// Chat group id == community_member id (ADR-13).
const ALICE_CHAT: &str = "alice";

async fn start(mode: WriteAclMode) -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            write_acl: Some(mode),
            ..Default::default()
        },
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("community_member", MEMBER_SKILL)
                .instance("community_member", "alice", ALICE_MEMBER)
                .instance("community_member", "bob", BOB_MEMBER)
                .done(),
        ),
    })
    .await
}

/// Raw JSON-RPC call → the whole response body (so callers can inspect
/// `result` vs `error`).
async fn call(p: &EscurelProcess, token: &str, name: &str, args: Value) -> Value {
    let resp = reqwest::Client::new()
        .post(p.mcp_url())
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": args },
        }))
        .send()
        .await
        .expect("post");
    assert_eq!(resp.status(), 200, "http status");
    resp.json().await.unwrap()
}

fn append_args(chat: &str, content: &str) -> Value {
    json!({ "chat_group_id": chat, "role": "user", "content": content, "embed": false })
}

#[tokio::test]
async fn owner_appends_and_reads_non_owner_blocked() {
    let p = start(WriteAclMode::Enforce).await;
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let bob = p.mint_token_with_sub(TENANT, Role::Agent, BOB);
    let admin = p.mint_token(TENANT, Role::Admin);

    // Owner appends to her own chat → ok.
    let a = call(
        &p,
        &alice,
        "append_message",
        append_args(ALICE_CHAT, "geheim"),
    )
    .await;
    assert!(
        a.get("error").is_none(),
        "alice may append to her own chat: {a}"
    );

    // Non-owner append → forbidden error.
    let b = call(&p, &bob, "append_message", append_args(ALICE_CHAT, "spy")).await;
    assert!(
        b["error"]["code"] == json!(-32003),
        "bob's append to alice's chat must be forbidden: {b}"
    );

    // Non-owner read → empty page (non-leaking), never alice's message.
    let bread = call(
        &p,
        &bob,
        "list_messages",
        json!({ "chat_group_id": ALICE_CHAT }),
    )
    .await;
    let msgs = &bread["result"]["structuredContent"]["messages"];
    assert_eq!(msgs, &json!([]), "bob must see an empty page, got: {bread}");

    // Owner reads her own → sees the message.
    let aread = call(
        &p,
        &alice,
        "list_messages",
        json!({ "chat_group_id": ALICE_CHAT }),
    )
    .await;
    let amsgs = aread["result"]["structuredContent"]["messages"]
        .as_array()
        .unwrap();
    assert_eq!(amsgs.len(), 1, "alice reads her own chat: {aread}");

    // Admin reads any chat.
    let adread = call(
        &p,
        &admin,
        "list_messages",
        json!({ "chat_group_id": ALICE_CHAT }),
    )
    .await;
    let admsgs = adread["result"]["structuredContent"]["messages"]
        .as_array()
        .unwrap();
    assert_eq!(admsgs.len(), 1, "admin reads any chat: {adread}");
}

#[tokio::test]
async fn off_mode_leaves_chat_open() {
    let p = start(WriteAclMode::Off).await;
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let bob = p.mint_token_with_sub(TENANT, Role::Agent, BOB);

    let a = call(
        &p,
        &alice,
        "append_message",
        append_args(ALICE_CHAT, "geheim"),
    )
    .await;
    assert!(a.get("error").is_none(), "{a}");
    // With the flag off, bob can still read alice's chat (legacy behaviour).
    let bread = call(
        &p,
        &bob,
        "list_messages",
        json!({ "chat_group_id": ALICE_CHAT }),
    )
    .await;
    let msgs = bread["result"]["structuredContent"]["messages"]
        .as_array()
        .unwrap();
    assert_eq!(msgs.len(), 1, "off mode keeps chat open: {bread}");
}

#[tokio::test]
async fn unknown_chat_group_is_ungated() {
    // A chat_group_id with no owning community_member is not private — any
    // authenticated caller may use it (compat for non-member chat groups).
    let p = start(WriteAclMode::Enforce).await;
    let bob = p.mint_token_with_sub(TENANT, Role::Agent, BOB);
    let a = call(
        &p,
        &bob,
        "append_message",
        append_args("ops-broadcast", "hi"),
    )
    .await;
    assert!(
        a.get("error").is_none(),
        "unknown chat group is ungated: {a}"
    );
}

const NOTE_PAGE: &str = "markdown/instances/note/n-001.md";
const NOTE_SKILL: &str = "---\nkind: skill\nid: note\ndescription: Note.\nvisibility: owner\n\
    owner_field: owner\nchat:\n  page_bound: true\n---\n# Note\n";
const NOTE_RECORD: &str =
    "---\nkind: instance\nskill: note\nid: n-001\nowner: \"whatsapp:111\"\n---\n# Note\n";
const TICKET_SKILL: &str = "---\nkind: skill\nid: ticket\ndescription: Ticket.\nvisibility: owner\n\
    owner_field: owner\n---\n# Ticket\n";
const TICKET_RECORD: &str =
    "---\nkind: instance\nskill: ticket\nid: t-001\nowner: \"whatsapp:111\"\n---\n# Ticket\n";

fn page_chat(subject: &str, page_id: &str) -> String {
    format!(
        "page-chat:v1:{:x}:{page_id}",
        Sha256::digest(subject.as_bytes())
    )
}

#[tokio::test]
async fn page_bound_chat_is_private_and_bound_to_a_readable_page() {
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            write_acl: Some(WriteAclMode::Enforce),
            ..Default::default()
        },
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("note", NOTE_SKILL)
                .skill("ticket", TICKET_SKILL)
                .instance("note", "n-001", NOTE_RECORD)
                .instance("ticket", "t-001", TICKET_RECORD)
                .done(),
        ),
    })
    .await;
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let bob = p.mint_token_with_sub(TENANT, Role::Agent, BOB);
    let admin = p.mint_token(TENANT, Role::Admin);
    let group = page_chat(ALICE, NOTE_PAGE);
    let args = |g: &str, content: &str| json!({"chat_group_id": g, "role": "user", "content": content, "embed": false});

    // The owner of the page and of the hash may append and read.
    let ok = call(&p, &alice, "append_message", args(&group, "first")).await;
    assert!(ok.get("error").is_none(), "owner appends: {ok}");
    let read = call(&p, &alice, "list_messages", json!({"chat_group_id": group})).await;
    assert_eq!(
        read["result"]["structuredContent"]["messages"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // Another caller: append forbidden, read empty, even when naming their own hash.
    let spy = call(&p, &bob, "append_message", args(&group, "spy")).await;
    assert_eq!(spy["error"]["code"], -32003);
    let spy_read = call(&p, &bob, "list_messages", json!({"chat_group_id": group})).await;
    assert_eq!(
        spy_read["result"]["structuredContent"]["messages"],
        json!([])
    );
    let own_hash = call(
        &p,
        &bob,
        "append_message",
        args(&page_chat(BOB, NOTE_PAGE), "x"),
    )
    .await;
    assert_eq!(
        own_hash["error"]["code"], -32003,
        "bob cannot read the page: {own_hash}"
    );

    // Admin reads any page chat.
    let adm = call(&p, &admin, "list_messages", json!({"chat_group_id": group})).await;
    assert_eq!(
        adm["result"]["structuredContent"]["messages"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // A page that does not exist, a skill that is not page-bound, and a
    // malformed id are all refused.
    for bad in [
        page_chat(ALICE, "markdown/instances/note/absent.md"),
        page_chat(ALICE, "markdown/instances/ticket/t-001.md"),
        format!(
            "page-chat:v2:{:x}:{NOTE_PAGE}",
            Sha256::digest(ALICE.as_bytes())
        ),
    ] {
        let r = call(&p, &alice, "append_message", args(&bad, "orphan")).await;
        assert_eq!(r["error"]["code"], -32003, "{bad}: {r}");
    }
}

// --- page-bound chat: run replies, revision guard, server-stamped author ---

const NOTE_PAGE_2: &str = "markdown/instances/note/n-002.md";
const NOTE_RECORD_2: &str =
    "---\nkind: instance\nskill: note\nid: n-002\nowner: \"whatsapp:111\"\n---\n# Note 2\n";
const BOB_NOTE_PAGE: &str = "markdown/instances/note/n-003.md";
const BOB_NOTE_RECORD: &str =
    "---\nkind: instance\nskill: note\nid: n-003\nowner: \"whatsapp:222\"\n---\n# Note 3\n";

async fn start_page_chat(signing: bool) -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        config_overrides: ConfigOverrides {
            write_acl: Some(WriteAclMode::Enforce),
            signing,
            ..Default::default()
        },
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("note", NOTE_SKILL)
                .instance("note", "n-001", NOTE_RECORD)
                .instance("note", "n-002", NOTE_RECORD_2)
                .instance("note", "n-003", BOB_NOTE_RECORD)
                .done(),
        ),
    })
    .await
}

/// A person asks for a reply: an event whose provenance names the page chat.
async fn capture_request(p: &EscurelProcess, token: &str, group: &str) -> String {
    let r = call(
        p,
        token,
        "capture_event",
        json!({
            "label_skill": "note",
            "source": "page-chat",
            "title": "reply request",
            "body": "please answer",
            "provenance": { "chat_group_id": group },
        }),
    )
    .await;
    assert!(r.get("error").is_none(), "capture: {r}");
    r["result"]["structuredContent"]["event_id"]
        .as_str()
        .expect("event_id")
        .to_owned()
}

fn reply(group: &str, role: &str, content: &str) -> Value {
    json!({"chat_group_id": group, "role": role, "content": content, "embed": false})
}

async fn messages(p: &EscurelProcess, token: &str, group: &str) -> Vec<Value> {
    let r = call(
        p,
        token,
        "list_messages",
        json!({"chat_group_id": group, "direction": "asc"}),
    )
    .await;
    r["result"]["structuredContent"]["messages"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

fn sha(s: &str) -> String {
    format!("{:x}", Sha256::digest(s.as_bytes()))
}

#[tokio::test]
async fn a_run_token_replies_only_into_the_page_chat_its_root_event_names() {
    let p = start_page_chat(false).await;
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let bob = p.mint_token_with_sub(TENANT, Role::Agent, BOB);
    let group = page_chat(ALICE, NOTE_PAGE);
    let other_group = page_chat(ALICE, NOTE_PAGE_2);
    let event = capture_request(&p, &alice, &group).await;
    let other_event = capture_request(&p, &alice, &other_group).await;
    // An event captured by bob that names alice's chat must not open it.
    let bobs_event = capture_request(&p, &bob, &group).await;

    // The runner's per-run token (admin authority, as in production).
    let run = p.mint_token_for_run(TENANT, Role::Admin, "agent:note-reply", "RUN-1", &event);
    let ok = call(
        &p,
        &run,
        "append_message",
        reply(&group, "assistant", "answer"),
    )
    .await;
    assert!(ok.get("error").is_none(), "run reply: {ok}");

    // Only `assistant`, only that group: the admin authority is not a bypass.
    let as_user = call(&p, &run, "append_message", reply(&group, "user", "x")).await;
    assert_eq!(as_user["error"]["code"], -32003, "{as_user}");
    let elsewhere = call(
        &p,
        &run,
        "append_message",
        reply(&other_group, "assistant", "x"),
    )
    .await;
    assert_eq!(elsewhere["error"]["code"], -32003, "{elsewhere}");
    let bobs_chat = page_chat(BOB, BOB_NOTE_PAGE);
    let foreign = call(
        &p,
        &run,
        "append_message",
        reply(&bobs_chat, "assistant", "x"),
    )
    .await;
    assert_eq!(foreign["error"]["code"], -32003, "{foreign}");

    // A run of another event cannot write here either.
    let run2 = p.mint_token_for_run(
        TENANT,
        Role::Admin,
        "agent:note-reply",
        "RUN-2",
        &other_event,
    );
    let wrong = call(&p, &run2, "append_message", reply(&group, "assistant", "x")).await;
    assert_eq!(wrong["error"]["code"], -32003, "{wrong}");
    // A run whose root event was captured by someone other than the chat's owner.
    let run3 = p.mint_token_for_run(
        TENANT,
        Role::Admin,
        "agent:note-reply",
        "RUN-3",
        &bobs_event,
    );
    let forged = call(&p, &run3, "append_message", reply(&group, "assistant", "x")).await;
    assert_eq!(forged["error"]["code"], -32003, "{forged}");
    // A run whose root event does not exist.
    let run4 = p.mint_token_for_run(
        TENANT,
        Role::Admin,
        "agent:note-reply",
        "RUN-4",
        "no-such-event",
    );
    let ghost = call(&p, &run4, "append_message", reply(&group, "assistant", "x")).await;
    assert_eq!(ghost["error"]["code"], -32003, "{ghost}");

    // The owner sees exactly the one reply, stamped with the run's lineage.
    let msgs = messages(&p, &alice, &group).await;
    assert_eq!(msgs.len(), 1, "{msgs:?}");
    assert_eq!(msgs[0]["role"], "assistant");
    assert_eq!(msgs[0]["author"], "agent:note-reply", "{msgs:?}");
    assert_eq!(msgs[0]["metadata"]["run_id"], "RUN-1", "{msgs:?}");
    assert_eq!(msgs[0]["metadata"]["event_id"], json!(event), "{msgs:?}");
    assert_eq!(msgs[0]["metadata"]["actor"], "escurel-runner", "{msgs:?}");
}

#[tokio::test]
async fn a_gateway_minted_run_replies_for_the_person_who_asked_and_no_one_else() {
    let p = start_page_chat(true).await;
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let bob = p.mint_token_with_sub(TENANT, Role::Agent, BOB);
    let group = page_chat(ALICE, NOTE_PAGE);
    let event = capture_request(&p, &alice, &group).await;

    let minted = call(
        &p,
        &alice,
        "mint_agent_token",
        json!({"skill": "note-reply", "root_event_id": event}),
    )
    .await;
    let token = minted["result"]["structuredContent"]["token"]
        .as_str()
        .expect("token")
        .to_owned();
    let run_id = minted["result"]["structuredContent"]["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let ok = call(
        &p,
        &token,
        "append_message",
        reply(&group, "assistant", "answer"),
    )
    .await;
    assert!(ok.get("error").is_none(), "gateway run reply: {ok}");
    // The run speaks as the assistant, never as the person.
    let as_user = call(&p, &token, "append_message", reply(&group, "user", "x")).await;
    assert_eq!(as_user["error"]["code"], -32003, "{as_user}");
    let other = call(
        &p,
        &token,
        "append_message",
        reply(&page_chat(ALICE, NOTE_PAGE_2), "assistant", "x"),
    )
    .await;
    assert_eq!(other["error"]["code"], -32003, "{other}");

    // Bob minting a run on alice's event gains nothing.
    let bobs = call(
        &p,
        &bob,
        "mint_agent_token",
        json!({"skill": "note-reply", "root_event_id": event}),
    )
    .await;
    let bobs_token = bobs["result"]["structuredContent"]["token"]
        .as_str()
        .expect("token")
        .to_owned();
    let spy = call(
        &p,
        &bobs_token,
        "append_message",
        reply(&group, "assistant", "x"),
    )
    .await;
    assert_eq!(spy["error"]["code"], -32003, "{spy}");

    let msgs = messages(&p, &alice, &group).await;
    assert_eq!(msgs.len(), 1, "{msgs:?}");
    // The verified subject of a gateway-minted token is the person; the agent is the actor.
    assert_eq!(msgs[0]["author"], ALICE, "{msgs:?}");
    assert_eq!(msgs[0]["metadata"]["run_id"], json!(run_id), "{msgs:?}");
    assert_eq!(msgs[0]["metadata"]["event_id"], json!(event), "{msgs:?}");
    assert_eq!(msgs[0]["metadata"]["actor"], "agent:note-reply", "{msgs:?}");
}

#[tokio::test]
async fn a_page_chat_append_bound_to_a_stale_revision_is_refused() {
    let p = start_page_chat(false).await;
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let admin = p.mint_token(TENANT, Role::Admin);
    let group = page_chat(ALICE, NOTE_PAGE);
    let seen = sha(NOTE_RECORD);

    let mut args = reply(&group, "user", "about this version");
    args["page_revision_sha256"] = json!(seen);
    let ok = call(&p, &alice, "append_message", args.clone()).await;
    assert!(ok.get("error").is_none(), "current revision: {ok}");

    let edited = NOTE_RECORD.replace("# Note", "# Note, edited");
    let upd = call(
        &p,
        &admin,
        "update_page",
        json!({"page_id": NOTE_PAGE, "content": edited}),
    )
    .await;
    assert!(upd.get("error").is_none(), "update: {upd}");

    args["content"] = json!("about the old version");
    let stale = call(&p, &alice, "append_message", args).await;
    assert_eq!(
        stale["error"]["data"]["code"], "revision_changed",
        "{stale}"
    );
    assert_eq!(messages(&p, &alice, &group).await.len(), 1);

    // A revision guard on a group that is not page-bound is a caller error.
    let mut plain = reply("ops-broadcast", "user", "x");
    plain["page_revision_sha256"] = json!(seen);
    let bad = call(&p, &alice, "append_message", plain).await;
    assert_eq!(bad["error"]["code"], -32602, "{bad}");
}

#[tokio::test]
async fn the_server_stamps_the_author_of_a_page_chat_message() {
    let p = start_page_chat(false).await;
    let alice = p.mint_token_with_sub(TENANT, Role::Agent, ALICE);
    let group = page_chat(ALICE, NOTE_PAGE);
    let mut args = reply(&group, "user", "hello");
    args["author"] = json!("someone-else");
    args["metadata"] = json!({"client": "kept", "run_id": "forged"});
    let ok = call(&p, &alice, "append_message", args).await;
    assert!(ok.get("error").is_none(), "{ok}");
    let msgs = messages(&p, &alice, &group).await;
    assert_eq!(msgs[0]["author"], ALICE, "{msgs:?}");
    assert_eq!(msgs[0]["metadata"]["client"], "kept", "{msgs:?}");
    assert!(
        msgs[0]["metadata"].get("run_id").is_none(),
        "a person's message carries no run lineage: {msgs:?}"
    );

    // Other chat groups keep the caller-supplied author.
    let mut plain = reply("ops-broadcast", "user", "x");
    plain["author"] = json!("display-name");
    call(&p, &alice, "append_message", plain).await;
    let open = messages(&p, &alice, "ops-broadcast").await;
    assert_eq!(open[0]["author"], "display-name", "{open:?}");
}
