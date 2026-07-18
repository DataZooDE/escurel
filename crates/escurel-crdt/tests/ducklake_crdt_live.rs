//! Live CRDT op-log re-homing round-trip (DuckLake PR 10, Phase B): a
//! REAL Postgres testcontainer backs the shared, writable
//! `crdt_pg.escurel_crdt_ops` / `crdt_pg.escurel_crdt_snapshots` tables
//! two INDEPENDENT [`DuckdbCrdtBackend`]s (each its own DuckDB
//! connection, mirroring writer + reader in production) attach
//! read-write onto. Mirrors `escurel-index`'s `ducklake_chat_live.rs`
//! (DuckLake PR 8) / `ducklake_events_live.rs` (DuckLake PR 9) exactly,
//! applied to [`CrdtBackend::append_op`] / [`CrdtBackend::snapshot`] /
//! [`CrdtBackend::load`] instead of the chat/events append+list surface.
//!
//! Also proves BYTE-EXACT round-tripping through the attached-Postgres
//! `BLOB` columns for op/snapshot payloads that are NOT valid UTF-8 (raw
//! Loro bytes), reading them back via a SECOND, independent connection —
//! see `docs/notes/discovered/2026-07-18-duckdb-blob-bytea-round-trip.md`
//! for the standalone empirical verification this PR ran before writing
//! any product code.
//!
//! Opt-in: gated behind the `live-ducklake` feature (needs Docker). Run
//! with `cargo test -p escurel-crdt --features live-ducklake --test
//! ducklake_crdt_live`.

#![cfg(feature = "live-ducklake")]

use std::sync::Arc;

use escurel_crdt::{CrdtBackend, DuckdbCrdtBackend, Op, Snapshot};
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ContainerAsync;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use tokio::sync::Mutex;

const TENANT: &str = "acme";

async fn live_postgres() -> (ContainerAsync<Postgres>, String) {
    let pg = Postgres::default().start().await.expect("start postgres");
    let port = pg.get_host_port_ipv4(5432).await.expect("pg port");
    let dsn = format!("host=127.0.0.1 port={port} user=postgres password=postgres dbname=postgres");
    (pg, dsn)
}

/// A fresh, standalone `DuckdbCrdtBackend` — its own in-memory DuckDB
/// connection — attached to the SAME CRDT Postgres tables. Two of these
/// in one test stand in for "writer" and "reader" without pulling in the
/// whole `escurel-server` boot path (mirrors `ducklake_events_live.rs`'s
/// `Replica` helper).
async fn replica(dsn: &str) -> DuckdbCrdtBackend {
    let conn = duckdb::Connection::open_in_memory().unwrap();
    let backend = DuckdbCrdtBackend::new(Arc::new(Mutex::new(conn)));
    backend
        .attach_shared_pg(dsn, TENANT)
        .await
        .expect("attach crdt pg");
    backend
}

/// Op bytes deliberately including `0x00` and non-UTF8 bytes — the exact
/// gotcha the BLOB/bytea verification note checked, exercised again here
/// through the real `CrdtBackend::append_op` bind path (not a raw SQL
/// literal).
fn raw_op_bytes() -> Vec<u8> {
    vec![0x00, 0x01, 0x02, 0xFE, 0xFF, 0x00, 0xAB, 0x10, 0x7F, 0x80]
}

#[tokio::test]
async fn crdt_ops_and_snapshots_round_trip_through_attached_postgres() {
    let (_pg, dsn) = live_postgres().await;
    let writer = replica(&dsn).await;
    let reader = replica(&dsn).await;

    assert!(writer.has_shared_crdt());
    assert!(reader.has_shared_crdt());

    let page_id = "page-shared";
    let op_bytes = raw_op_bytes();
    writer
        .append_op(page_id, "op-1", 1, &Op::new(op_bytes.clone()))
        .await
        .expect("append_op on writer");

    let snap_bytes = vec![0x99, 0x00, 0xFF, 0x01, 0x02, 0x03];
    writer
        .snapshot(page_id, 1, &Snapshot::new(snap_bytes.clone()))
        .await
        .expect("snapshot on writer");

    // A SEPARATE backend, separate DuckDB connection, separate attach —
    // sees the rows immediately: it's the same physical Postgres tables.
    let loaded = reader
        .load(page_id)
        .await
        .expect("load on reader")
        .expect("page has state");
    assert_eq!(
        loaded.0.as_bytes(),
        snap_bytes.as_slice(),
        "snapshot bytes must round-trip byte-exact through attached Postgres"
    );
    // `load` only returns ops strictly newer than the snapshot floor; the
    // op at hlc=1 is subsumed by the snapshot also at hlc=1, so the
    // replay list is empty here — assert via `max_hlc` and `snapshot_at`
    // instead, which read the op row directly.
    assert_eq!(reader.max_hlc(page_id).await.unwrap(), 1);
    let snap_at = reader
        .snapshot_at(page_id, 1)
        .await
        .expect("snapshot_at on reader")
        .expect("snapshot exists at hlc=1");
    assert_eq!(snap_at, snap_bytes);

    // A second op, past the snapshot floor, DOES show up in `load`'s
    // replay list — proves op-row byte fidelity through the same path.
    writer
        .append_op(page_id, "op-2", 2, &Op::new(op_bytes.clone()))
        .await
        .expect("append_op #2 on writer");
    let loaded2 = reader.load(page_id).await.unwrap().unwrap();
    assert_eq!(loaded2.1.len(), 1, "exactly the post-snapshot op replays");
    assert_eq!(
        loaded2.1[0].as_bytes(),
        op_bytes.as_slice(),
        "op bytes must round-trip byte-exact through attached Postgres, \
         including 0x00/0xFF/non-UTF8 bytes"
    );
}

#[tokio::test]
async fn compact_subsumed_ops_reclaims_bytes_across_attached_postgres() {
    let (_pg, dsn) = live_postgres().await;
    let backend = replica(&dsn).await;

    let page_id = "page-compact";
    let op_bytes = raw_op_bytes();
    backend
        .append_op(page_id, "op-1", 1, &Op::new(op_bytes.clone()))
        .await
        .unwrap();
    backend
        .append_op(page_id, "op-2", 2, &Op::new(op_bytes.clone()))
        .await
        .unwrap();
    backend
        .snapshot(page_id, 2, &Snapshot::new(vec![0x01, 0x02]))
        .await
        .unwrap();

    let (deleted, bytes) = backend.compact_subsumed_ops(page_id).await.unwrap();
    assert_eq!(
        deleted, 2,
        "both ops at hlc<=2 are subsumed by the snapshot"
    );
    assert_eq!(
        bytes,
        (op_bytes.len() * 2) as u64,
        "reclaimed bytes must match the deleted ops' total op_bytes length"
    );

    // A second compaction pass has nothing left to reclaim.
    let (deleted2, bytes2) = backend.compact_subsumed_ops(page_id).await.unwrap();
    assert_eq!(deleted2, 0);
    assert_eq!(bytes2, 0);
}

#[tokio::test]
async fn pages_with_snapshots_is_scoped_to_this_tenant() {
    let (_pg, dsn) = live_postgres().await;
    let backend = replica(&dsn).await;

    backend
        .snapshot("page-a", 1, &Snapshot::new(vec![0x01]))
        .await
        .unwrap();
    backend
        .snapshot("page-b", 1, &Snapshot::new(vec![0x02]))
        .await
        .unwrap();

    let mut pages = backend.pages_with_snapshots().await.unwrap();
    pages.sort();
    assert_eq!(pages, vec!["page-a".to_owned(), "page-b".to_owned()]);
}
