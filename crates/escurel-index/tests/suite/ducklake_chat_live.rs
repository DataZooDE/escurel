//! Live chat re-homing round-trip (DuckLake PR 8, Phase B): a REAL
//! Postgres testcontainer backs the shared, writable `chat_pg.
//! escurel_chat_messages` table two INDEPENDENT `Indexer`s (each its own
//! DuckDB connection, mirroring writer + reader in production) attach
//! read-write onto — the concurrency model spike 3
//! (docs/notes/discovered/2026-07-17-ducklake-spike-results.md) verified
//! ("two DuckDB processes concurrently `INSERT`ing into one table
//! through `ATTACH … (TYPE postgres)`: 500/500 rows, no lost writes").
//!
//! Opt-in: gated behind the `live-ducklake` feature (needs Docker),
//! mirroring `ducklake_publish_live.rs` / `ducklake_adopt_live.rs`. Run
//! with `cargo test -p escurel-index --features live-ducklake --test
//! ducklake_chat_live`.

#![cfg(feature = "live-ducklake")]

use std::sync::Arc;

use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::indexer::{AppendChatMessage, ListChatMessages, SearchChatMessages};
use escurel_index::read::OrderDir;
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, LaneStore};
use tempfile::TempDir;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ContainerAsync;
use testcontainers_modules::testcontainers::runners::AsyncRunner;

const TENANT: &str = "acme";

/// A fresh, standalone `Indexer` — its own DuckDB connection, its own
/// `FsStore` — attached to the SAME chat Postgres. Two of these in one
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
    indexer.attach_chat_pg(dsn).await.expect("attach chat pg");
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

fn append<'a>(
    group: &'a str,
    role: &'a str,
    content: &'a str,
    embed: bool,
) -> AppendChatMessage<'a> {
    AppendChatMessage {
        chat_group_id: group,
        role,
        content,
        author: None,
        ts: None,
        metadata: None,
        msg_id: None,
        embed,
    }
}

#[tokio::test]
async fn two_replicas_share_chat_history_via_attached_postgres() {
    let (_pg, dsn) = live_postgres().await;
    let writer = replica(&dsn).await;
    let reader = replica(&dsn).await;

    assert!(writer.indexer.has_shared_chat());
    assert!(reader.indexer.has_shared_chat());

    writer
        .indexer
        .append_chat_message(append("room-1", "user", "hello from the writer", false))
        .await
        .expect("append on writer");

    // A SEPARATE Indexer, separate DuckDB connection, separate attach —
    // sees the row immediately: it's the same physical Postgres table.
    let page = reader
        .indexer
        .list_chat_messages(ListChatMessages {
            chat_group_id: "room-1",
            since: None,
            until: None,
            limit: 10,
            cursor: None,
            direction: OrderDir::Asc,
        })
        .await
        .expect("list on reader");
    assert_eq!(page.messages.len(), 1);
    assert_eq!(page.messages[0].content, "hello from the writer");
    assert_eq!(page.messages[0].chat_group_id, "room-1");
}

#[tokio::test]
async fn chat_vector_search_over_attached_postgres_returns_relevant_message() {
    let (_pg, dsn) = live_postgres().await;
    let r = replica(&dsn).await;

    // ZeroEmbedder is content-derived-but-deterministic (see its own
    // tests) — distinct content yields distinct vectors, so a
    // self-similarity search on the exact text it was embedded from
    // ranks that row first.
    r.indexer
        .append_chat_message(append(
            "room-2",
            "user",
            "the quarterly revenue report",
            true,
        ))
        .await
        .expect("append #1");
    r.indexer
        .append_chat_message(append("room-2", "user", "a recipe for banana bread", true))
        .await
        .expect("append #2");
    r.indexer
        .append_chat_message(append(
            "room-2",
            "user",
            "notes on the revenue forecast",
            true,
        ))
        .await
        .expect("append #3");

    let hits = r
        .indexer
        .search_chat_messages(SearchChatMessages {
            chat_group_id: "room-2",
            query: "quarterly revenue report",
            limit: 2,
        })
        .await
        .expect("search over attached Postgres");

    assert_eq!(hits.len(), 2, "limit=2 caps the result set");
    assert_eq!(
        hits[0].content, "the quarterly revenue report",
        "the exact-text match must rank first: {hits:?}"
    );
}

#[tokio::test]
async fn delete_chat_history_removes_rows_for_all_replicas() {
    let (_pg, dsn) = live_postgres().await;
    let a = replica(&dsn).await;
    let b = replica(&dsn).await;

    a.indexer
        .append_chat_message(append("room-3", "user", "please forget me", false))
        .await
        .expect("append");

    // GDPR erasure via replica A's connection.
    let deleted = a
        .indexer
        .delete_chat_history(Some("room-3"), None, None)
        .await
        .expect("delete on A");
    assert_eq!(deleted, 1);

    // A FRESH attach on replica B — separate connection entirely — sees
    // zero rows: the physical table lost the row, not a local cache.
    let page = b
        .indexer
        .list_chat_messages(ListChatMessages {
            chat_group_id: "room-3",
            since: None,
            until: None,
            limit: 10,
            cursor: None,
            direction: OrderDir::Asc,
        })
        .await
        .expect("list on B");
    assert!(
        page.messages.is_empty(),
        "delete on one replica must remove the row for every replica: {page:?}"
    );
}
