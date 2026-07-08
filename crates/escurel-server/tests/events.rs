//! Event-sourcing surface (M7): capture → inbox → assign → events
//! round-trip over the real HTTP (MCP) transport.
//!
//! These exercise the `escurel_client::Client` event methods
//! end-to-end against a live gateway: a captured event lands in the
//! inbox, assignment moves it onto an instance and out of the inbox,
//! and the per-instance history lists it back.

use escurel_client::{
    AssignEventRequest, CaptureEventRequest, ListEventsRequest, ListInboxRequest,
};
use escurel_test_support::{AuthMode, EscurelProcess, FixtureBuilder, Opts, Role};

const TENANT: &str = "acme"; // aligned to the served indexer; tenant boundary now enforced
const SPINE: &str = "markdown/instances/engagement__spine.md";

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(FixtureBuilder::new().tenant(TENANT).done()),
        ..Default::default()
    })
    .await
}

#[tokio::test]
async fn capture_inbox_assign_events_round_trip() {
    let p = start().await;
    let c = p.client_for(TENANT, Role::Agent).await;

    // Capture two events into the inbox.
    let e1 = c
        .capture_event(CaptureEventRequest {
            source: "gmail".to_owned(),
            mime: "message/rfc822".to_owned(),
            label_skill: "email".to_owned(),
            title: "first".to_owned(),
            body: "hello".to_owned(),
            ..Default::default()
        })
        .await
        .expect("capture 1");
    assert_eq!(e1.status, "inbox");
    assert!(!e1.event_id.is_empty(), "server-generated event id");

    c.capture_event(CaptureEventRequest {
        source: "gmail".to_owned(),
        mime: "message/rfc822".to_owned(),
        label_skill: "email".to_owned(),
        title: "second".to_owned(),
        body: "world".to_owned(),
        ..Default::default()
    })
    .await
    .expect("capture 2");

    // Both sit in the inbox.
    let inbox = c
        .list_inbox(ListInboxRequest { limit: 0 })
        .await
        .expect("list_inbox");
    assert_eq!(inbox.events.len(), 2);

    // The spine has no processed events yet.
    let before = c
        .list_events(ListEventsRequest {
            instance_page_id: SPINE.to_owned(),
            limit: 0,
        })
        .await
        .expect("list_events before");
    assert_eq!(before.events.len(), 0);

    // Fold e1 into the spine.
    let ack = c
        .assign_event(AssignEventRequest {
            event_id: e1.event_id.clone(),
            instance_page_id: SPINE.to_owned(),
        })
        .await
        .expect("assign");
    assert_eq!(ack.event_id, e1.event_id);
    assert_eq!(ack.instance_page_id, SPINE);

    // The inbox shrank and the spine's history grew.
    let inbox_after = c
        .list_inbox(ListInboxRequest { limit: 0 })
        .await
        .expect("list_inbox after");
    assert_eq!(inbox_after.events.len(), 1, "e1 left the inbox");

    let after = c
        .list_events(ListEventsRequest {
            instance_page_id: SPINE.to_owned(),
            limit: 0,
        })
        .await
        .expect("list_events after");
    assert_eq!(after.events.len(), 1, "e1 folded into the spine");
    assert_eq!(after.events[0].event_id, e1.event_id);
    assert_eq!(after.events[0].status, "processed");
    p.shutdown().await;
}
