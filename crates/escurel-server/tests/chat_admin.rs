//! End-to-end tests for the admin `DeleteChatHistory` RPC
//! (DataZooDE/escurel#63 — M-Chat retention + GDPR erasure).
//!
//! Seeds messages via the agent `append_message` tool, then exercises
//! the admin `DeleteChatHistory` RPC across the documented filter
//! combinations: full purge, per-group purge, before-cutoff prune,
//! and the agent-role rejection (admin-only).

use escurel_proto::v1::escurel_admin_client::EscurelAdminClient;
use escurel_proto::v1::escurel_client::EscurelClient;
use escurel_proto::v1::{AppendMessageRequest, DeleteChatHistoryRequest, ListMessagesRequest};
use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use tonic::Request;
use tonic::metadata::MetadataValue;
use tonic::transport::Channel;

const TENANT: &str = "carl";

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(FixtureBuilder::new().tenant(TENANT).done()),
        ..Default::default()
    })
    .await
}

fn bearer(token: &str) -> MetadataValue<tonic::metadata::Ascii> {
    format!("Bearer {token}").parse().unwrap()
}

fn req<T>(body: T, bearer: &MetadataValue<tonic::metadata::Ascii>) -> Request<T> {
    let mut r = Request::new(body);
    r.metadata_mut().insert("authorization", bearer.clone());
    r
}

async fn channel(p: &EscurelProcess) -> Channel {
    let endpoint = p.grpc_endpoint().expect("grpc endpoint");
    Channel::from_shared(endpoint.to_owned())
        .unwrap()
        .connect()
        .await
        .unwrap()
}

async fn seed(p: &EscurelProcess, group: &str, ts: &str, content: &str) {
    let token = p.mint_token(TENANT, Role::Agent);
    let bearer = bearer(&token);
    let mut client = EscurelClient::new(channel(p).await);
    client
        .append_message(req(
            AppendMessageRequest {
                chat_group_id: group.to_owned(),
                role: "user".to_owned(),
                content: content.to_owned(),
                author: String::new(),
                ts: ts.to_owned(),
                metadata_json: String::new(),
                msg_id: String::new(),
                embed: true,
            },
            &bearer,
        ))
        .await
        .unwrap();
}

async fn seed_author(p: &EscurelProcess, group: &str, ts: &str, content: &str, author: &str) {
    let token = p.mint_token(TENANT, Role::Agent);
    let bearer = bearer(&token);
    let mut client = EscurelClient::new(channel(p).await);
    client
        .append_message(req(
            AppendMessageRequest {
                chat_group_id: group.to_owned(),
                role: "user".to_owned(),
                content: content.to_owned(),
                author: author.to_owned(),
                ts: ts.to_owned(),
                metadata_json: String::new(),
                msg_id: String::new(),
                embed: true,
            },
            &bearer,
        ))
        .await
        .unwrap();
}

async fn count(p: &EscurelProcess, group: &str) -> usize {
    let token = p.mint_token(TENANT, Role::Agent);
    let bearer = bearer(&token);
    let mut client = EscurelClient::new(channel(p).await);
    let resp = client
        .list_messages(req(
            ListMessagesRequest {
                chat_group_id: group.to_owned(),
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
    resp.messages.len()
}

#[tokio::test]
async fn delete_chat_history_purges_group() {
    let p = start().await;
    seed(&p, "room-1", "2026-05-25T10:00:00Z", "a").await;
    seed(&p, "room-2", "2026-05-25T10:00:00Z", "b").await;

    let admin = p.mint_token(TENANT, Role::Admin);
    let mut admin_client = EscurelAdminClient::new(channel(&p).await);
    let resp = admin_client
        .delete_chat_history(req(
            DeleteChatHistoryRequest {
                tenant_id: TENANT.to_owned(),
                chat_group_id: "room-1".to_owned(),
                before_ts: String::new(),
                author: String::new(),
            },
            &bearer(&admin),
        ))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.deleted, 1);

    assert_eq!(count(&p, "room-1").await, 0, "room-1 empty after purge");
    assert_eq!(count(&p, "room-2").await, 1, "room-2 untouched");

    p.shutdown().await;
}

#[tokio::test]
async fn delete_chat_history_with_before_ts_purges_old_only() {
    let p = start().await;
    for (i, ts) in [
        "2026-05-25T10:00:00Z",
        "2026-05-25T10:00:01Z",
        "2026-05-25T10:00:02Z",
        "2026-05-25T10:00:03Z",
    ]
    .iter()
    .enumerate()
    {
        seed(&p, "room-1", ts, &format!("m{i}")).await;
    }

    let admin = p.mint_token(TENANT, Role::Admin);
    let mut admin_client = EscurelAdminClient::new(channel(&p).await);
    let resp = admin_client
        .delete_chat_history(req(
            DeleteChatHistoryRequest {
                tenant_id: TENANT.to_owned(),
                chat_group_id: "room-1".to_owned(),
                before_ts: "2026-05-25T10:00:02Z".to_owned(),
                author: String::new(),
            },
            &bearer(&admin),
        ))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.deleted, 2, "two rows strictly before the cutoff");

    assert_eq!(count(&p, "room-1").await, 2, "two rows at or after survive");

    p.shutdown().await;
}

#[tokio::test]
async fn delete_chat_history_without_filters_purges_everything() {
    let p = start().await;
    seed(&p, "room-1", "2026-05-25T10:00:00Z", "a").await;
    seed(&p, "room-2", "2026-05-25T10:00:00Z", "b").await;

    let admin = p.mint_token(TENANT, Role::Admin);
    let mut admin_client = EscurelAdminClient::new(channel(&p).await);
    let resp = admin_client
        .delete_chat_history(req(
            DeleteChatHistoryRequest {
                tenant_id: TENANT.to_owned(),
                chat_group_id: String::new(),
                before_ts: String::new(),
                author: String::new(),
            },
            &bearer(&admin),
        ))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.deleted, 2);

    p.shutdown().await;
}

/// GDPR right-to-erasure of a single member: deleting by `author`
/// across all groups removes only that member's messages and leaves
/// every other member's history intact (issue #63).
#[tokio::test]
async fn delete_chat_history_by_author_erases_only_that_member() {
    let p = start().await;
    seed_author(&p, "room-1", "2026-05-25T10:00:00Z", "from alice", "alice").await;
    seed_author(&p, "room-1", "2026-05-25T10:00:01Z", "from bob", "bob").await;
    seed_author(
        &p,
        "room-2",
        "2026-05-25T10:00:02Z",
        "alice elsewhere",
        "alice",
    )
    .await;

    let admin = p.mint_token(TENANT, Role::Admin);
    let mut admin_client = EscurelAdminClient::new(channel(&p).await);
    let resp = admin_client
        .delete_chat_history(req(
            DeleteChatHistoryRequest {
                tenant_id: TENANT.to_owned(),
                chat_group_id: String::new(), // every group
                before_ts: String::new(),
                author: "alice".to_owned(), // erase this member everywhere
            },
            &bearer(&admin),
        ))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        resp.deleted, 2,
        "both of alice's messages, across both groups"
    );

    assert_eq!(count(&p, "room-1").await, 1, "bob's message remains");
    assert_eq!(
        count(&p, "room-2").await,
        0,
        "alice's only room-2 message erased"
    );

    p.shutdown().await;
}

/// `author` composes with `chat_group_id`: erase a member within one
/// group only, leaving the same member's messages in other groups.
#[tokio::test]
async fn delete_chat_history_by_author_scoped_to_group() {
    let p = start().await;
    seed_author(&p, "room-1", "2026-05-25T10:00:00Z", "alice r1", "alice").await;
    seed_author(&p, "room-2", "2026-05-25T10:00:01Z", "alice r2", "alice").await;

    let admin = p.mint_token(TENANT, Role::Admin);
    let mut admin_client = EscurelAdminClient::new(channel(&p).await);
    let resp = admin_client
        .delete_chat_history(req(
            DeleteChatHistoryRequest {
                tenant_id: TENANT.to_owned(),
                chat_group_id: "room-1".to_owned(),
                before_ts: String::new(),
                author: "alice".to_owned(),
            },
            &bearer(&admin),
        ))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.deleted, 1, "only alice's room-1 message");

    assert_eq!(count(&p, "room-1").await, 0);
    assert_eq!(
        count(&p, "room-2").await,
        1,
        "alice's room-2 message untouched"
    );

    p.shutdown().await;
}

#[tokio::test]
async fn delete_chat_history_rejects_agent_role() {
    let p = start().await;
    seed(&p, "room-1", "2026-05-25T10:00:00Z", "a").await;

    let agent = p.mint_token(TENANT, Role::Agent);
    let mut admin_client = EscurelAdminClient::new(channel(&p).await);
    let err = admin_client
        .delete_chat_history(req(
            DeleteChatHistoryRequest {
                tenant_id: TENANT.to_owned(),
                chat_group_id: "room-1".to_owned(),
                before_ts: String::new(),
                author: String::new(),
            },
            &bearer(&agent),
        ))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::PermissionDenied);

    // The row is still there.
    assert_eq!(count(&p, "room-1").await, 1);

    p.shutdown().await;
}
