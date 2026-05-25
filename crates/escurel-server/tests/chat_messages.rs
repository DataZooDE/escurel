//! End-to-end tests for the `append_message` and `list_messages`
//! chat-history agent tools (DataZooDE/escurel#63 — M-Chat).
//!
//! Real running gateway, real Indexer (DuckDB + FsStore +
//! ZeroEmbedder), exercised over both transports: MCP-over-HTTP
//! (raw JSON-RPC via reqwest) and native gRPC (tonic
//! `EscurelClient`). The chat surface sits next to the typed
//! `pages`/`blocks` knowledge base; these tests verify that an
//! agent can append and read back conversation history without
//! touching the page write path.

use std::sync::Arc;

use escurel_proto::v1::escurel_client::EscurelClient;
use escurel_proto::v1::{AppendMessageRequest, ListMessagesRequest};
use escurel_quota::{QuotaConfig, QuotaManager};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use serde_json::{Value, json};
use tonic::Request;
use tonic::metadata::MetadataValue;
use tonic::transport::Channel;

const TENANT: &str = "carl";

async fn start(quota: Option<Arc<QuotaManager>>) -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(FixtureBuilder::new().tenant(TENANT).done()),
        config_overrides: ConfigOverrides {
            quota,
            ..Default::default()
        },
    })
    .await
}

// --- MCP transport helpers ---------------------------------------

async fn call_mcp(p: &EscurelProcess, tenant: &str, role: Role, name: &str, args: Value) -> Value {
    let token = p.mint_token(tenant, role);
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
        .expect("post");
    assert_eq!(resp.status(), 200, "http status");
    let body: Value = resp.json().await.unwrap();
    if body.get("error").is_some() {
        panic!("tool {name} returned error: {body}");
    }
    body["result"].clone()
}

async fn call_mcp_raw(
    p: &EscurelProcess,
    tenant: &str,
    role: Role,
    name: &str,
    args: Value,
) -> reqwest::Response {
    let token = p.mint_token(tenant, role);
    reqwest::Client::new()
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
        .expect("post")
}

// --- gRPC transport helpers --------------------------------------

async fn grpc_client(
    p: &EscurelProcess,
    tenant: &str,
) -> (
    EscurelClient<Channel>,
    MetadataValue<tonic::metadata::Ascii>,
) {
    let endpoint = p.grpc_endpoint().expect("grpc endpoint");
    let channel = Channel::from_shared(endpoint.to_owned())
        .unwrap()
        .connect()
        .await
        .unwrap();
    let t = p.mint_token(tenant, Role::Agent);
    let bearer: MetadataValue<_> = format!("Bearer {t}").parse().unwrap();
    (EscurelClient::new(channel), bearer)
}

fn authed<T>(body: T, bearer: &MetadataValue<tonic::metadata::Ascii>) -> Request<T> {
    let mut r = Request::new(body);
    r.metadata_mut().insert("authorization", bearer.clone());
    r
}

// --- MCP tests ----------------------------------------------------

#[tokio::test]
async fn mcp_append_then_list_returns_time_ordered() {
    let p = start(None).await;

    for (ts, role, body) in [
        ("2026-05-25T10:00:00Z", "user", "first"),
        ("2026-05-25T10:00:05Z", "assistant", "second"),
        ("2026-05-25T10:00:10Z", "user", "third"),
    ] {
        let ack = call_mcp(
            &p,
            TENANT,
            Role::Agent,
            "append_message",
            json!({
                "chat_group_id": "room-1",
                "role": role,
                "content": body,
                "ts": ts,
            }),
        )
        .await;
        assert!(ack["msg_id"].is_string(), "ack carries msg_id: {ack}");
        assert!(ack["ts"].is_string(), "ack carries ts: {ack}");
    }

    let result = call_mcp(
        &p,
        TENANT,
        Role::Agent,
        "list_messages",
        json!({ "chat_group_id": "room-1", "direction": "asc" }),
    )
    .await;
    let msgs = result["messages"].as_array().expect("messages array");
    let bodies: Vec<&str> = msgs.iter().filter_map(|m| m["content"].as_str()).collect();
    assert_eq!(bodies, vec!["first", "second", "third"]);
    assert!(
        result.get("next_cursor").is_none() || result["next_cursor"].is_null(),
        "no cursor when results fit under the limit"
    );

    p.shutdown().await;
}

#[tokio::test]
async fn mcp_list_paginates_with_cursor() {
    let p = start(None).await;
    for i in 0..5 {
        call_mcp(
            &p,
            TENANT,
            Role::Agent,
            "append_message",
            json!({
                "chat_group_id": "room-1",
                "role": "user",
                "content": format!("m{i}"),
                "ts": format!("2026-05-25T10:00:0{i}Z"),
            }),
        )
        .await;
    }

    let p1 = call_mcp(
        &p,
        TENANT,
        Role::Agent,
        "list_messages",
        json!({ "chat_group_id": "room-1", "direction": "asc", "limit": 2 }),
    )
    .await;
    let p1_msgs = p1["messages"].as_array().unwrap();
    assert_eq!(p1_msgs.len(), 2);
    assert_eq!(p1_msgs[0]["content"], "m0");
    let cursor = p1["next_cursor"]
        .as_str()
        .expect("cursor present")
        .to_owned();

    let p2 = call_mcp(
        &p,
        TENANT,
        Role::Agent,
        "list_messages",
        json!({
            "chat_group_id": "room-1",
            "direction": "asc",
            "limit": 2,
            "cursor": cursor,
        }),
    )
    .await;
    let p2_msgs = p2["messages"].as_array().unwrap();
    assert_eq!(p2_msgs.len(), 2);
    assert_eq!(p2_msgs[0]["content"], "m2");
    assert_eq!(p2_msgs[1]["content"], "m3");

    p.shutdown().await;
}

#[tokio::test]
async fn mcp_append_with_embed_false_returns_unembedded_message() {
    let p = start(None).await;

    call_mcp(
        &p,
        TENANT,
        Role::Agent,
        "append_message",
        json!({
            "chat_group_id": "room-1",
            "role": "user",
            "content": "skip embedding",
            "ts": "2026-05-25T10:00:00Z",
            "embed": false,
        }),
    )
    .await;

    let result = call_mcp(
        &p,
        TENANT,
        Role::Agent,
        "list_messages",
        json!({ "chat_group_id": "room-1", "direction": "asc" }),
    )
    .await;
    let msgs = result["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0]["embedded"], false);

    p.shutdown().await;
}

#[tokio::test]
async fn mcp_filters_by_since_and_until() {
    let p = start(None).await;
    for i in 0..5 {
        call_mcp(
            &p,
            TENANT,
            Role::Agent,
            "append_message",
            json!({
                "chat_group_id": "room-1",
                "role": "user",
                "content": format!("m{i}"),
                "ts": format!("2026-05-25T10:00:0{i}Z"),
            }),
        )
        .await;
    }

    let result = call_mcp(
        &p,
        TENANT,
        Role::Agent,
        "list_messages",
        json!({
            "chat_group_id": "room-1",
            "direction": "asc",
            "since": "2026-05-25T10:00:01Z",
            "until": "2026-05-25T10:00:03Z",
        }),
    )
    .await;
    let bodies: Vec<&str> = result["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["content"].as_str())
        .collect();
    assert_eq!(bodies, vec!["m1", "m2"], "since inclusive, until exclusive");

    p.shutdown().await;
}

// Cross-tenant isolation is **not** asserted here. The current
// gateway holds a single `Indexer` per `AppState` (`server.rs:52`),
// so all tenants share one DuckDB at the indexer layer. Tenancy
// is enforced today only at the quota + audit boundary; the
// per-tenant indexer routing the spec describes (`docs/spec/
// platform.md §Tenancy`) is a separate workstream. The chat
// tools inherit the gateway's current isolation model — when
// per-tenant indexers land, a cross-tenant test belongs in a
// gateway-level integration file, not here.

#[tokio::test]
async fn mcp_chat_group_isolation_separates_history() {
    // What we *can* verify today: distinct `chat_group_id`s within
    // the same tenant do not cross-pollute. This is the SQL-level
    // filter on `chat_messages.chat_group_id`.
    let p = start(None).await;

    call_mcp(
        &p,
        TENANT,
        Role::Agent,
        "append_message",
        json!({
            "chat_group_id": "room-1",
            "role": "user",
            "content": "msg in room-1",
            "ts": "2026-05-25T10:00:00Z",
        }),
    )
    .await;
    call_mcp(
        &p,
        TENANT,
        Role::Agent,
        "append_message",
        json!({
            "chat_group_id": "room-2",
            "role": "user",
            "content": "msg in room-2",
            "ts": "2026-05-25T10:00:01Z",
        }),
    )
    .await;

    let result = call_mcp(
        &p,
        TENANT,
        Role::Agent,
        "list_messages",
        json!({ "chat_group_id": "room-1", "direction": "asc" }),
    )
    .await;
    let msgs = result["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0]["content"], "msg in room-1");

    p.shutdown().await;
}

#[tokio::test]
async fn mcp_append_debits_writes_dimension() {
    let q = QuotaConfig {
        queries_per_minute: 60,
        writes_per_minute: 1,
        embeds_per_minute: 60,
        concurrent_sessions: 32,
    };
    let p = start(Some(Arc::new(QuotaManager::new(q)))).await;

    // First append passes.
    call_mcp(
        &p,
        TENANT,
        Role::Agent,
        "append_message",
        json!({
            "chat_group_id": "room-1",
            "role": "user",
            "content": "one",
            "ts": "2026-05-25T10:00:00Z",
        }),
    )
    .await;

    // Second append is rejected with the JSON-RPC quota error envelope.
    let raw = call_mcp_raw(
        &p,
        TENANT,
        Role::Agent,
        "append_message",
        json!({
            "chat_group_id": "room-1",
            "role": "user",
            "content": "two",
            "ts": "2026-05-25T10:00:01Z",
        }),
    )
    .await;
    let body: Value = raw.json().await.unwrap();
    assert!(
        body.get("error").is_some(),
        "expected quota error envelope; got: {body}",
    );

    // A read should still pass — Queries bucket is independent.
    call_mcp(
        &p,
        TENANT,
        Role::Agent,
        "list_messages",
        json!({ "chat_group_id": "room-1", "direction": "asc" }),
    )
    .await;

    p.shutdown().await;
}

#[tokio::test]
async fn mcp_missing_auth_returns_401() {
    let p = start(None).await;
    let resp = reqwest::Client::new()
        .post(p.mcp_url())
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "append_message",
                "arguments": {
                    "chat_group_id": "room-1",
                    "role": "user",
                    "content": "anon",
                },
            },
        }))
        .send()
        .await
        .expect("post");
    assert_eq!(resp.status(), 401);
    p.shutdown().await;
}

// --- gRPC tests ---------------------------------------------------

#[tokio::test]
async fn grpc_append_then_list_round_trip() {
    let p = start(None).await;
    let (mut client, bearer) = grpc_client(&p, TENANT).await;

    for (ts, role, body) in [
        ("2026-05-25T10:00:00Z", "user", "first"),
        ("2026-05-25T10:00:05Z", "assistant", "second"),
    ] {
        let resp = client
            .append_message(authed(
                AppendMessageRequest {
                    chat_group_id: "room-1".to_owned(),
                    role: role.to_owned(),
                    content: body.to_owned(),
                    author: String::new(),
                    ts: ts.to_owned(),
                    metadata_json: String::new(),
                    msg_id: String::new(),
                    embed: true,
                },
                &bearer,
            ))
            .await
            .unwrap()
            .into_inner();
        assert!(!resp.msg_id.is_empty(), "ack msg_id set");
        assert!(!resp.ts.is_empty(), "ack ts set");
    }

    let resp = client
        .list_messages(authed(
            ListMessagesRequest {
                chat_group_id: "room-1".to_owned(),
                since: String::new(),
                until: String::new(),
                limit: 0,
                cursor: String::new(),
                direction: "asc".to_owned(),
            },
            &bearer,
        ))
        .await
        .unwrap()
        .into_inner();
    let bodies: Vec<&str> = resp.messages.iter().map(|m| m.content.as_str()).collect();
    assert_eq!(bodies, vec!["first", "second"]);

    p.shutdown().await;
}

#[tokio::test]
async fn grpc_missing_auth_returns_unauthenticated() {
    let p = start(None).await;
    let endpoint = p.grpc_endpoint().expect("grpc endpoint");
    let channel = Channel::from_shared(endpoint.to_owned())
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut client = EscurelClient::new(channel);

    let err = client
        .append_message(Request::new(AppendMessageRequest {
            chat_group_id: "room-1".to_owned(),
            role: "user".to_owned(),
            content: "x".to_owned(),
            author: String::new(),
            ts: String::new(),
            metadata_json: String::new(),
            msg_id: String::new(),
            embed: true,
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::Unauthenticated);

    p.shutdown().await;
}
