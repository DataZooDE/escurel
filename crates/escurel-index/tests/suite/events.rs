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

// --- assign_event compare-and-set -------------------------------------
//
// `assign_event` used to be a blind last-writer-wins UPDATE whose
// rows-affected count was discarded, so two agents racing to claim the
// same inbox event both "succeeded" and the second silently overwrote the
// first's binding. It is now a CAS on `status = 'inbox'`. These tests pin
// the three outcomes that matters: the claim, the safe re-run, and the
// conflict.

#[tokio::test]
async fn assign_event_is_idempotent_for_the_same_instance() {
    let h = fresh_harness();
    let ev = h.indexer.capture_event(gmail_event()).await.unwrap();

    h.indexer
        .assign_event(&ev.event_id, INSTANCE)
        .await
        .unwrap();
    // The runner re-runs `update_page` + `assign_event` to finish a
    // partial success, so a second identical assign MUST still succeed.
    h.indexer
        .assign_event(&ev.event_id, INSTANCE)
        .await
        .expect("re-assigning to the same instance is an idempotent re-run, not a conflict");

    let history = h.indexer.list_events(INSTANCE, None).await.unwrap();
    assert_eq!(history.len(), 1, "the re-run must not duplicate history");
    assert_eq!(history[0].instance_page_id.as_deref(), Some(INSTANCE));
}

#[tokio::test]
async fn assign_event_refuses_to_reassign_a_claimed_event() {
    let h = fresh_harness();
    let ev = h.indexer.capture_event(gmail_event()).await.unwrap();
    const OTHER: &str = "markdown/instances/engagement/other.md";

    h.indexer
        .assign_event(&ev.event_id, INSTANCE)
        .await
        .unwrap();
    let err = h
        .indexer
        .assign_event(&ev.event_id, OTHER)
        .await
        .expect_err("a second instance claiming a processed event is a conflict");

    let msg = err.to_string();
    assert!(
        msg.contains("already assigned") && msg.contains(INSTANCE),
        "the error must name the winning instance, got: {msg}",
    );
    // The original binding survives — this is the data loss the CAS prevents.
    let history = h.indexer.list_events(INSTANCE, None).await.unwrap();
    assert_eq!(history.len(), 1);
    assert!(
        h.indexer.list_events(OTHER, None).await.unwrap().is_empty(),
        "the losing claim must not have moved the event",
    );
}

#[tokio::test]
async fn assign_event_on_a_missing_event_is_an_error() {
    let h = fresh_harness();
    let err = h
        .indexer
        .assign_event("01ARZ3NDEKTSV4RRFFQ69G5FAV", INSTANCE)
        .await
        .expect_err("assigning an event that was never captured must fail");
    assert!(err.to_string().contains("does not exist"), "got: {err}",);
}

/// Drain a paged event surface until `next_cursor` disappears; panics on
/// a replayed id (a cursor that fails to advance) or a runaway listing.
async fn drain_pages<F, Fut>(mut fetch: F) -> Vec<String>
where
    F: FnMut(Option<String>) -> Fut,
    Fut: std::future::Future<Output = escurel_index::EventPage>,
{
    let mut seen: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    for page_no in 0..20 {
        let page = fetch(cursor.clone()).await;
        for e in &page.events {
            assert!(
                !seen.contains(&e.event_id),
                "page {page_no} replayed `{}` — the cursor must advance",
                e.event_id,
            );
            seen.push(e.event_id.clone());
        }
        match page.next_cursor {
            Some(c) => cursor = Some(c),
            None => return seen,
        }
    }
    panic!("listing never terminated — next_cursor kept coming");
}

/// Capture 3 dated + 3 undated (`at_ts IS NULL`) events. The NULLS LAST
/// block must paginate correctly in BOTH directions: the cursor crosses
/// the dated→NULL boundary and then resumes inside the NULL block by
/// `event_id` alone (`events.rs`'s resume predicate).
async fn capture_mixed_null_at(h: &Harness) -> Vec<String> {
    let mut ids = Vec::new();
    for i in 0..6 {
        let at = (i < 3).then(|| format!("2026-04-01T09:0{i}:00Z"));
        let ev = h
            .indexer
            .capture_event(NewEvent {
                event_id: Some(format!("01HNULLPAGET{i:012}")),
                at,
                source: "test".to_owned(),
                label_skill: "email".to_owned(),
                title: format!("event {i}"),
                body: "b".to_owned(),
                ..Default::default()
            })
            .await
            .expect("capture");
        ids.push(ev.event_id);
    }
    ids
}

#[tokio::test]
async fn inbox_pagination_descends_through_null_at_ts_rows() {
    let h = fresh_harness();
    let ids = capture_mixed_null_at(&h).await;

    let seen = drain_pages(|cursor| {
        let idx = &h.indexer;
        async move {
            idx.list_inbox_page(2, cursor.as_deref(), false)
                .await
                .expect("list_inbox_page")
        }
    })
    .await;

    assert_eq!(seen.len(), ids.len(), "every event reachable: {seen:?}");
    // DESC order with NULLS LAST: dated rows newest-first, then the
    // NULL block by event_id DESC.
    let expected: Vec<String> = vec![
        ids[2].clone(),
        ids[1].clone(),
        ids[0].clone(),
        ids[5].clone(),
        ids[4].clone(),
        ids[3].clone(),
    ];
    assert_eq!(
        seen, expected,
        "DESC NULLS LAST order preserved across pages"
    );
}

#[tokio::test]
async fn history_pagination_ascends_through_null_at_ts_rows() {
    let h = fresh_harness();
    let ids = capture_mixed_null_at(&h).await;
    for id in &ids {
        h.indexer.assign_event(id, INSTANCE).await.expect("assign");
    }

    let seen = drain_pages(|cursor| {
        let idx = &h.indexer;
        async move {
            idx.list_events_page(INSTANCE, 2, cursor.as_deref(), false)
                .await
                .expect("list_events_page")
        }
    })
    .await;

    assert_eq!(seen.len(), ids.len(), "every event reachable: {seen:?}");
    // ASC order with NULLS LAST: dated rows oldest-first, then the NULL
    // block by event_id ASC.
    let expected: Vec<String> = vec![
        ids[0].clone(),
        ids[1].clone(),
        ids[2].clone(),
        ids[3].clone(),
        ids[4].clone(),
        ids[5].clone(),
    ];
    assert_eq!(
        seen, expected,
        "ASC NULLS LAST order preserved across pages"
    );
}

// --- kind: user | system, and the lineage columns ---------------------
//
// Knowledge-workbench backend, P1 (PR1). A `system` event is bookkeeping
// written by the runner or the gateway about a run — never work for a
// human or an agent. It skips the inbox: captured with a target page it is
// stored `processed` on that page at once (no `assign_event`), and the
// default list surfaces hide it unless asked (`include_system`).
// `root_event_id` / `run_id` are real indexed columns so a lineage is one
// equality read, not a JSON scan.

use escurel_index::EventKind;

fn run_event(title: &str, target: Option<&str>) -> NewEvent {
    NewEvent {
        kind: EventKind::System,
        source: "escurel-runner".to_owned(),
        label_skill: "escurel:run".to_owned(),
        instance_page_id: target.map(str::to_owned),
        title: title.to_owned(),
        body: "{}".to_owned(),
        root_event_id: Some("01HROOTEVENT00000000000000".to_owned()),
        run_id: Some("01HRUNID000000000000000000".to_owned()),
        ..Default::default()
    }
}

#[tokio::test]
async fn a_system_event_with_a_target_is_stored_processed_without_assign() {
    let h = fresh_harness();
    let stored = h
        .indexer
        .capture_event(run_event("run-started", Some(INSTANCE)))
        .await
        .unwrap();

    assert_eq!(stored.kind, EventKind::System);
    assert_eq!(stored.status, "processed", "no assign_event round-trip");
    assert_eq!(stored.instance_page_id.as_deref(), Some(INSTANCE));
    assert_eq!(
        stored.root_event_id.as_deref(),
        Some("01HROOTEVENT00000000000000")
    );
    assert_eq!(stored.run_id.as_deref(), Some("01HRUNID000000000000000000"));

    assert!(
        h.indexer.list_inbox(None).await.unwrap().is_empty(),
        "a system event is never inbox work",
    );
    // It IS on the page's history — but only for a caller that asks.
    let hidden = h
        .indexer
        .list_events_page(INSTANCE, 10, None, false)
        .await
        .unwrap();
    assert!(hidden.events.is_empty(), "hidden by default: {hidden:?}");
    let shown = h
        .indexer
        .list_events_page(INSTANCE, 10, None, true)
        .await
        .unwrap();
    assert_eq!(shown.events.len(), 1);
    assert_eq!(shown.events[0].event_id, stored.event_id);
}

#[tokio::test]
async fn a_system_event_without_a_target_stays_inbox_but_hidden_from_list_inbox() {
    let h = fresh_harness();
    let stored = h
        .indexer
        .capture_event(run_event("runner-status", None))
        .await
        .unwrap();
    assert_eq!(stored.status, "inbox", "no page to attach to yet");

    assert!(
        h.indexer.list_inbox(None).await.unwrap().is_empty(),
        "list_inbox hides system rows",
    );
    let page = h.indexer.list_inbox_page(10, None, false).await.unwrap();
    assert!(page.events.is_empty());
    let page = h.indexer.list_inbox_page(10, None, true).await.unwrap();
    assert_eq!(page.events.len(), 1);
    assert_eq!(page.events[0].kind, EventKind::System);
}

#[tokio::test]
async fn list_events_hides_system_rows_unless_include_system() {
    let h = fresh_harness();
    // One human event, assigned the ordinary way; two run events on the
    // same page.
    let user = h.indexer.capture_event(gmail_event()).await.unwrap();
    h.indexer
        .assign_event(&user.event_id, INSTANCE)
        .await
        .unwrap();
    for title in ["run-started", "run-finished"] {
        h.indexer
            .capture_event(run_event(title, Some(INSTANCE)))
            .await
            .unwrap();
    }

    let history = h.indexer.list_events(INSTANCE, None).await.unwrap();
    assert_eq!(history.len(), 1, "the human sees only the human event");
    assert_eq!(history[0].event_id, user.event_id);
    assert_eq!(history[0].kind, EventKind::User);

    let all = h
        .indexer
        .list_events_page(INSTANCE, 10, None, true)
        .await
        .unwrap();
    assert_eq!(all.events.len(), 3);
    assert_eq!(
        all.events
            .iter()
            .filter(|e| e.kind == EventKind::System)
            .count(),
        2
    );
}

/// Hardening H3: a label listing is INGESTION order and resumes by it, so
/// an event captured after a poll with an earlier `at` still follows the
/// cursor; a page's own history stays chronological.
#[tokio::test]
async fn a_label_listing_is_ingestion_ordered_and_a_backdated_event_follows_the_cursor() {
    use escurel_index::EventListFilter;
    let h = fresh_harness();
    let mut first = gmail_event();
    first.at = Some("2026-04-01T09:00:05Z".to_owned());
    first.title = "first".to_owned();
    let first = h.indexer.capture_event(first).await.unwrap();
    let page = h
        .indexer
        .list_events_filtered_page(
            &EventListFilter {
                label_skill: Some("email".to_owned()),
                ..Default::default()
            },
            true,
            10,
            None,
        )
        .await
        .unwrap();
    assert_eq!(page.events.len(), 1);
    let cursor = page.resume_cursor.expect("resume cursor");
    // Captured AFTER the poll, dated BEFORE the first event.
    let mut late = gmail_event();
    late.at = Some("2026-04-01T09:00:01Z".to_owned());
    late.title = "backdated".to_owned();
    let late = h.indexer.capture_event(late).await.unwrap();
    let next = h
        .indexer
        .list_events_filtered_page(
            &EventListFilter {
                label_skill: Some("email".to_owned()),
                ..Default::default()
            },
            true,
            10,
            Some(&cursor),
        )
        .await
        .unwrap();
    assert_eq!(
        next.events
            .iter()
            .map(|e| e.event_id.as_str())
            .collect::<Vec<_>>(),
        [late.event_id.as_str()]
    );
    let all = h
        .indexer
        .list_events_filtered_page(
            &EventListFilter {
                label_skill: Some("email".to_owned()),
                ..Default::default()
            },
            true,
            10,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        all.events
            .iter()
            .map(|e| e.title.as_str())
            .collect::<Vec<_>>(),
        ["first", "backdated"],
        "ingestion order, not `at` order"
    );
    // A page's history: assign both to a page, list by page → `at` order.
    let page_id = "markdown/instances/customer/acme.md";
    h.indexer
        .assign_event(&first.event_id, page_id)
        .await
        .unwrap();
    h.indexer
        .assign_event(&late.event_id, page_id)
        .await
        .unwrap();
    let hist = h
        .indexer
        .list_events_filtered_page(
            &EventListFilter {
                instance_page_id: Some(page_id.to_owned()),
                status: Some("processed".to_owned()),
                ..Default::default()
            },
            true,
            10,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        hist.events
            .iter()
            .map(|e| e.title.as_str())
            .collect::<Vec<_>>(),
        ["backdated", "first"],
        "a page's history stays chronological"
    );
}

/// The `seq` migration (H3) is presence-checked and checkpointed like the
/// lineage one, and numbers existing rows in the `(at_ts, event_id)` order
/// the tails used until now.
#[test]
fn reopening_a_pre_seq_events_file_backfills_seq_once() {
    use escurel_index::Migrator;
    let db_dir = TempDir::new().unwrap();
    let path = db_dir.path().join("escurel.duckdb");
    {
        let conn = Connection::open(&path).unwrap();
        Migrator::up(&conn).unwrap();
        // Rewind `events` to its pre-seq (0016) shape.
        conn.execute_batch(
            "DROP TABLE events; \
             CREATE TABLE events (\
                 event_id VARCHAR PRIMARY KEY, at_ts TIMESTAMP, \
                 source VARCHAR NOT NULL DEFAULT '', mime VARCHAR NOT NULL DEFAULT '', \
                 label_skill VARCHAR NOT NULL DEFAULT '', instance_page_id VARCHAR, \
                 status VARCHAR NOT NULL DEFAULT 'inbox', title VARCHAR NOT NULL DEFAULT '', \
                 body VARCHAR NOT NULL DEFAULT '', provenance JSON, \
                 created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP, \
                 kind VARCHAR DEFAULT 'user', root_event_id VARCHAR, run_id VARCHAR); \
             INSERT INTO events (event_id, at_ts, label_skill) VALUES \
                 ('b-later', TIMESTAMP '2026-04-01 09:00:05', 'email'), \
                 ('a-earlier', TIMESTAMP '2026-04-01 09:00:01', 'email'), \
                 ('c-undated', NULL, 'email'); \
             CHECKPOINT;",
        )
        .unwrap();
    }
    let seqs = |conn: &Connection| -> Vec<(String, Option<i64>)> {
        let mut stmt = conn
            .prepare("SELECT event_id, seq FROM events ORDER BY seq NULLS LAST, event_id")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    {
        let conn = Connection::open(&path).unwrap();
        Migrator::ensure_events_seq(&conn).unwrap();
        assert_eq!(
            seqs(&conn),
            vec![
                ("a-earlier".to_owned(), Some(1)),
                ("b-later".to_owned(), Some(2)),
                ("c-undated".to_owned(), Some(3)),
            ],
            "backfilled in (at_ts, event_id) order, undated last"
        );
        Migrator::ensure_events_seq(&conn).unwrap();
        assert_eq!(seqs(&conn).len(), 3, "idempotent");
    }
    {
        let conn =
            Connection::open(&path).expect("reopen after the migration must not replay an ALTER");
        Migrator::ensure_events_seq(&conn).unwrap();
        assert_eq!(seqs(&conn)[2], ("c-undated".to_owned(), Some(3)));
    }
    let _ = Connection::open(&path).expect("third open is clean too");
}

#[tokio::test]
async fn a_user_event_is_self_rooted_and_kind_defaults_to_user() {
    // Back-compat: every existing caller builds `NewEvent` with
    // `..Default::default()` and never names `kind`. Such an event is a
    // `user` event, and it is its OWN lineage root — so
    // `root_event_id = <its id>` finds the root and its cascade with one
    // equality, without a special case for "the root has no root".
    let h = fresh_harness();
    let stored = h.indexer.capture_event(gmail_event()).await.unwrap();
    assert_eq!(stored.kind, EventKind::User);
    assert_eq!(stored.status, "inbox");
    assert_eq!(
        stored.root_event_id.as_deref(),
        Some(stored.event_id.as_str())
    );
    assert_eq!(stored.run_id, None);
}

#[test]
fn reopening_a_pre_lineage_events_file_gains_the_columns_once() {
    // `events.created_at` is `DEFAULT CURRENT_TIMESTAMP`, so an ALTER that
    // is left in the WAL cannot be replayed by the NEXT process to open the
    // file (docs/notes/discovered/2026-09-16-alter-on-a-defaulted-table-
    // poisons-the-wal.md). The migration must be presence-checked and
    // checkpointed: this test builds a pre-lineage file, migrates it, and
    // then opens it twice more — the second plain open is the replay.
    use escurel_index::Migrator;

    let db_dir = TempDir::new().unwrap();
    let path = db_dir.path().join("escurel.duckdb");
    {
        let conn = Connection::open(&path).unwrap();
        Migrator::up(&conn).unwrap();
        // Rewind `events` to its pre-lineage shape (DuckDB refuses DROP
        // COLUMN on an indexed table, so recreate it as 0004 declared it).
        conn.execute_batch(
            "DROP TABLE events; \
             CREATE TABLE events (\
                 event_id VARCHAR PRIMARY KEY, at_ts TIMESTAMP, \
                 source VARCHAR NOT NULL DEFAULT '', mime VARCHAR NOT NULL DEFAULT '', \
                 label_skill VARCHAR NOT NULL DEFAULT '', instance_page_id VARCHAR, \
                 status VARCHAR NOT NULL DEFAULT 'inbox', title VARCHAR NOT NULL DEFAULT '', \
                 body VARCHAR NOT NULL DEFAULT '', provenance JSON, \
                 created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP); \
             CREATE INDEX events_status_at ON events (status, at_ts); \
             CREATE INDEX events_instance_at ON events (instance_page_id, at_ts); \
             CHECKPOINT;",
        )
        .unwrap();
    }
    let has_kind = |conn: &Connection| -> i64 {
        conn.query_row(
            "SELECT count(*) FROM information_schema.columns \
             WHERE table_name = 'events' AND column_name IN ('kind', 'root_event_id', 'run_id')",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    {
        let conn = Connection::open(&path).unwrap();
        assert_eq!(has_kind(&conn), 0, "fixture is pre-lineage");
        Migrator::ensure_events_lineage(&conn).unwrap();
        assert_eq!(has_kind(&conn), 3);
        // Idempotent on the same connection.
        Migrator::ensure_events_lineage(&conn).unwrap();
    }
    {
        // The replay: a fresh process opening the file after the ALTER.
        let conn =
            Connection::open(&path).expect("reopen after the migration must not replay an ALTER");
        assert_eq!(has_kind(&conn), 3);
        Migrator::ensure_events_lineage(&conn).unwrap();
        // A pre-lineage row (no kind) reads as a user event.
        conn.execute_batch(
            "INSERT INTO events (event_id, label_skill, kind) VALUES ('legacy', 'email', NULL);",
        )
        .unwrap();
        let kind: Option<String> = conn
            .query_row(
                "SELECT kind FROM events WHERE event_id = 'legacy'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(kind, None, "legacy rows carry NULL, readers COALESCE");
    }
    let _ = Connection::open(&path).expect("third open is clean too");
}
