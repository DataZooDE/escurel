//! M7-PR2: the events / inbox surface. Real DuckDB + FsStore +
//! ZeroEmbedder, no mocks. Captures an event into the inbox, then has
//! it assigned to an instance and verifies it moves into that
//! instance's processed event history.

use std::sync::Arc;

use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator, NewEvent};
use escurel_storage::{FsStore, LaneStore};
use tempfile::TempDir;

const TENANT: &str = "acme";
const INSTANCE: &str = "markdown/instances/engagement/spine.md";

struct Harness {
    indexer: Indexer,
    _store_dir: TempDir,
    _db_dir: TempDir,
}

fn fresh_harness() -> Harness {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let duckdb_path = db_dir.path().join("escurel.duckdb");
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(&duckdb_path).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Indexer::new(Arc::clone(&store), embedder, conn, TENANT).unwrap();
    Harness {
        indexer,
        _store_dir: store_dir,
        _db_dir: db_dir,
    }
}

fn gmail_event() -> NewEvent {
    NewEvent {
        at: Some("2026-04-01T09:00:00Z".to_owned()),
        source: "gmail".to_owned(),
        mime: "message/rfc822".to_owned(),
        label_skill: "email".to_owned(),
        title: "Contact form · datazoo.de".to_owned(),
        body: "An inbound enquiry.".to_owned(),
        provenance: Some(serde_json::json!({ "extracted_by": "agt:scout-a" })),
        ..Default::default()
    }
}

#[tokio::test]
async fn capture_with_explicit_event_id_is_idempotent() {
    // The dynamic-workflows keystone (§3.6): a reducer that re-runs (or two
    // reduce passes racing) emits the *same* content-addressed step id. So
    // capturing twice with the same explicit `event_id` must be a no-op the
    // second time — one inbox row, no primary-key error — which is what lets
    // the ledger's `(tenant, event_id)` index collapse the duplicate run.
    let h = fresh_harness();
    let step = NewEvent {
        event_id: Some("01HSTEPKEYDETERMINISTIC00".to_owned()),
        source: "escurel-runner".to_owned(),
        label_skill: "verify-vote".to_owned(),
        instance_page_id: Some("markdown/instances/verify-vote/r1-verify-abc123.md".to_owned()),
        title: "vote".to_owned(),
        body: "first".to_owned(),
        ..Default::default()
    };

    let first = h.indexer.capture_event(step.clone()).await.unwrap();
    assert_eq!(first.event_id, "01HSTEPKEYDETERMINISTIC00");

    // Re-emit the same step id (different body, as a re-run might) — must not
    // error and must not add a second inbox row; first-writer-wins.
    let second = NewEvent {
        body: "second".to_owned(),
        ..step
    };
    let again = h
        .indexer
        .capture_event(second)
        .await
        .expect("re-capturing the same event_id must not error");
    assert_eq!(again.event_id, "01HSTEPKEYDETERMINISTIC00");
    assert_eq!(
        again.body, "first",
        "first write wins; the second is a no-op"
    );

    let inbox = h.indexer.list_inbox(None).await.unwrap();
    assert_eq!(
        inbox.len(),
        1,
        "exactly one row for the deduplicated step id"
    );
    assert_eq!(inbox[0].body, "first");
}

#[tokio::test]
async fn capture_lands_in_inbox_then_assign_moves_to_instance() {
    let h = fresh_harness();

    // 1. Capture → lands in the inbox with a server-generated id.
    let ev = h.indexer.capture_event(gmail_event()).await.unwrap();
    assert!(!ev.event_id.is_empty(), "server assigns an event id");
    assert_eq!(ev.status, "inbox");
    assert_eq!(ev.at.as_deref(), Some("2026-04-01T09:00:00Z"));
    assert_eq!(ev.label_skill, "email");

    // 2. The inbox shows it; the instance has no history yet.
    let inbox = h.indexer.list_inbox(None).await.unwrap();
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].event_id, ev.event_id);
    assert_eq!(inbox[0].source, "gmail");
    assert!(
        h.indexer
            .list_events(INSTANCE, None)
            .await
            .unwrap()
            .is_empty(),
        "unassigned event is not in any instance's history",
    );

    // 3. The (simulated) agent assigns it to the instance.
    h.indexer
        .assign_event(&ev.event_id, INSTANCE)
        .await
        .unwrap();

    // 4. It has left the inbox and entered the instance's event history.
    assert!(
        h.indexer.list_inbox(None).await.unwrap().is_empty(),
        "assigned event leaves the inbox",
    );
    let history = h.indexer.list_events(INSTANCE, None).await.unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].event_id, ev.event_id);
    assert_eq!(history[0].status, "processed");
    assert_eq!(history[0].instance_page_id.as_deref(), Some(INSTANCE));
    assert_eq!(
        history[0].provenance["extracted_by"], "agt:scout-a",
        "provenance round-trips",
    );
}

#[tokio::test]
async fn capture_can_preflag_a_candidate_instance_but_stays_in_inbox() {
    let h = fresh_harness();
    let mut ev = gmail_event();
    ev.instance_page_id = Some(INSTANCE.to_owned()); // Gmail-label-style hint
    let stored = h.indexer.capture_event(ev).await.unwrap();

    // Pre-flagged but unprocessed: still in the inbox, not yet history.
    assert_eq!(stored.status, "inbox");
    assert_eq!(h.indexer.list_inbox(None).await.unwrap().len(), 1);
    assert!(
        h.indexer
            .list_events(INSTANCE, None)
            .await
            .unwrap()
            .is_empty(),
        "a pre-flag is a hint, not processing",
    );
}
