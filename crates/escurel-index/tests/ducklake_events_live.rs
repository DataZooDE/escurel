//! Live events re-homing round-trip (DuckLake PR 9, Phase B): a REAL
//! Postgres testcontainer backs the shared, writable `events_pg.
//! escurel_events` table two INDEPENDENT `Indexer`s (each its own DuckDB
//! connection, mirroring writer + reader in production) attach
//! read-write onto. Mirrors `ducklake_chat_live.rs` (DuckLake PR 8)
//! exactly, applied to `capture_event` / `assign_event` / `list_inbox` /
//! `list_events` instead of `append_chat_message` / `list_chat_messages`.
//!
//! Opt-in: gated behind the `live-ducklake` feature (needs Docker). Run
//! with `cargo test -p escurel-index --features live-ducklake --test
//! ducklake_events_live`.

#![cfg(feature = "live-ducklake")]

use std::sync::Arc;

use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator, NewEvent};
use escurel_storage::{FsStore, LaneStore};
use tempfile::TempDir;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ContainerAsync;
use testcontainers_modules::testcontainers::runners::AsyncRunner;

const TENANT: &str = "acme";
const INSTANCE: &str = "markdown/instances/engagement/spine.md";

/// A fresh, standalone `Indexer` — its own DuckDB connection, its own
/// `FsStore` — attached to the SAME events Postgres. Two of these in one
/// test stand in for "writer" and "reader" without pulling in the whole
/// `escurel-server` boot path.
struct Replica {
    indexer: Indexer,
    _store_dir: TempDir,
    _db_dir: TempDir,
}

async fn replica(dsn: &str) -> Replica {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Indexer::new(store, embedder, conn, TENANT).unwrap();
    indexer
        .attach_events_pg(dsn)
        .await
        .expect("attach events pg");
    Replica {
        indexer,
        _store_dir: store_dir,
        _db_dir: db_dir,
    }
}

async fn live_postgres() -> (ContainerAsync<Postgres>, String) {
    let pg = Postgres::default().start().await.expect("start postgres");
    let port = pg.get_host_port_ipv4(5432).await.expect("pg port");
    let dsn = format!("host=127.0.0.1 port={port} user=postgres password=postgres dbname=postgres");
    (pg, dsn)
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
async fn two_replicas_share_events_via_attached_postgres() {
    let (_pg, dsn) = live_postgres().await;
    let writer = replica(&dsn).await;
    let reader = replica(&dsn).await;

    assert!(writer.indexer.has_shared_events());
    assert!(reader.indexer.has_shared_events());

    let captured = writer
        .indexer
        .capture_event(gmail_event())
        .await
        .expect("capture on writer");

    // A SEPARATE Indexer, separate DuckDB connection, separate attach —
    // sees the row immediately: it's the same physical Postgres table.
    let inbox = reader
        .indexer
        .list_inbox(None)
        .await
        .expect("list_inbox on reader");
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].event_id, captured.event_id);
    assert_eq!(inbox[0].source, "gmail");
    assert_eq!(
        inbox[0].provenance["extracted_by"], "agt:scout-a",
        "provenance round-trips through the attached-Postgres table",
    );
}

#[tokio::test]
async fn assign_event_updates_are_visible_across_replicas() {
    let (_pg, dsn) = live_postgres().await;
    let a = replica(&dsn).await;
    let b = replica(&dsn).await;

    let captured = a
        .indexer
        .capture_event(gmail_event())
        .await
        .expect("capture on A");

    // Assign on replica A...
    a.indexer
        .assign_event(&captured.event_id, INSTANCE)
        .await
        .expect("assign on A");

    // ...visible on a completely separate replica B's connection.
    let inbox_b = b.indexer.list_inbox(None).await.expect("list_inbox on B");
    assert!(
        inbox_b.is_empty(),
        "assigned event must leave the inbox for every replica: {inbox_b:?}"
    );
    let history_b = b
        .indexer
        .list_events(INSTANCE, None)
        .await
        .expect("list_events on B");
    assert_eq!(history_b.len(), 1);
    assert_eq!(history_b[0].event_id, captured.event_id);
    assert_eq!(history_b[0].status, "processed");
    assert_eq!(history_b[0].instance_page_id.as_deref(), Some(INSTANCE));
}

#[tokio::test]
async fn capture_with_explicit_event_id_is_idempotent_over_attached_postgres() {
    let (_pg, dsn) = live_postgres().await;
    let r = replica(&dsn).await;

    let step = NewEvent {
        event_id: Some("01HSTEPKEYDETERMINISTIC00".to_owned()),
        source: "escurel-runner".to_owned(),
        label_skill: "verify-vote".to_owned(),
        title: "vote".to_owned(),
        body: "first".to_owned(),
        ..Default::default()
    };
    let first = r.indexer.capture_event(step.clone()).await.unwrap();
    assert_eq!(first.body, "first");

    let second = NewEvent {
        body: "second".to_owned(),
        ..step
    };
    let again = r
        .indexer
        .capture_event(second)
        .await
        .expect("re-capturing the same event_id must not error");
    assert_eq!(
        again.body, "first",
        "first write wins; the second is a no-op — even against attached Postgres"
    );

    let inbox = r.indexer.list_inbox(None).await.unwrap();
    assert_eq!(inbox.len(), 1, "no duplicate row for the deduplicated id");
}
