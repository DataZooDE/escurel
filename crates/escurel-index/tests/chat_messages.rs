//! Integration tests for the `chat_messages` indexer surface
//! (DataZooDE/escurel#63 — per-chat-group conversation history).
//!
//! Real DuckDB file in a `tempfile::TempDir`, real `ZeroEmbedder`
//! (768-dim), real `FsStore`. No mocks; the indexer write path runs
//! end-to-end and the inserted rows are observed via a separate
//! `Connection::open` (DuckDB second-connection-stale notes don't
//! bite us here because the indexer commits before each assertion).

use std::sync::Arc;

use duckdb::Connection;
use escurel_embed::{Embedder, ZeroEmbedder};
use escurel_index::indexer::{AppendChatMessage, ListChatMessages};
use escurel_index::read::OrderDir;
use escurel_index::{Indexer, Migrator};
use escurel_storage::{FsStore, LaneStore};
use tempfile::TempDir;

const TENANT: &str = "carl";

struct Harness {
    indexer: Indexer,
    _store_dir: TempDir,
    _db_dir: TempDir,
}

fn fresh_harness() -> Harness {
    let store_dir = TempDir::new().expect("tempdir for store");
    let db_dir = TempDir::new().expect("tempdir for db");
    let duckdb_path = db_dir.path().join("escurel.duckdb");
    let store: Arc<dyn LaneStore> = Arc::new(FsStore::new(store_dir.path().to_path_buf()));
    let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder::default());
    let conn = Connection::open(&duckdb_path).expect("open duckdb");
    Migrator::up(&conn).expect("migrate v1 schema (includes chat_messages)");
    let indexer = Indexer::new(Arc::clone(&store), embedder, conn, TENANT).expect("indexer");
    Harness {
        indexer,
        _store_dir: store_dir,
        _db_dir: db_dir,
    }
}

fn append<'a>(
    group: &'a str,
    role: &'a str,
    content: &'a str,
    ts: Option<&'a str>,
) -> AppendChatMessage<'a> {
    AppendChatMessage {
        chat_group_id: group,
        role,
        content,
        author: None,
        ts,
        metadata: None,
        msg_id: None,
        embed: true,
    }
}

#[tokio::test]
async fn append_then_list_returns_time_ordered() {
    let h = fresh_harness();

    h.indexer
        .append_chat_message(append(
            "room-1",
            "user",
            "first",
            Some("2026-05-25T10:00:00Z"),
        ))
        .await
        .expect("append #1");
    h.indexer
        .append_chat_message(append(
            "room-1",
            "assistant",
            "second",
            Some("2026-05-25T10:00:05Z"),
        ))
        .await
        .expect("append #2");
    h.indexer
        .append_chat_message(append(
            "room-1",
            "user",
            "third",
            Some("2026-05-25T10:00:10Z"),
        ))
        .await
        .expect("append #3");

    let page = h
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
        .expect("list");

    assert_eq!(page.messages.len(), 3);
    let bodies: Vec<&str> = page.messages.iter().map(|m| m.content.as_str()).collect();
    assert_eq!(bodies, vec!["first", "second", "third"]);
    assert!(
        page.next_cursor.is_none(),
        "no cursor when results fit under the limit"
    );

    // Descending = reverse order.
    let desc = h
        .indexer
        .list_chat_messages(ListChatMessages {
            chat_group_id: "room-1",
            since: None,
            until: None,
            limit: 10,
            cursor: None,
            direction: OrderDir::Desc,
        })
        .await
        .expect("list desc");
    let bodies: Vec<&str> = desc.messages.iter().map(|m| m.content.as_str()).collect();
    assert_eq!(bodies, vec!["third", "second", "first"]);
}

#[tokio::test]
async fn list_paginates_with_cursor() {
    let h = fresh_harness();

    // Seed 5 messages at distinct timestamps.
    for (i, ts) in [
        "2026-05-25T10:00:00Z",
        "2026-05-25T10:00:01Z",
        "2026-05-25T10:00:02Z",
        "2026-05-25T10:00:03Z",
        "2026-05-25T10:00:04Z",
    ]
    .iter()
    .enumerate()
    {
        let body = format!("m{i}");
        h.indexer
            .append_chat_message(append("room-1", "user", &body, Some(ts)))
            .await
            .expect("append");
    }

    // First page: limit 2, ascending.
    let p1 = h
        .indexer
        .list_chat_messages(ListChatMessages {
            chat_group_id: "room-1",
            since: None,
            until: None,
            limit: 2,
            cursor: None,
            direction: OrderDir::Asc,
        })
        .await
        .expect("p1");
    assert_eq!(
        p1.messages
            .iter()
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>(),
        vec!["m0", "m1"],
    );
    let cursor = p1
        .next_cursor
        .expect("cursor present when more rows remain");

    // Second page using the cursor.
    let p2 = h
        .indexer
        .list_chat_messages(ListChatMessages {
            chat_group_id: "room-1",
            since: None,
            until: None,
            limit: 2,
            cursor: Some(&cursor),
            direction: OrderDir::Asc,
        })
        .await
        .expect("p2");
    assert_eq!(
        p2.messages
            .iter()
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>(),
        vec!["m2", "m3"],
    );
    let cursor = p2.next_cursor.expect("cursor for the last row");

    // Third page consumes the last message; no further cursor.
    let p3 = h
        .indexer
        .list_chat_messages(ListChatMessages {
            chat_group_id: "room-1",
            since: None,
            until: None,
            limit: 2,
            cursor: Some(&cursor),
            direction: OrderDir::Asc,
        })
        .await
        .expect("p3");
    assert_eq!(
        p3.messages
            .iter()
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>(),
        vec!["m4"],
    );
    assert!(p3.next_cursor.is_none(), "no cursor when the page drains");
}

#[tokio::test]
async fn list_filters_since_until() {
    let h = fresh_harness();
    for (i, ts) in [
        "2026-05-25T10:00:00Z",
        "2026-05-25T10:00:01Z",
        "2026-05-25T10:00:02Z",
        "2026-05-25T10:00:03Z",
        "2026-05-25T10:00:04Z",
    ]
    .iter()
    .enumerate()
    {
        let body = format!("m{i}");
        h.indexer
            .append_chat_message(append("room-1", "user", &body, Some(ts)))
            .await
            .expect("append");
    }

    let page = h
        .indexer
        .list_chat_messages(ListChatMessages {
            chat_group_id: "room-1",
            since: Some("2026-05-25T10:00:01Z"),
            until: Some("2026-05-25T10:00:03Z"),
            limit: 100,
            cursor: None,
            direction: OrderDir::Asc,
        })
        .await
        .expect("list filtered");

    // since is inclusive, until is exclusive (matches the typical
    // half-open range used by retention/erasure).
    assert_eq!(
        page.messages
            .iter()
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>(),
        vec!["m1", "m2"],
        "since inclusive, until exclusive",
    );
}

#[tokio::test]
async fn list_isolates_by_chat_group_id() {
    let h = fresh_harness();
    h.indexer
        .append_chat_message(append("room-1", "user", "in 1", None))
        .await
        .expect("append room-1");
    h.indexer
        .append_chat_message(append("room-2", "user", "in 2", None))
        .await
        .expect("append room-2");

    let p1 = h
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
        .expect("list room-1");
    assert_eq!(p1.messages.len(), 1);
    assert_eq!(p1.messages[0].content, "in 1");

    let p2 = h
        .indexer
        .list_chat_messages(ListChatMessages {
            chat_group_id: "room-2",
            since: None,
            until: None,
            limit: 10,
            cursor: None,
            direction: OrderDir::Asc,
        })
        .await
        .expect("list room-2");
    assert_eq!(p2.messages.len(), 1);
    assert_eq!(p2.messages[0].content, "in 2");
}

#[tokio::test]
async fn append_with_embed_false_skips_vector() {
    let h = fresh_harness();

    let mut without = append(
        "room-1",
        "user",
        "skip embedding",
        Some("2026-05-25T10:00:00Z"),
    );
    without.embed = false;
    h.indexer
        .append_chat_message(without)
        .await
        .expect("append skip-embed");

    h.indexer
        .append_chat_message(append(
            "room-1",
            "assistant",
            "embed me",
            Some("2026-05-25T10:00:01Z"),
        ))
        .await
        .expect("append embed");

    // Read back via the indexer's own connection (avoids the
    // second-connection staleness gotcha) by listing — the API
    // surfaces the embedded flag.
    let page = h
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
        .expect("list");

    assert_eq!(page.messages.len(), 2);
    assert!(!page.messages[0].embedded, "first row was embed=false");
    assert!(page.messages[1].embedded, "second row was embed=true");
}

#[tokio::test]
async fn append_uses_supplied_msg_id() {
    let h = fresh_harness();
    let mut input = append("room-1", "user", "with id", Some("2026-05-25T10:00:00Z"));
    input.msg_id = Some("01JZZZZZZZZZZZZZZZZZZZZZZZ");
    let stored = h.indexer.append_chat_message(input).await.expect("append");
    assert_eq!(stored.msg_id, "01JZZZZZZZZZZZZZZZZZZZZZZZ");
}

#[tokio::test]
async fn append_generates_msg_id_when_absent() {
    let h = fresh_harness();
    let stored = h
        .indexer
        .append_chat_message(append(
            "room-1",
            "user",
            "auto",
            Some("2026-05-25T10:00:00Z"),
        ))
        .await
        .expect("append");
    // ULIDs are 26 chars, Crockford base32.
    assert_eq!(stored.msg_id.len(), 26, "server msg_id is a 26-char ULID");
    assert!(
        stored.msg_id.chars().all(|c| c.is_ascii_alphanumeric()),
        "ULID is ASCII alphanumeric only: {}",
        stored.msg_id,
    );
}

#[tokio::test]
async fn delete_chat_history_purges_group() {
    let h = fresh_harness();
    h.indexer
        .append_chat_message(append(
            "room-1",
            "user",
            "in 1",
            Some("2026-05-25T10:00:00Z"),
        ))
        .await
        .expect("seed");
    h.indexer
        .append_chat_message(append(
            "room-2",
            "user",
            "in 2",
            Some("2026-05-25T10:00:01Z"),
        ))
        .await
        .expect("seed");

    let deleted = h
        .indexer
        .delete_chat_history(Some("room-1"), None, None)
        .await
        .expect("delete room-1");
    assert_eq!(deleted, 1, "exactly one row removed");

    let p1 = h
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
        .expect("list room-1");
    assert!(p1.messages.is_empty(), "room-1 is empty after delete");

    let p2 = h
        .indexer
        .list_chat_messages(ListChatMessages {
            chat_group_id: "room-2",
            since: None,
            until: None,
            limit: 10,
            cursor: None,
            direction: OrderDir::Asc,
        })
        .await
        .expect("list room-2");
    assert_eq!(
        p2.messages.len(),
        1,
        "room-2 is untouched by a room-1 delete",
    );
}

#[tokio::test]
async fn delete_with_before_ts_purges_old_only() {
    let h = fresh_harness();
    for (i, ts) in [
        "2026-05-25T10:00:00Z",
        "2026-05-25T10:00:01Z",
        "2026-05-25T10:00:02Z",
        "2026-05-25T10:00:03Z",
    ]
    .iter()
    .enumerate()
    {
        let body = format!("m{i}");
        h.indexer
            .append_chat_message(append("room-1", "user", &body, Some(ts)))
            .await
            .expect("seed");
    }

    let deleted = h
        .indexer
        .delete_chat_history(Some("room-1"), Some("2026-05-25T10:00:02Z"), None)
        .await
        .expect("delete before ts");
    assert_eq!(
        deleted, 2,
        "two rows strictly before the cutoff are removed"
    );

    let page = h
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
        .expect("list survivors");
    assert_eq!(
        page.messages
            .iter()
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>(),
        vec!["m2", "m3"],
        "rows at or after the cutoff survive",
    );
}

#[tokio::test]
async fn delete_without_filters_purges_everything() {
    let h = fresh_harness();
    h.indexer
        .append_chat_message(append("room-1", "user", "a", None))
        .await
        .expect("seed");
    h.indexer
        .append_chat_message(append("room-2", "user", "b", None))
        .await
        .expect("seed");

    let deleted = h
        .indexer
        .delete_chat_history(None, None, None)
        .await
        .expect("nuke");
    assert_eq!(deleted, 2);
}

#[tokio::test]
async fn append_persists_metadata_json() {
    let h = fresh_harness();
    let meta = serde_json::json!({"source": "matrix", "thread_id": "t-42"});
    let mut input = append("room-1", "user", "with meta", Some("2026-05-25T10:00:00Z"));
    input.metadata = Some(meta.clone());
    h.indexer.append_chat_message(input).await.expect("append");

    let page = h
        .indexer
        .list_chat_messages(ListChatMessages {
            chat_group_id: "room-1",
            since: None,
            until: None,
            limit: 1,
            cursor: None,
            direction: OrderDir::Asc,
        })
        .await
        .expect("list");
    assert_eq!(page.messages.len(), 1);
    assert_eq!(page.messages[0].metadata.as_ref(), Some(&meta));
}

/// Regression guard for DuckLake PR 8 (Phase B, chat re-homing): a
/// single-file-backend `Indexer::new` construction — this test's whole
/// harness — must NOT be wired onto the shared attached-Postgres chat
/// table. `Indexer::attach_chat_pg` is the ONLY thing that flips
/// `has_shared_chat`; nothing in this file's construction path calls it,
/// so every other test above is exercising the exact same local
/// `chat_messages` table as before this PR (byte-identical behaviour).
#[tokio::test]
async fn single_file_backend_chat_unaffected() {
    let h = fresh_harness();
    assert!(
        !h.indexer.has_shared_chat(),
        "single-file boot must default to the local chat_messages table"
    );
}
