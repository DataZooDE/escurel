//! The supported postgres -> lake copy of the append surfaces (`events_copy`).
//!
//! heron-escurel keeps `escurel_events`, `escurel_drafts` and `escurel_chat_messages` in the Cloud SQL
//! catalog database; the owner moves them to the Drive-backed lake. Switching `ESCUREL_EVENTS_BACKEND` /
//! `ESCUREL_CHAT_BACKEND` alone starts EMPTY lake tables and orphans the Postgres rows (inbox items, pending
//! drafts, audit events, chat). These tests pin the copy against a REAL Postgres (the source) and a real
//! DuckLake (DuckDB-file catalog + local DATA_PATH, the destination): no mocks.
//!
//! Opt-in: behind the `live-ducklake` feature (needs Docker). Run with
//! `cargo test -p escurel-index --features live-ducklake --test suite events_copy_live`.

#![cfg(feature = "live-ducklake")]

use std::sync::Arc;

use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::drafts::NewDraft;
use escurel_index::snapshot::{
    CopyTable, EventsCopyOptions, EventsCopyReport, LakeConfig, ObjectStoreSecret,
    attach_chat_lake, attach_drafts_lake, attach_events_lake, events_copy,
};
use escurel_index::{AppendChatMessage, EventKind, Indexer, Migrator, NewEvent};
use escurel_storage::{FsStore, LaneStore};
use tempfile::TempDir;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ContainerAsync;
use testcontainers_modules::testcontainers::runners::AsyncRunner;

const TENANT: &str = "acme";
const OTHER_TENANT: &str = "globex";

struct Source {
    _pg: ContainerAsync<Postgres>,
    dsn: String,
    // Keep the seeding indexers' dirs alive for the test.
    _dirs: Vec<TempDir>,
}

async fn live_postgres() -> (ContainerAsync<Postgres>, String) {
    let pg = Postgres::default().start().await.expect("start postgres");
    let port = pg.get_host_port_ipv4(5432).await.expect("pg port");
    let dsn = format!("host=127.0.0.1 port={port} user=postgres password=postgres dbname=postgres");
    (pg, dsn)
}

/// An indexer for `tenant` whose events, drafts and chat are the shared Postgres tables.
async fn pg_replica(dsn: &str, tenant: &str, dirs: &mut Vec<TempDir>) -> Indexer {
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let indexer = Indexer::new(store, embedder, conn, tenant).unwrap();
    indexer.attach_events_pg(dsn).await.expect("attach events");
    indexer.attach_drafts_pg(dsn).await.expect("attach drafts");
    indexer.attach_chat_pg(dsn).await.expect("attach chat");
    dirs.push(store_dir);
    dirs.push(db_dir);
    indexer
}

fn event(n: u32, kind: EventKind) -> NewEvent {
    NewEvent {
        event_id: Some(format!("evt-{n:03}")),
        at: Some(format!("2026-10-01T09:{:02}:00Z", n % 60)),
        source: "gmail".to_owned(),
        mime: "message/rfc822".to_owned(),
        label_skill: if kind == EventKind::System {
            "escurel:run-progress".to_owned()
        } else {
            "email".to_owned()
        },
        title: format!("event {n}"),
        body: format!("body of event {n} \u{2014} with a non-ascii dash"),
        provenance: Some(serde_json::json!({ "extracted_by": "agt:scout-a", "n": n })),
        kind,
        root_event_id: (n > 1).then(|| "evt-001".to_owned()),
        run_id: (kind == EventKind::System).then(|| format!("run-{n}")),
        ..Default::default()
    }
}

/// Seed the source: 6 events (3 user, 3 system, some assigned), 2 drafts, 3 chat messages for `TENANT`, plus
/// rows of `OTHER_TENANT` that must never be copied.
async fn seed_source() -> Source {
    let (pg, dsn) = live_postgres().await;
    let mut dirs = Vec::new();
    let acme = pg_replica(&dsn, TENANT, &mut dirs).await;
    for n in 1..=6 {
        let kind = if n % 2 == 0 {
            EventKind::System
        } else {
            EventKind::User
        };
        acme.capture_event(event(n, kind)).await.expect("capture");
    }
    acme.assign_event("evt-003", "markdown/instances/engagement/spine.md")
        .await
        .expect("assign");
    for n in 1..=2 {
        acme.create_draft(NewDraft {
            target_page_id: format!("markdown/instances/note/n{n}.md"),
            content: format!("---\nkind: instance\nid: n{n}\n---\n# n{n}\n"),
            base_sha256: None,
            author: "agt:scout-a".to_owned(),
            event_id: Some("evt-001".to_owned()),
            changeset_id: None,
            base_version: None,
            run_id: None,
            root_event_id: None,
        })
        .await
        .expect("draft");
    }
    for (i, role) in ["user", "assistant", "user"].into_iter().enumerate() {
        acme.append_chat_message(AppendChatMessage {
            chat_group_id: "group-1",
            role,
            content: &format!("message {i}"),
            author: Some("jr"),
            ts: Some(&format!("2026-10-01T10:0{i}:00Z")),
            metadata: Some(serde_json::json!({ "i": i })),
            msg_id: Some(&format!("msg-{i}")),
            embed: false,
        })
        .await
        .expect("chat");
    }
    let globex = pg_replica(&dsn, OTHER_TENANT, &mut dirs).await;
    globex
        .capture_event(event(1, EventKind::User))
        .await
        .expect("other tenant event");
    globex
        .append_chat_message(AppendChatMessage {
            chat_group_id: "group-x",
            role: "user",
            content: "other tenant",
            author: None,
            ts: Some("2026-10-01T10:00:00Z"),
            metadata: None,
            msg_id: Some("msg-x"),
            embed: false,
        })
        .await
        .expect("other tenant chat");
    Source {
        _pg: pg,
        dsn,
        _dirs: dirs,
    }
}

struct DestLake {
    cfg: LakeConfig,
    _dir: TempDir,
}

fn dest_lake() -> DestLake {
    let dir = TempDir::new().unwrap();
    std::fs::create_dir_all(dir.path().join("data")).unwrap();
    let cfg = LakeConfig {
        catalog_dsn: dir
            .path()
            .join("catalog.ducklake")
            .to_str()
            .unwrap()
            .to_owned(),
        data_path: dir.path().join("data").to_str().unwrap().to_owned(),
        object_store: ObjectStoreSecret::None,
    };
    DestLake { cfg, _dir: dir }
}

fn opts<'a>(source: &'a Source, apply: bool, tables: &'a [CopyTable]) -> EventsCopyOptions<'a> {
    EventsCopyOptions {
        source_dsn: &source.dsn,
        tenant: TENANT,
        apply,
        tables,
    }
}

const ALL: &[CopyTable] = &[CopyTable::Events, CopyTable::Drafts, CopyTable::Chat];

fn table<'a>(r: &'a EventsCopyReport, name: &str) -> &'a escurel_index::snapshot::TableCopyReport {
    r.tables
        .iter()
        .find(|t| t.table == name)
        .unwrap_or_else(|| panic!("no report for {name}: {r:?}"))
}

/// Read the destination with plain SQL on a fresh connection.
fn dest_rows(cfg: &LakeConfig, sql: &str) -> Vec<Vec<String>> {
    let conn = Connection::open_in_memory().unwrap();
    attach_events_lake(&conn, cfg).unwrap();
    attach_drafts_lake(&conn, cfg).unwrap();
    attach_chat_lake(&conn, cfg).unwrap();
    let mut stmt = conn.prepare(sql).unwrap();
    let mut rows = stmt.query([]).unwrap();
    let mut out = Vec::new();
    while let Some(row) = rows.next().unwrap() {
        let cols = row.as_ref().column_count();
        out.push(
            (0..cols)
                .map(|i| {
                    row.get::<_, Option<String>>(i)
                        .unwrap()
                        .unwrap_or_else(|| "NULL".into())
                })
                .collect(),
        );
    }
    out
}

fn dest_count(cfg: &LakeConfig, table: &str) -> i64 {
    dest_rows(
        cfg,
        &format!("SELECT count(*)::VARCHAR FROM append_lake.{table}"),
    )[0][0]
        .parse()
        .unwrap()
}

#[tokio::test]
async fn a_dry_run_reports_counts_and_writes_nothing() {
    let source = seed_source().await;
    let lake = dest_lake();

    let report = events_copy(&lake.cfg, &opts(&source, false, ALL))
        .await
        .expect("dry run");

    assert!(!report.applied, "default is a dry run: {report:?}");
    let events = table(&report, "escurel_events");
    assert_eq!(
        events.source_rows, 6,
        "only this tenant's events: {events:?}"
    );
    assert_eq!(events.already_present, 0);
    assert_eq!(events.to_copy, 6);
    assert_eq!(events.copied, 0, "a dry run copies nothing");
    assert_eq!(table(&report, "escurel_drafts").source_rows, 2);
    assert_eq!(table(&report, "escurel_chat_messages").source_rows, 3);
    assert!(
        report.audit_event_id.is_none(),
        "a dry run writes no audit event"
    );

    // Nothing was created in the destination: not even the tables.
    let conn = Connection::open_in_memory().unwrap();
    escurel_index::snapshot::attach_lake(&conn, &lake.cfg, false).unwrap();
    let tables: i64 = conn
        .query_row(
            "SELECT count(*) FROM duckdb_tables() WHERE table_name LIKE 'escurel_%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(tables, 0, "a dry run must not create lake tables");
}

#[tokio::test]
async fn apply_copies_every_surface_with_ids_seq_and_lineage_intact() {
    let source = seed_source().await;
    let lake = dest_lake();

    let report = events_copy(&lake.cfg, &opts(&source, true, ALL))
        .await
        .expect("apply");

    assert!(report.applied);
    for (name, rows) in [
        ("escurel_events", 6),
        ("escurel_drafts", 2),
        ("escurel_chat_messages", 3),
    ] {
        let t = table(&report, name);
        assert_eq!(t.copied, rows, "{name}: {t:?}");
        assert_eq!(
            t.source_checksum, t.dest_checksum,
            "{name}: the destination rows hash like the source rows: {t:?}"
        );
    }

    // Events: ids, order (seq), kind and lineage survive.
    let rows = dest_rows(
        &lake.cfg,
        "SELECT event_id, seq::VARCHAR, kind, COALESCE(root_event_id, 'NULL'), COALESCE(run_id, 'NULL'), \
                status, title FROM append_lake.escurel_events WHERE tenant = 'acme' \
                AND label_skill <> 'escurel:events-copy' ORDER BY seq",
    );
    let ids: Vec<&str> = rows.iter().map(|r| r[0].as_str()).collect();
    assert_eq!(
        ids,
        [
            "evt-001", "evt-002", "evt-003", "evt-004", "evt-005", "evt-006"
        ]
    );
    let seqs: Vec<&str> = rows.iter().map(|r| r[1].as_str()).collect();
    assert_eq!(
        seqs,
        ["1", "2", "3", "4", "5", "6"],
        "seq is preserved, not renumbered"
    );
    assert_eq!(rows[1][2], "system");
    assert_eq!(rows[1][3], "evt-001", "root_event_id survives");
    assert_eq!(rows[1][4], "run-2", "run_id survives");
    assert_eq!(
        rows[2][5], "processed",
        "the assigned status survives (evt-003)"
    );
    // Drafts keep their ids; chat keeps msg ids and order.
    let drafts = dest_rows(
        &lake.cfg,
        "SELECT count(DISTINCT draft_id)::VARCHAR, min(event_id) FROM append_lake.escurel_drafts WHERE tenant = 'acme'",
    );
    assert_eq!(drafts[0], ["2", "evt-001"]);
    let chat = dest_rows(
        &lake.cfg,
        "SELECT msg_id, role FROM append_lake.escurel_chat_messages WHERE tenant = 'acme' ORDER BY ts",
    );
    assert_eq!(
        chat,
        [["msg-0", "user"], ["msg-1", "assistant"], ["msg-2", "user"]]
    );

    // Nothing of the other tenant arrived.
    assert_eq!(
        dest_rows(
            &lake.cfg,
            "SELECT count(*)::VARCHAR FROM append_lake.escurel_events WHERE tenant <> 'acme'"
        )[0][0],
        "0"
    );
    assert_eq!(
        dest_rows(
            &lake.cfg,
            "SELECT count(*)::VARCHAR FROM append_lake.escurel_chat_messages WHERE tenant <> 'acme'"
        )[0][0],
        "0"
    );

    // An audit event records the copy, and the lake's next seq continues after the copied ones.
    let audit_id = report.audit_event_id.clone().expect("an audit event id");
    let audit = dest_rows(
        &lake.cfg,
        &format!(
            "SELECT label_skill, kind, seq::VARCHAR, body FROM append_lake.escurel_events WHERE event_id = '{audit_id}'"
        ),
    );
    assert_eq!(audit[0][0], "escurel:events-copy");
    assert_eq!(audit[0][1], "system");
    assert_eq!(audit[0][2], "7", "the audit event takes the next seq");
    assert!(
        audit[0][3].contains("escurel_events"),
        "the counts are in the body: {audit:?}"
    );
}

#[tokio::test]
async fn a_rerun_copies_nothing_twice() {
    let source = seed_source().await;
    let lake = dest_lake();
    events_copy(&lake.cfg, &opts(&source, true, ALL))
        .await
        .expect("first");
    let before = (
        dest_count(&lake.cfg, "escurel_events"),
        dest_count(&lake.cfg, "escurel_drafts"),
        dest_count(&lake.cfg, "escurel_chat_messages"),
    );

    let again = events_copy(&lake.cfg, &opts(&source, true, ALL))
        .await
        .expect("rerun");

    for name in ["escurel_events", "escurel_drafts", "escurel_chat_messages"] {
        let t = table(&again, name);
        assert_eq!(t.copied, 0, "{name} copies nothing the second time: {t:?}");
        assert_eq!(t.already_present, t.source_rows);
    }
    assert!(
        again.audit_event_id.is_none(),
        "a run that copied nothing writes no new audit event"
    );
    let after = (
        dest_count(&lake.cfg, "escurel_events"),
        dest_count(&lake.cfg, "escurel_drafts"),
        dest_count(&lake.cfg, "escurel_chat_messages"),
    );
    assert_eq!(before, after, "no duplicates in the destination");
}

#[tokio::test]
async fn a_table_with_some_rows_present_copies_only_the_missing_ones() {
    let source = seed_source().await;
    let lake = dest_lake();
    events_copy(&lake.cfg, &opts(&source, true, ALL))
        .await
        .expect("first");
    // Two copied events vanish from the lake (an interrupted earlier run, a restore): the table is HALF there.
    {
        let conn = Connection::open_in_memory().unwrap();
        attach_events_lake(&conn, &lake.cfg).unwrap();
        conn.execute_batch(
            "DELETE FROM append_lake.escurel_events WHERE event_id IN ('evt-002', 'evt-005');",
        )
        .unwrap();
    }

    let report = events_copy(&lake.cfg, &opts(&source, true, ALL))
        .await
        .expect("top up");

    let events = table(&report, "escurel_events");
    assert_eq!(events.already_present, 4, "{events:?}");
    assert_eq!(events.copied, 2, "{events:?}");
    let ids = dest_rows(
        &lake.cfg,
        "SELECT event_id FROM append_lake.escurel_events WHERE tenant = 'acme' \
         AND label_skill <> 'escurel:events-copy' ORDER BY event_id",
    );
    let ids: Vec<&str> = ids.iter().map(|r| r[0].as_str()).collect();
    assert_eq!(
        ids,
        [
            "evt-001", "evt-002", "evt-003", "evt-004", "evt-005", "evt-006"
        ],
        "no duplicates, nothing missing"
    );
    assert_eq!(events.source_checksum, events.dest_checksum, "{events:?}");
}

#[tokio::test]
async fn a_partial_run_resumes_where_it_stopped() {
    let source = seed_source().await;
    let lake = dest_lake();
    // The run that died after the events: only the events table was copied.
    events_copy(&lake.cfg, &opts(&source, true, &[CopyTable::Events]))
        .await
        .expect("events only");
    assert_eq!(
        dest_count(&lake.cfg, "escurel_events"),
        6 + 1,
        "6 copied + the audit event"
    );

    let resumed = events_copy(&lake.cfg, &opts(&source, true, ALL))
        .await
        .expect("resume");

    assert_eq!(
        table(&resumed, "escurel_events").copied,
        0,
        "events are not copied again"
    );
    assert_eq!(table(&resumed, "escurel_drafts").copied, 2);
    assert_eq!(table(&resumed, "escurel_chat_messages").copied, 3);
}

#[tokio::test]
async fn a_row_that_differs_in_the_destination_refuses_the_apply() {
    let source = seed_source().await;
    let lake = dest_lake();
    events_copy(&lake.cfg, &opts(&source, true, ALL))
        .await
        .expect("first");
    // Someone edited a copied row in the lake: the copy must not paper over it, nor overwrite it.
    {
        let conn = Connection::open_in_memory().unwrap();
        attach_events_lake(&conn, &lake.cfg).unwrap();
        conn.execute_batch(
            "UPDATE append_lake.escurel_events SET title = 'edited in the lake' WHERE event_id = 'evt-002';",
        )
        .unwrap();
    }

    let err = events_copy(&lake.cfg, &opts(&source, true, ALL))
        .await
        .expect_err("a diverged row refuses");
    assert!(err.to_string().contains("differ"), "{err}");
    let title = dest_rows(
        &lake.cfg,
        "SELECT title FROM append_lake.escurel_events WHERE event_id = 'evt-002'",
    );
    assert_eq!(
        title[0][0], "edited in the lake",
        "the lake row is left as it was"
    );
}

#[tokio::test]
async fn the_source_is_never_modified() {
    let source = seed_source().await;
    let lake = dest_lake();
    let probe = |sql: &str| -> i64 {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("INSTALL postgres; LOAD postgres;")
            .unwrap();
        conn.execute_batch(&format!(
            "ATTACH '{}' AS s (TYPE postgres, READ_ONLY);",
            source.dsn
        ))
        .unwrap();
        conn.query_row(sql, [], |r| r.get(0)).unwrap()
    };
    let count = || {
        probe(
            "SELECT (SELECT count(*) FROM s.escurel_events) + (SELECT count(*) FROM s.escurel_drafts) \
                    + (SELECT count(*) FROM s.escurel_chat_messages)",
        )
    };
    let before = count();

    events_copy(&lake.cfg, &opts(&source, true, ALL))
        .await
        .expect("apply");

    assert_eq!(count(), before, "the source tables lose and gain no rows");
}
