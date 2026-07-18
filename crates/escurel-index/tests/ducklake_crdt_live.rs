//! Live CRDT-history re-homing round-trip (DuckLake PR 10, Phase B): a
//! REAL Postgres testcontainer backs the shared, writable
//! `crdt_pg.escurel_crdt_snapshots` table two INDEPENDENT `Indexer`s
//! (each its own DuckDB connection, mirroring writer + reader in
//! production) attach read-write onto. Mirrors `ducklake_events_live.rs`
//! (DuckLake PR 9) exactly, applied to `Indexer::seed_snapshot_history` /
//! `Indexer::list_snapshots` instead of `capture_event`/`list_events`.
//!
//! This is DELIBERATELY a separate attach from `escurel-crdt`'s own
//! `ducklake_crdt_live.rs` (which exercises `DuckdbCrdtBackend`'s
//! session-actor path) — see the field doc on `Indexer::crdt_pg_backend`
//! for why `list_snapshots` reads off the INDEXER's own connection,
//! bypassing the `CrdtBackend` trait entirely, and therefore needs its
//! own attach + its own test.
//!
//! Opt-in: gated behind the `live-ducklake` feature (needs Docker). Run
//! with `cargo test -p escurel-index --features live-ducklake --test
//! ducklake_crdt_live`.

#![cfg(feature = "live-ducklake")]

use std::sync::Arc;

use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, LaneStore};
use tempfile::TempDir;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ContainerAsync;
use testcontainers_modules::testcontainers::runners::AsyncRunner;

const TENANT: &str = "acme";
const PAGE_ID: &str = "markdown/instances/engagement/spine.md";

/// A fresh, standalone `Indexer` — its own DuckDB connection, its own
/// `FsStore` — attached to the SAME crdt-history Postgres table. Two of
/// these in one test stand in for "writer" and "reader" without pulling
/// in the whole `escurel-server` boot path.
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
    indexer.attach_crdt_pg(dsn).await.expect("attach crdt pg");
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

#[tokio::test]
async fn two_replicas_share_crdt_snapshot_history_via_attached_postgres() {
    let (_pg, dsn) = live_postgres().await;
    let writer = replica(&dsn).await;
    let reader = replica(&dsn).await;

    assert!(writer.indexer.has_shared_crdt());
    assert!(reader.indexer.has_shared_crdt());

    writer
        .indexer
        .seed_snapshot_history(
            PAGE_ID,
            &[
                ("2026-01-01T00:00:00Z", "---\nid: spine\n---\nv1\n"),
                ("2026-02-01T00:00:00Z", "---\nid: spine\n---\nv2\n"),
            ],
        )
        .await
        .expect("seed on writer");

    // A SEPARATE Indexer, separate DuckDB connection, separate attach —
    // sees the rows immediately: it's the same physical Postgres table.
    let history = reader
        .indexer
        .list_snapshots(PAGE_ID)
        .await
        .expect("list_snapshots on reader");
    assert_eq!(
        history,
        vec![
            "2026-01-01T00:00:00Z".to_owned(),
            "2026-02-01T00:00:00Z".to_owned(),
        ],
        "reader must see the writer's snapshot history through the shared table"
    );
}

#[tokio::test]
async fn crdt_snapshot_history_is_scoped_per_tenant() {
    let (_pg, dsn) = live_postgres().await;
    let a = replica(&dsn).await;

    a.indexer
        .seed_snapshot_history(
            PAGE_ID,
            &[("2026-03-01T00:00:00Z", "---\nid: x\n---\nbody\n")],
        )
        .await
        .expect("seed on A");

    // A second tenant sharing the SAME physical table must not see A's
    // history — `tenant` scopes every row, same as chat/events.
    let store_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(db_dir.path().join("escurel.duckdb")).unwrap();
    Migrator::up(&conn).unwrap();
    let other_tenant = Indexer::new(store, embedder, conn, "other-tenant").unwrap();
    other_tenant
        .attach_crdt_pg(&dsn)
        .await
        .expect("attach crdt pg for other tenant");

    let history = other_tenant
        .list_snapshots(PAGE_ID)
        .await
        .expect("list_snapshots for other tenant");
    assert!(
        history.is_empty(),
        "a different tenant must not see tenant `acme`'s snapshot history: {history:?}"
    );
}
