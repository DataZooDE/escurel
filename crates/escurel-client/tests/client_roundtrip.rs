//! End-to-end tests for the `escurel-client` typed wrapper.
//!
//! Real gateway via `escurel-test-support`, real MCP-over-HTTP
//! transport, real `OidcVerifier` against the in-process JWKS the
//! support crate stands up, real `Indexer` with a real DuckDB file. No
//! mocks at the boundary the test exercises (CLAUDE principle 2).

use escurel_client::{
    AppendMessageRequest, AssignEventRequest, CaptureEventRequest, Client, ExpandRequest,
    ListEventsRequest, ListInboxRequest, ListMessagesRequest, ListSkillsRequest, ResolveRequest,
    SearchRequest, SecretString, UpdatePageRequest, ValidateRequest,
};
use escurel_test_support::{AuthMode, ConfigOverrides, EscurelProcess, FixtureBuilder, Opts, Role};

const TENANT: &str = "acme";

const CUSTOMER_SKILL: &str = "---\n\
type: skill\n\
id: customer\n\
description: A buying organisation.\n\
required_frontmatter: [id, name]\n\
optional_frontmatter: [tier]\n\
---\n\
# customer\n";

const ACME_INSTANCE: &str = "---\n\
type: instance\n\
skill: customer\n\
id: acme\n\
name: Acme Corp\n\
tier: gold\n\
---\n\
# Acme Corp\n\nKey account. See [[customer::initech]].\n";

const INITECH_INSTANCE: &str = "---\n\
type: instance\n\
skill: customer\n\
id: initech\n\
name: Initech\n\
---\n\
# Initech\n";

async fn start() -> EscurelProcess {
    EscurelProcess::spawn(Opts {
        auth: AuthMode::TestIssuer,
        fixtures: Some(
            FixtureBuilder::new()
                .tenant(TENANT)
                .skill("customer", CUSTOMER_SKILL)
                .instance("customer", "acme", ACME_INSTANCE)
                .instance("customer", "initech", INITECH_INSTANCE)
                .done(),
        ),
        config_overrides: ConfigOverrides {
            gateway_version: Some("1.0.0-test".to_owned()),
            ..Default::default()
        },
    })
    .await
}

async fn authed_client(p: &EscurelProcess) -> Client {
    let token = p.mint_token(TENANT, Role::Agent);
    Client::connect(p.base_url(), SecretString::from(token))
        .await
        .unwrap()
}

/// Resolve a `[[wikilink]]` to its concrete `page_id`.
async fn resolve_page_id(client: &Client, wikilink: &str) -> String {
    client
        .resolve(ResolveRequest {
            wikilink: wikilink.to_owned(),
            ..Default::default()
        })
        .await
        .unwrap()
        .page
        .expect("page present")
        .page_id
}

#[tokio::test]
async fn connect_succeeds_against_running_gateway() {
    let p = start().await;
    // A successful connect + a trivial RPC proves the channel is
    // up and the token is being threaded through.
    let client = authed_client(&p).await;
    client
        .list_skills(ListSkillsRequest::default())
        .await
        .unwrap();
    p.shutdown().await;
}

#[tokio::test]
async fn list_skills_round_trips() {
    let p = start().await;
    let client = authed_client(&p).await;
    let resp = client
        .list_skills(ListSkillsRequest::default())
        .await
        .unwrap();
    // Every tenant ships the mandatory `escurel` meta-skill alongside
    // the seeded `customer` (locked decision 3).
    assert!(resp.skills.iter().any(|s| s.id == "escurel"));
    let customer = resp
        .skills
        .iter()
        .find(|s| s.id == "customer")
        .expect("seeded customer skill present");
    assert_eq!(customer.description, "A buying organisation.");
    p.shutdown().await;
}

#[tokio::test]
async fn resolve_round_trips() {
    let p = start().await;
    let client = authed_client(&p).await;
    let resp = client
        .resolve(ResolveRequest {
            wikilink: "[[customer::acme]]".to_owned(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(resp.exists);
    let page = resp.page.expect("page present");
    assert_eq!(page.skill, "customer");
    assert_eq!(page.slug, "acme");
    p.shutdown().await;
}

#[tokio::test]
async fn expand_round_trips() {
    let p = start().await;
    let client = authed_client(&p).await;
    let resolved = client
        .resolve(ResolveRequest {
            wikilink: "[[customer::acme]]".to_owned(),
            ..Default::default()
        })
        .await
        .unwrap();
    let page_id = resolved.page.unwrap().page_id;
    let resp = client
        .expand(ExpandRequest {
            page_id,
            anchor: String::new(),
            version: String::new(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(!resp.body.is_empty());
    assert!(resp.wikilinks_out.iter().any(|w| w.id == "initech"));
    p.shutdown().await;
}

#[tokio::test]
async fn search_round_trips() {
    let p = start().await;
    let client = authed_client(&p).await;
    // ZeroEmbedder + FTS-backed search; query the seeded body text.
    let resp = client
        .search(SearchRequest {
            q: "Acme".to_owned(),
            k: 5,
            ..Default::default()
        })
        .await
        .unwrap();
    // The response shape is what the contract commits to — the
    // surface returns whatever the indexer ranked. Asserting on
    // `granularity` is the cheapest stable invariant.
    assert_eq!(resp.granularity, "block");
    p.shutdown().await;
}

#[tokio::test]
async fn update_page_round_trips() {
    let p = start().await;
    let client = authed_client(&p).await;
    let body = "---\n\
type: instance\n\
skill: customer\n\
id: globex\n\
name: Globex\n\
---\n\
# Globex\n";
    let resp = client
        .update_page(UpdatePageRequest {
            page_id: "markdown/instances/customer/globex.md".to_owned(),
            content: body.to_owned(),
        })
        .await
        .unwrap();
    assert!(resp.ok, "update_page should succeed: {resp:?}");
    p.shutdown().await;
}

#[tokio::test]
async fn missing_token_surfaces_unauthenticated_error() {
    let p = start().await;
    // Bogus token: parses fine as a header but the verifier rejects
    // it. The HTTP transport surfaces the rejection as `Error::Http {
    // status: 401, .. }`.
    let client = Client::connect(
        p.base_url(),
        SecretString::from("not.a.real.jwt".to_owned()),
    )
    .await
    .unwrap();
    let err = client
        .list_skills(ListSkillsRequest::default())
        .await
        .unwrap_err();
    match err {
        escurel_client::Error::Http { status, .. } => {
            assert_eq!(status, 401, "expected 401 Unauthorized, got {status}");
        }
        other => panic!("expected Error::Http {{ status: 401 }}, got {other:?}"),
    }
    p.shutdown().await;
}

#[tokio::test]
async fn append_then_list_messages_round_trip() {
    let p = start().await;
    let client = authed_client(&p).await;

    for (ts, content) in [
        ("2026-05-26T09:00:00Z", "hello"),
        ("2026-05-26T09:00:05Z", "world"),
    ] {
        let ack = client
            .append_message(AppendMessageRequest {
                chat_group_id: "room-1".to_owned(),
                role: "user".to_owned(),
                content: content.to_owned(),
                ts: ts.to_owned(),
                embed: true,
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(!ack.msg_id.is_empty());
        assert!(!ack.ts.is_empty());
    }

    let resp = client
        .list_messages(ListMessagesRequest {
            chat_group_id: "room-1".to_owned(),
            direction: "asc".to_owned(),
            ..Default::default()
        })
        .await
        .unwrap();
    let bodies: Vec<&str> = resp.messages.iter().map(|m| m.content.as_str()).collect();
    assert_eq!(bodies, vec!["hello", "world"]);
    p.shutdown().await;
}

/// `validate` dry-runs the indexer pipeline over draft content and
/// returns the same issue list the write path would surface — without
/// committing. A well-formed instance page validates clean.
#[tokio::test]
async fn validate_accepts_well_formed_page() {
    let p = start().await;
    let client = authed_client(&p).await;
    let page_id = resolve_page_id(&client, "[[customer::acme]]").await;
    let resp = client
        .validate(ValidateRequest {
            as_page_id: page_id,
            content: ACME_INSTANCE.to_owned(),
        })
        .await
        .unwrap();
    assert!(
        resp.ok,
        "expected clean validation, issues: {:?}",
        resp.issues
    );
    p.shutdown().await;
}

/// Realistic CRM flow exercising the whole M7 event quartet end to end:
/// capture an event (lands in the inbox) → see it in `list_inbox` →
/// `assign_event` it to the Acme customer instance → read it back from
/// that instance's processed history via `list_events`.
#[tokio::test]
async fn capture_inbox_assign_list_events_round_trip() {
    let p = start().await;
    let client = authed_client(&p).await;
    let acme = resolve_page_id(&client, "[[customer::acme]]").await;

    let captured = client
        .capture_event(CaptureEventRequest {
            source: "manual".to_owned(),
            mime: "text/plain".to_owned(),
            label_skill: "note".to_owned(),
            title: "Renewal call".to_owned(),
            body: "Acme wants to renew the gold tier.".to_owned(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        !captured.event_id.is_empty(),
        "server should mint a ULID event_id"
    );
    assert_eq!(captured.status, "inbox");
    let event_id = captured.event_id.clone();

    // Unassigned event is visible in the global inbox.
    let inbox = client
        .list_inbox(ListInboxRequest { limit: 0 })
        .await
        .unwrap();
    assert!(
        inbox.events.iter().any(|e| e.event_id == event_id),
        "captured event {event_id} not found in inbox"
    );

    // Assign it to the Acme instance; the ack echoes the binding.
    let ack = client
        .assign_event(AssignEventRequest {
            event_id: event_id.clone(),
            instance_page_id: acme.clone(),
        })
        .await
        .unwrap();
    assert_eq!(ack.event_id, event_id);
    assert_eq!(ack.instance_page_id, acme);

    // It now appears in the instance's processed event history and has
    // left the inbox.
    let events = client
        .list_events(ListEventsRequest {
            instance_page_id: acme.clone(),
            limit: 0,
        })
        .await
        .unwrap();
    let assigned = events
        .events
        .iter()
        .find(|e| e.event_id == event_id)
        .expect("assigned event in instance history");
    assert_eq!(assigned.status, "processed");
    assert_eq!(assigned.instance_page_id, acme);

    let inbox_after = client
        .list_inbox(ListInboxRequest { limit: 0 })
        .await
        .unwrap();
    assert!(
        !inbox_after.events.iter().any(|e| e.event_id == event_id),
        "assigned event should no longer be in the inbox"
    );
    p.shutdown().await;
}

/// `list_inbox` honours its `limit` cap.
#[tokio::test]
async fn list_inbox_respects_limit() {
    let p = start().await;
    let client = authed_client(&p).await;
    for i in 0..3 {
        client
            .capture_event(CaptureEventRequest {
                source: "manual".to_owned(),
                mime: "text/plain".to_owned(),
                label_skill: "note".to_owned(),
                title: format!("evt-{i}"),
                body: "body".to_owned(),
                ..Default::default()
            })
            .await
            .unwrap();
    }
    let limited = client
        .list_inbox(ListInboxRequest { limit: 2 })
        .await
        .unwrap();
    assert_eq!(limited.events.len(), 2, "limit=2 should cap the inbox read");
    p.shutdown().await;
}

#[tokio::test]
async fn invalid_endpoint_url_returns_error() {
    // Not a URL at all — must surface as `InvalidEndpoint`, never
    // as a panic, never as a connect-timeout.
    let err = Client::connect("not a url", SecretString::from("x".to_owned()))
        .await
        .unwrap_err();
    assert!(
        matches!(err, escurel_client::Error::InvalidEndpoint(_)),
        "got {err:?}"
    );
}

#[tokio::test]
async fn token_is_not_leaked_in_debug_output() {
    // The secret marker must not appear in any `{:?}` formatting of
    // the client. This is the only mechanical check we have against
    // accidental log leaks.
    let secret = "THIS_TOKEN_SHOULD_NEVER_APPEAR_IN_LOGS_xyz123";
    let p = start().await;
    let client = Client::connect(p.base_url(), SecretString::from(secret.to_owned()))
        .await
        .unwrap();
    let dbg = format!("{client:?}");
    assert!(
        !dbg.contains(secret),
        "bearer token leaked into Debug output: {dbg}"
    );
    p.shutdown().await;
}
