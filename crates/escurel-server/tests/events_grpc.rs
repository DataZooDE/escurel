//! End-to-end tests for the gRPC event/inbox tools (M7-PR2b) — the
//! native-gRPC twins of the MCP `capture_event` / `list_inbox` /
//! `list_events` / `assign_event` tools.
//!
//! Real running gateway + real Indexer (DuckDB + FsStore +
//! ZeroEmbedder), exercised over native gRPC (tonic `EscurelClient`):
//! capture two events into the inbox, assign one to an instance, and
//! assert the inbox / event-history views move accordingly — proving
//! the gRPC surface mirrors the MCP one 1:1.

use escurel_proto::v1::escurel_client::EscurelClient;
use escurel_proto::v1::{
    AssignEventRequest, CaptureEventRequest, ListEventsRequest, ListInboxRequest,
};
use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};
use tonic::Request;
use tonic::metadata::MetadataValue;
use tonic::transport::Channel;

const TENANT: &str = "carl";
const SPINE: &str = "markdown/instances/engagement__spine.md";

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(FixtureBuilder::new().tenant(TENANT).done()),
        ..Default::default()
    })
    .await
}

async fn client(
    p: &EscurelProcess,
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
    let token = p.mint_token(TENANT, Role::Agent);
    let bearer: MetadataValue<_> = format!("Bearer {token}").parse().unwrap();
    (EscurelClient::new(channel), bearer)
}

fn authed<T>(body: T, bearer: &MetadataValue<tonic::metadata::Ascii>) -> Request<T> {
    let mut r = Request::new(body);
    r.metadata_mut().insert("authorization", bearer.clone());
    r
}

#[tokio::test]
async fn capture_inbox_assign_list_round_trip_over_grpc() {
    let p = start().await;
    let (mut c, b) = client(&p).await;

    // Capture two events into the inbox.
    let e1 = c
        .capture_event(authed(
            CaptureEventRequest {
                source: "gmail".into(),
                mime: "message/rfc822".into(),
                label_skill: "gmail".into(),
                title: "contact form".into(),
                ..Default::default()
            },
            &b,
        ))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(e1.status, "inbox");
    assert!(!e1.event_id.is_empty(), "server-generated event id");

    c.capture_event(authed(
        CaptureEventRequest {
            source: "meet".into(),
            label_skill: "meet".into(),
            title: "discovery call".into(),
            ..Default::default()
        },
        &b,
    ))
    .await
    .unwrap();

    // Both sit in the inbox.
    let inbox = c
        .list_inbox(authed(ListInboxRequest { limit: 0 }, &b))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(inbox.events.len(), 2);

    // The spine has no processed events yet.
    let before = c
        .list_events(authed(
            ListEventsRequest {
                instance_page_id: SPINE.into(),
                limit: 0,
            },
            &b,
        ))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(before.events.len(), 0);

    // Fold e1 into the spine.
    let ack = c
        .assign_event(authed(
            AssignEventRequest {
                event_id: e1.event_id.clone(),
                instance_page_id: SPINE.into(),
            },
            &b,
        ))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(ack.event_id, e1.event_id);
    assert_eq!(ack.instance_page_id, SPINE);

    // The inbox shrank and the spine's history grew.
    let inbox_after = c
        .list_inbox(authed(ListInboxRequest { limit: 0 }, &b))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(inbox_after.events.len(), 1, "e1 left the inbox");

    let after = c
        .list_events(authed(
            ListEventsRequest {
                instance_page_id: SPINE.into(),
                limit: 0,
            },
            &b,
        ))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(after.events.len(), 1, "e1 folded into the spine");
    assert_eq!(after.events[0].event_id, e1.event_id);
    assert_eq!(after.events[0].status, "processed");

    p.shutdown().await;
}
