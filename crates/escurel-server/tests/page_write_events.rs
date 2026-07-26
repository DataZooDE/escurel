//! A confirmed `update_page` write announces itself to the outbound webhook —
//! as a NOTIFICATION, never as an inbox item.
//!
//! The distinction is the whole design. The inbox is a work queue that the
//! runner drains and dispatches. Enqueueing writes turns a write into work,
//! and then a write announces itself, the runner dispatches it, processing
//! writes again — unbounded, since a page-write carries no cascade lineage
//! for the depth/budget caps to bound. Measured: one demo run produced 122
//! announcements for a single skill and starved the real workflow behind a
//! quota gate.
//!
//! Notifying instead keeps the queue's meaning, and is also what makes
//! workflow completion observable — a runner-authored write is exactly the
//! interesting one, and any inbox-based scheme must suppress it to stay safe.

use escurel_client::{ListInboxRequest, UpdatePageRequest};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};
use std::sync::{Arc, Mutex};

const TENANT: &str = "acme";
const PAGE: &str = "markdown/instances/customer/acme-gmbh.md";
const BODY: &str =
    "---\ntype: instance\nskill: customer\nid: acme-gmbh\n---\n# Acme GmbH\nA customer.\n";

/// A webhook sink that records every delivered payload.
async fn sink() -> (String, Arc<Mutex<Vec<serde_json::Value>>>) {
    let seen: Arc<Mutex<Vec<serde_json::Value>>> = Arc::default();
    let got = Arc::clone(&seen);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind sink");
    let url = format!("http://{}/hook", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let app = axum::Router::new().route(
            "/hook",
            axum::routing::post(move |body: axum::body::Bytes| {
                let got = Arc::clone(&got);
                async move {
                    if let Ok(v) = serde_json::from_slice(&body) {
                        got.lock().unwrap().push(v);
                    }
                    axum::http::StatusCode::NO_CONTENT
                }
            }),
        );
        let _ = axum::serve(listener, app).await;
    });
    (url, seen)
}

/// A confirmed instance write reaches the webhook, and the inbox stays empty.
#[tokio::test]
async fn a_write_notifies_the_webhook_but_never_the_inbox() {
    let (hook, seen) = sink().await;
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(FixtureBuilder::new().tenant(TENANT).done()),
        config_overrides: ConfigOverrides {
            webhook_url: Some(hook),
            ..Default::default()
        },
    })
    .await;
    let c = p.client_for(TENANT, Role::Agent).await;

    let before = c
        .list_inbox(ListInboxRequest { limit: 100 })
        .await
        .expect("list_inbox")
        .events
        .len();

    c.update_page(UpdatePageRequest {
        page_id: PAGE.to_owned(),
        content: BODY.to_owned(),
    })
    .await
    .expect("update_page");

    // The notification lands…
    let mut delivered = None;
    for _ in 0..50 {
        if let Some(v) = seen
            .lock()
            .unwrap()
            .iter()
            .find(|v| v.get("kind").and_then(|k| k.as_str()) == Some("page_write"))
            .cloned()
        {
            delivered = Some(v);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let ev = delivered.expect("a page_write notification reaches the webhook");
    assert_eq!(ev["instance_page_id"], PAGE, "names the written page");
    assert_eq!(ev["label_skill"], "customer", "skill from the page path");

    // …and the WORK QUEUE is untouched. This is the property that keeps the
    // runner from dispatching writes and looping.
    let after = c
        .list_inbox(ListInboxRequest { limit: 100 })
        .await
        .expect("list_inbox")
        .events
        .len();
    assert_eq!(after, before, "a page write must never enqueue work");
}

/// Skill pages are not instance changes; nothing is announced.
#[tokio::test]
async fn a_skill_page_write_is_not_announced() {
    let (hook, seen) = sink().await;
    let p = EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(FixtureBuilder::new().tenant(TENANT).done()),
        config_overrides: ConfigOverrides {
            webhook_url: Some(hook),
            ..Default::default()
        },
    })
    .await;
    let c = p.client_for(TENANT, Role::Agent).await;

    let _ = c
        .update_page(UpdatePageRequest {
            page_id: "markdown/skills/customer.md".to_owned(),
            content: "---\ntype: skill\nid: customer\ndescription: Customers.\n---\n# customer\n"
                .to_owned(),
        })
        .await;

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert!(
        !seen
            .lock()
            .unwrap()
            .iter()
            .any(|v| v.get("kind").and_then(|k| k.as_str()) == Some("page_write")),
        "a skill-page write is not an instance change"
    );
}
